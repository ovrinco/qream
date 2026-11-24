//! The controller wired to the real world inside the daemon: workers start in panes of the
//! campaign's session with their frozen arguments and their directive as first prompt; rows
//! are read and written through the store crate as `qream-controller`; judgments reach the
//! chief as file mail; time is the wall clock. Seals, job ends and agent changes are fed to
//! the controller as events; nothing here ticks with a model turn.
use crate::{
    Error, Result,
    api::{Api, StoreHandle},
    capacity::{self, Capacity, Limits},
    controller::{self, AgentInfo, Campaign, Controller, Decision, Deps, Event, Task, Timing},
    folder::Directive,
    job,
    mail::{self, Mail, Message},
    observe, profile, seal,
    states::{CampaignState, TaskWord},
    storage,
};
use parking_lot::Mutex;
use qream_term::{
    agent::{AgentKind, AgentSpec, AgentState},
    session::SessionStore,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::sync::broadcast;

/// Socket commands this module answers (CONTRACTS.md §2, engine).
pub const COMMANDS: &[&str] = &[
    "campaign.trigger",
    "controller.start",
    "controller.status",
    "deploy.dry_run",
    "run.status",
    "run.accept",
    "run.read",
    "run.send",
    "mail.send",
    "mail.broadcast",
    "mail.inbox",
    "mail.read",
    "mail.ack",
    "mail.ask",
    "mail.answer",
    "job.start",
    "job.list",
    "job.stop",
    "submit",
    "row.check",
    "gapfill",
    "profile.get",
    "profile.set",
    "profile.harnesses",
    "capacity",
    "quota",
    "fence",
    "events",
    "notify",
    "conversation.get",
    "task.directive",
];
/// The commands among them that change a record or start a process.
pub const WRITES: &[&str] = &[
    "campaign.trigger",
    "controller.start",
    "run.accept",
    "run.send",
    "mail.send",
    "mail.broadcast",
    "mail.read",
    "mail.ack",
    "mail.ask",
    "mail.answer",
    "job.start",
    "job.stop",
    "submit",
    "row.check",
    "gapfill",
    "profile.set",
    "fence",
];

type Live = Controller<Agents, Rows, ChiefMail, Clock>;
/// One campaign's controller and what the daemon has already fed it.
pub struct Runner {
    pub controller: Live,
    /// Ledger lines already published as `task` events.
    published: usize,
    /// Each worker's last agent state, to feed changes once.
    agents: BTreeMap<String, AgentState>,
    /// Job ends already fed, as `task/name`.
    jobs: BTreeSet<String>,
    /// The worker panes were closed after the campaign finished.
    closed: bool,
}

fn text<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| Error::Denied(format!("Give the {key}")))
}
fn opt<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty())
}
fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}
fn short_hash(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .take(6)
        .map(|b| format!("{b:02x}"))
        .collect()
}
/// One line: the terminal submits on a newline, so delivered text is joined.
fn one_line(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" · ")
}
/// Keys already applied, one file per kind under `controller/`.
fn seen(root: &Path, file: &str) -> Result<BTreeSet<String>> {
    let path = root.join("controller").join(file);
    if path.exists() {
        storage::read(&path)
    } else {
        Ok(BTreeSet::new())
    }
}
fn remember(root: &Path, file: &str, keys: &BTreeSet<String>) -> Result<()> {
    storage::write(&root.join("controller").join(file), keys)
}
/// A program the pane can start: an absolute path that exists or a name on `PATH`.
pub fn installed(program: &str) -> bool {
    let path = Path::new(program);
    if path.is_absolute() {
        return path.is_file();
    }
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|dir| dir.join(program).is_file()))
}
/// The harnesses installed on this machine, the only ones a profile may name.
pub fn harnesses() -> Vec<&'static str> {
    ["claude", "codex", "opencode"]
        .into_iter()
        .filter(|h| installed(h))
        .collect()
}
/// The processor's temperature from the `k10temp` (or first) hardware monitor.
pub fn celsius() -> Option<f64> {
    let dirs = std::fs::read_dir("/sys/class/hwmon").ok()?;
    let mut fallback = None;
    for dir in dirs.flatten() {
        let path = dir.path();
        let name = std::fs::read_to_string(path.join("name")).unwrap_or_default();
        let Some(milli) = std::fs::read_to_string(path.join("temp1_input"))
            .ok()
            .and_then(|t| t.trim().parse::<f64>().ok())
        else {
            continue;
        };
        if name.trim() == "k10temp" {
            return Some(milli / 1000.0);
        }
        fallback.get_or_insert(milli / 1000.0);
    }
    fallback
}
/// Machine capacity with the campaign's limits (`controller/capacity.json`, else defaults).
pub fn machine_capacity(root: &Path, active_sessions: u64) -> Result<Capacity> {
    let configured: Value =
        storage::read(&root.join("controller/capacity.json")).unwrap_or(json!({}));
    let limits = match configured.get("limits") {
        Some(limits) => serde_json::from_value(limits.clone())?,
        None => Limits {
            memory_per_task: 4 << 30,
            threads_per_task: 1,
            disk_reserve: 20 << 30,
            disk_per_task: 1 << 30,
            sessions: 16,
        },
    };
    let provider = configured
        .get("provider_slots")
        .and_then(Value::as_u64)
        .unwrap_or(16);
    capacity::calculate(
        &capacity::machine(root, active_sessions, provider)?,
        &limits,
    )
}

/// The wave number of a release key `run:release:N:hash`.
fn wave_of_key(key: &str) -> Option<u32> {
    let mut parts = key.split(':').skip_while(|p| *p != "release");
    parts.next()?;
    parts.next()?.parse().ok()
}
/// The terminal layer as the controller's `Agents`.
pub struct Agents {
    sessions: Arc<Mutex<SessionStore>>,
    session: String,
    root: PathBuf,
    env: Vec<(String, String)>,
    /// The daemon's stop engine: every worker start is an owned unit launched through it.
    launch: Option<std::sync::Weak<crate::api::Api>>,
}
impl Agents {
    fn pane(&self, task: &str) -> Option<String> {
        pane_of(&self.sessions.lock(), &self.session, task)
    }
    fn fenced(&self) -> BTreeSet<String> {
        storage::read::<BTreeSet<String>>(&self.root.join("controller/fence.json"))
            .unwrap_or_default()
    }
}
/// The open pane of a task (`QREAM_TASK`) in a session, the latest when several.
pub fn pane_of(sessions: &SessionStore, session: &str, task: &str) -> Option<String> {
    sessions
        .panes(session)
        .ok()?
        .into_iter()
        .filter(|j| !j.closed && j.spec.env.get("QREAM_TASK").map(String::as_str) == Some(task))
        .enumerate()
        .max_by_key(|(index, journal)| {
            let launch = journal.launch.as_ref();
            // Observations are cached; consult the process before preferring a live launch.
            // A zombie has exited even if its PID is still present in /proc.
            let live = journal.lost.is_none()
                && launch.and_then(|record| record.pid).is_some_and(|pid| {
                    std::fs::read_to_string(format!("/proc/{pid}/stat"))
                        .ok()
                        .and_then(|stat| {
                            stat.rsplit_once(") ")
                                .and_then(|(_, rest)| rest.chars().next())
                        })
                        .is_some_and(|state| !matches!(state, 'Z' | 'X' | 'x'))
                });
            let started = launch
                .and_then(|record| chrono::DateTime::parse_from_rfc3339(&record.started_at).ok());
            (live, started, *index)
        })
        .map(|(_, journal)| journal.spec.id)
}
fn kind(task: &Task) -> Result<AgentKind> {
    Ok(match task.kind.as_str() {
        "claude" => AgentKind::Claude,
        "codex" => AgentKind::Codex,
        "opencode" => AgentKind::Opencode,
        "shell" => AgentKind::Shell,
        other => {
            return Err(Error::Denied(format!(
                "{other} is not a harness QREAM starts"
            )));
        }
    })
}
/// The program and its frozen arguments: a harness by name, a shell task's own argv, then the
/// turn hooks QREAM adds to every Codex and Claude Code worker.
fn argv(task: &Task) -> Vec<String> {
    if task.kind == "shell" {
        task.args.clone()
    } else {
        std::iter::once(task.kind.clone())
            .chain(task.args.iter().cloned())
            .chain(hook_args(&task.kind))
            .collect()
    }
}
/// The harness reports each turn's start and end to `qream hook`, which records the task's turn
/// in its campaign: the task's word follows these events instead of a reading of its screen
/// (OD-2026-10-11-11). QREAM supplies the hook itself, so Codex's per-hook trust review is skipped
/// for this launch only.
pub fn hook_args(kind: &str) -> Vec<String> {
    const EVENTS: [(&str, u32); 5] = [
        ("SessionStart", 5),
        ("UserPromptSubmit", 5),
        ("Stop", 5),
        ("Interrupt", 3),
        ("SessionEnd", 3),
    ];
    match kind {
        "codex" => {
            let mut args = Vec::new();
            for (event, timeout) in EVENTS {
                args.push("-c".into());
                args.push(format!(
                    "hooks.{event}=[{{hooks=[{{type=\"command\",command=\"qream hook\",timeout={timeout}}}]}}]"
                ));
            }
            args.push("--dangerously-bypass-hook-trust".into());
            args
        }
        "claude" => {
            let hooks: serde_json::Map<String, Value> = [
                "SessionStart",
                "UserPromptSubmit",
                "Stop",
                "SessionEnd",
            ]
            .iter()
            .map(|event| {
                (
                    (*event).to_owned(),
                    json!([{"hooks":[{"type":"command","command":"qream hook","timeout":5}]}]),
                )
            })
            .collect();
            vec!["--settings".into(), json!({ "hooks": hooks }).to_string()]
        }
        _ => Vec::new(),
    }
}
fn directive_path(root: &Path, task: &Task) -> PathBuf {
    if task.directive.is_absolute() {
        task.directive.clone()
    } else {
        root.join(&task.directive)
    }
}
impl controller::Agents for Agents {
    fn capacity(&self) -> Result<usize> {
        let sessions = self.sessions.lock().list().iter().filter(|s| s.up).count() as u64;
        Ok(machine_capacity(&self.root, sessions.saturating_sub(1))?.slots as usize)
    }
    fn admit(&self, _: &Campaign, task: &Task) -> Result<()> {
        if self.root.join("STOP").exists() {
            return Err(Error::Denied("STOP is present: no starts".into()));
        }
        if self.fenced().contains(&task.id) {
            return Err(Error::Denied(format!("{} is fenced", task.id)));
        }
        kind(task)?;
        let program = argv(task).into_iter().next().unwrap_or_default();
        if !installed(&program) {
            return Err(Error::Denied(format!("{program} is not installed")));
        }
        if !directive_path(&self.root, task).is_file() || !task.cwd.is_dir() {
            return Err(Error::Denied(format!(
                "{} needs its directive and its working folder",
                task.id
            )));
        }
        let halt = self.root.join(format!("state/quota/{}.until", task.kind));
        if let Ok(h) = storage::read::<Value>(&halt)
            && h["reset_at"].as_i64().unwrap_or(0) as u64 > unix_now()
        {
            return Err(Error::Denied(format!(
                "Blocked (quota) until {}",
                h["printed_reset"].as_str().unwrap_or("its reset")
            )));
        }
        Ok(())
    }
    /// All of the batch or none: a failure closes the panes this call opened.
    fn start(&mut self, key: &str, campaign: &Campaign, tasks: &[Task]) -> Result<()> {
        let file = self.root.join("controller/starts.json");
        let mut starts: BTreeMap<String, BTreeMap<String, String>> =
            storage::read(&file).unwrap_or_default();
        let done = starts.get(key).cloned().unwrap_or_default();
        let mut sessions = self.sessions.lock();
        if !sessions
            .list()
            .iter()
            .any(|s| s.name == self.session && s.up)
        {
            sessions.up(&self.session, &self.root)?;
        }
        let workspace = match sessions
            .workspaces(&self.session)?
            .into_iter()
            .find(|w| w.label == "workers")
        {
            Some(w) => w.id,
            None => {
                sessions
                    .new_workspace(&self.session, "workers", &self.root, BTreeMap::new())?
                    .id
            }
        };
        let mut opened = Vec::new();
        let mut result = Ok(());
        for task in tasks.iter().filter(|t| !done.contains_key(&t.id)) {
            let mut env: BTreeMap<String, String> = self.env.iter().cloned().collect();
            env.insert("QREAM_TASK".into(), task.id.clone());
            env.insert("QREAM_CAMPAIGN".into(), campaign.name.clone());
            env.insert("QREAM_ROOT".into(), self.root.display().to_string());
            let directive = directive_path(&self.root, task);
            env.insert("QREAM_DIRECTIVE".into(), directive.display().to_string());
            env.insert("QREAM_ROLE".into(), "worker".into());
            if let Some(wave) = wave_of_key(key) {
                env.insert("QREAM_WAVE".into(), wave.to_string());
            }
            let spec = AgentSpec {
                kind: kind(task)?,
                argv: argv(task),
                env: BTreeMap::new(),
                prompt_file: Some(directive),
            };
            let pane =
                match sessions.new_pane(&self.session, &workspace, &task.cwd, env.clone(), None) {
                    Ok(pane) => pane.id,
                    Err(e) => {
                        result = Err(Error::from(e));
                        break;
                    }
                };
            opened.push(pane.clone());
            let started = match self.launch.as_ref().and_then(std::sync::Weak::upgrade) {
                // The launch registers the unit with the stop engine before the process exists.
                Some(api) => {
                    drop(sessions);
                    let launched =
                        api.launch_unit_sync(&campaign.name, &task.id, &env, &pane, spec, None);
                    sessions = self.sessions.lock();
                    launched.map(|_| pane)
                }
                None => sessions
                    .start(&pane, spec)
                    .map(|_| pane)
                    .map_err(Error::from),
            };
            match started {
                Ok(pane) => {
                    starts
                        .entry(key.into())
                        .or_default()
                        .insert(task.id.clone(), pane);
                }
                Err(e) => {
                    result = Err(e);
                    break;
                }
            }
        }
        if result.is_err() {
            for pane in &opened {
                let _ = sessions.close(pane);
            }
            starts.remove(key);
        }
        drop(sessions);
        storage::write(&file, &starts)?;
        result
    }
    /// The pause is the controller's record; ending processes is the stop path's work.
    fn stop(&mut self, _: &Campaign) -> Result<()> {
        Ok(())
    }
    fn info(&self, _: &Campaign, task: &str) -> Result<AgentInfo> {
        let owned_job = job::list(&self.root, task)?
            .into_iter()
            .find(|j| j.ended_at.is_none())
            .map(|j| j.name);
        let sessions = self.sessions.lock();
        // A lost pane (its process ended with the daemon) keeps its last observation; it is never
        // active work, so a sealed task whose pane was lost can still be judged.
        let agent = pane_of(&sessions, &self.session, task).and_then(|pane| {
            let journal = sessions.journal(&pane).ok()?;
            let mut agent = journal.agent.clone()?;
            if journal.lost.is_some() {
                agent.state = AgentState::Done;
            }
            Some(agent)
        });
        // The harness's turn hook decides whether a turn is live, as it does for the task's word.
        let hooked = std::fs::read(self.root.join("state/turns").join(format!("{task}.json")))
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            .and_then(|r| Some((r["active"].as_bool()?, r["at"].as_str()?.to_owned())));
        Ok(match agent {
            Some(a) => AgentInfo {
                active: match &hooked {
                    Some((active, at)) if at.as_str() >= a.started_at.as_str() => *active,
                    _ => a.state == AgentState::Working,
                },
                ended: a.state == AgentState::Done,
                owned_job,
                blocked: (a.state == AgentState::Blocked).then(|| {
                    a.blocked_kind
                        .map(|k| format!("{k:?}").to_lowercase())
                        .unwrap_or_else(|| "question".into())
                }),
                interactive_ready: a.interactive_ready,
                observed_at: unix_now(),
            },
            None => AgentInfo {
                owned_job,
                observed_at: unix_now(),
                ..AgentInfo::default()
            },
        })
    }
    fn deliver(
        &mut self,
        key: &str,
        _: &Campaign,
        task: &str,
        text: &str,
        _refresh: bool,
    ) -> Result<()> {
        let mut keys = seen(&self.root, "deliveries.json")?;
        if keys.contains(key) {
            return Ok(());
        }
        if self.fenced().contains(task) {
            return Err(Error::Denied(format!("{task} is fenced")));
        }
        let pane = self
            .pane(task)
            .ok_or_else(|| Error::Denied(format!("{task} has no pane")))?;
        {
            // A pane mid-turn takes the delivery when it is next idle (its pending prompt).
            let mut sessions = self.sessions.lock();
            match sessions.prompt(&pane, &one_line(text)) {
                Err(qream_term::Error::Busy) => sessions.queue_prompt(&pane, &one_line(text))?,
                other => other?,
            }
        }
        keys.insert(key.into());
        remember(&self.root, "deliveries.json", &keys)
    }
}

/// The store crate as the controller's `Rows`.
pub struct Rows {
    store: StoreHandle,
    root: PathBuf,
}
impl Rows {
    fn once(&self, key: &str, f: impl FnOnce() -> Result<()>) -> Result<()> {
        let mut keys = seen(&self.root, "row-keys.json")?;
        if keys.contains(key) {
            return Ok(());
        }
        f()?;
        keys.insert(key.into());
        remember(&self.root, "row-keys.json", &keys)
    }
    fn review(&self, key: &str, text: &str) -> Result<PathBuf> {
        let path = self
            .root
            .join("controller/reviews")
            .join(format!("{}.md", short_hash(key)));
        storage::io(
            &self.root,
            std::fs::create_dir_all(self.root.join("controller/reviews")),
        )?;
        crate::error::write(&path, text)?;
        Ok(path)
    }
}
/// The rows a task serves: its directive's `Serves rows:` line, else its assignments.
pub fn served(root: &Path, store: &StoreHandle, campaign: &str, task: &str) -> Result<Vec<String>> {
    for (_, tasks) in observe::manifests(root)? {
        if let Some((_, Some(path), _)) = tasks.iter().find(|(id, _, _)| id == task) {
            let path = if path.is_absolute() {
                path.clone()
            } else {
                root.join(path)
            };
            let rows: Vec<String> = Directive::open(&path)?.served().into_keys().collect();
            if !rows.is_empty() {
                return Ok(rows);
            }
        }
    }
    store.with(|s| {
        Ok(s.assignments(campaign)?
            .into_iter()
            .filter(|a| a.task == task)
            .map(|a| a.row_id)
            .collect())
    })
}
impl controller::Rows for Rows {
    fn served(&self, campaign: &Campaign, task: &str) -> Result<Vec<String>> {
        served(&self.root, &self.store, &campaign.name, task)
    }
    fn partial(
        &self,
        _campaign: &Campaign,
        task: &str,
    ) -> Result<std::collections::BTreeSet<String>> {
        for (_, tasks) in observe::manifests(&self.root)? {
            if let Some((_, Some(path), _)) = tasks.iter().find(|(id, _, _)| id == task) {
                let path = if path.is_absolute() {
                    path.clone()
                } else {
                    self.root.join(path)
                };
                return Ok(Directive::open(&path)?
                    .served()
                    .into_iter()
                    .filter_map(|(row, part)| part.then_some(row))
                    .collect());
            }
        }
        Ok(std::collections::BTreeSet::new())
    }
    fn status(&self, row: &str) -> Result<String> {
        self.store.with(|s| {
            Ok(s.get(row)?
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned())
        })
    }
    fn set_status(&mut self, key: &str, row: &str, status: &str) -> Result<()> {
        self.once(key, || self.store.with(|s| Ok(s.status(row, status)?)))
    }
    fn check(&mut self, key: &str, row: &str, evidence: &str) -> Result<()> {
        self.once(key, || {
            let review = self.review(key, evidence)?;
            self.store
                .with(|s| Ok(s.evidence(row, &review, &format!("{row}: checked"))?))?;
            let mut checked = seen(&self.root, "checked.json")?;
            checked.insert(row.into());
            remember(&self.root, "checked.json", &checked)
        })
    }
    fn checked(&self, row: &str) -> Result<bool> {
        Ok(seen(&self.root, "checked.json")?.contains(row))
    }
    fn closing_fault(&mut self, key: &str, row: &str, verdict: &str) -> Result<()> {
        self.once(key, || {
            let review = self.review(key, verdict)?;
            self.store
                .with(|s| Ok(s.evidence(row, &review, &format!("{row}: unproven at the close"))?))
        })
    }
}

/// Judgment mail to the chief (`harness/chief`), coalesced and sent once per key.
pub struct ChiefMail {
    root: PathBuf,
    events: broadcast::Sender<Value>,
}
impl ChiefMail {
    fn post(&self, campaign: &Campaign, id: &str, body: &str) -> Result<()> {
        Mail::new(&self.root).send(
            id,
            &format!("{}/controller", campaign.name),
            "harness/chief",
            body,
            false,
        )?;
        let _ = self.events.send(json!({"v":1,"event":"mail","at":storage::now(),"data":{"campaign":campaign.name,"to":"harness/chief","id":id}}));
        Ok(())
    }
}
impl controller::Mail for ChiefMail {
    fn judgments(&mut self, campaign: &Campaign, items: &[controller::JudgmentMail]) -> Result<()> {
        let mut keys = seen(&self.root, "mail-keys.json")?;
        let fresh: Vec<_> = items.iter().filter(|i| !keys.contains(&i.key)).collect();
        if fresh.is_empty() {
            return Ok(());
        }
        let body = fresh
            .iter()
            .map(|i| format!("{} seal {}: {}", i.task, i.seal, i.question))
            .collect::<Vec<_>>()
            .join("\n");
        let joined = fresh
            .iter()
            .map(|i| i.key.as_str())
            .collect::<Vec<_>>()
            .join(",");
        self.post(
            campaign,
            &format!("judgment-{}", short_hash(&joined)),
            &body,
        )?;
        keys.extend(fresh.iter().map(|i| i.key.clone()));
        remember(&self.root, "mail-keys.json", &keys)
    }
    fn send(&mut self, key: &str, campaign: &Campaign, text: &str) -> Result<()> {
        let mut keys = seen(&self.root, "mail-keys.json")?;
        if keys.contains(key) {
            return Ok(());
        }
        self.post(campaign, &format!("note-{}", short_hash(key)), text)?;
        keys.insert(key.into());
        remember(&self.root, "mail-keys.json", &keys)
    }
}

pub struct Clock;
impl controller::Clock for Clock {
    fn now(&self) -> u64 {
        unix_now()
    }
    fn timestamp(&self) -> String {
        storage::now()
    }
}

/// Shows a worker's mail in its pane when it waits at its prompt, never into a turn.
struct Panes<'a> {
    sessions: &'a Mutex<SessionStore>,
    session: &'a str,
}
impl mail::Agents for Panes<'_> {
    fn show_mail_if_idle(&self, recipient: &str, message: &Message) -> Result<bool> {
        let Some(task) = recipient.rsplit('/').next() else {
            return Ok(false);
        };
        let mut sessions = self.sessions.lock();
        let Some(pane) = pane_of(&sessions, self.session, task) else {
            return Ok(false);
        };
        let body = match serde_json::from_str::<job::Job>(&message.body) {
            Ok(j) => format!(
                "Your job {} ended with exit {}; its log is {}",
                j.name,
                j.exit_code.unwrap_or(-1),
                j.log.display()
            ),
            Err(_) => format!("Mail from {}: {}", message.from, message.body),
        };
        match sessions.prompt(&pane, &one_line(&body)) {
            Ok(()) => Ok(true),
            Err(qream_term::Error::Busy) => Ok(false),
            Err(e) => Err(e.into()),
        }
    }
}

/// Shows the chief its mail in the orchestrator seat's pane when it waits at its prompt.
struct ChiefPane<'a> {
    sessions: &'a Mutex<SessionStore>,
    pane: &'a str,
}
impl mail::Agents for ChiefPane<'_> {
    fn show_mail_if_idle(&self, _: &str, message: &Message) -> Result<bool> {
        let body = format!("Mail from {}: {}", message.from, message.body);
        match self.sessions.lock().prompt(self.pane, &one_line(&body)) {
            Ok(()) => Ok(true),
            Err(qream_term::Error::Busy) => Ok(false),
            Err(e) => Err(e.into()),
        }
    }
}

/// The campaign's run identity, fixed at the first controller start; a read-only daemon reads it and writes nothing.
fn run_id(root: &Path, name: &str, read_only: bool) -> Result<String> {
    let path = root.join("controller/run.json");
    if let Ok(run) = storage::read::<String>(&path) {
        return Ok(run);
    }
    if read_only {
        return Ok(format!("{name}-unstarted"));
    }
    let run = format!("{name}-{}", chrono::Local::now().format("%Y%m%dT%H%M%S"));
    if !read_only {
        storage::write(&path, &run)?;
    }
    Ok(run)
}
fn ledger_lines(root: &Path) -> Vec<Value> {
    std::fs::read_to_string(root.join("state/control.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .map(|mut v| {
            if let Some(o) = v.as_object_mut() {
                o.remove("snapshot");
            }
            v
        })
        .collect()
}

/// The command-line view of the same observation served by `states`.
/// A read never creates a controller or a run identity.
pub fn read_status(root: &Path, name: &str, state: &CampaignState) -> Result<Value> {
    Ok(json!({
        "campaign": name,
        "run": run_id(root, name, true)?,
        "word": state.word,
        "triggered": root.join("controller/TRIGGERED").exists(),
        "finished": state.finished,
        "current_wave": state.current_wave,
        "observed_at": state.observed_at,
        "age_seconds": state.age_seconds,
        "stale": state.stale,
        "stalled_minutes": state.stalled_minutes,
        "reason": state.reason,
        "counts": state.counts,
        "next": null,
        "tasks": state.tasks.iter().map(|task| {
            let mut value = json!(task);
            value["accepted"] = json!(task.word == TaskWord::Accepted);
            value
        }).collect::<Vec<_>>()
    }))
}

impl Api {
    fn controller(&self, name: &str, read_only: bool) -> Result<Live> {
        let campaign = self.registry.get(name)?;
        let root = campaign.root.clone();
        let run = run_id(&root, name, read_only)?;
        Controller::new(
            Campaign {
                name: name.into(),
                run,
                root: root.clone(),
            },
            Deps {
                agents: Agents {
                    sessions: Arc::clone(&self.sessions),
                    session: campaign.session.clone(),
                    root: root.clone(),
                    env: self.worker_env.lock().clone(),
                    launch: self.me.get().cloned(),
                },
                rows: Rows {
                    store: self.store.clone(),
                    root: root.clone(),
                },
                mail: ChiefMail {
                    root: root.clone(),
                    events: self.events.clone(),
                },
                clock: Clock,
            },
        )
    }
    /// The campaign's controller, created from its ledger on first use; never triggers.
    fn runner<'a>(
        &self,
        runners: &'a mut BTreeMap<String, Runner>,
        name: &str,
    ) -> Result<&'a mut Runner> {
        if !runners.contains_key(name) {
            let root = self.registry.get(name)?.root;
            let controller = self.controller(name, self.read_only)?;
            runners.insert(
                name.into(),
                Runner {
                    controller,
                    published: ledger_lines(&root).len(),
                    agents: BTreeMap::new(),
                    jobs: BTreeSet::new(),
                    closed: false,
                },
            );
        }
        runners
            .get_mut(name)
            .ok_or_else(|| Error::Denied("The controller did not start".into()))
    }
    /// Controllers of campaigns up whose run was triggered come back with the daemon.
    pub fn restore_controllers(&self) -> Result<()> {
        if self.read_only {
            return Ok(());
        }
        let mut runners = self.runners.lock();
        for campaign in self.registry.list()? {
            if campaign.status == "up" && campaign.root.join("controller/TRIGGERED").exists() {
                self.runner(&mut runners, &campaign.name)?;
            }
        }
        Ok(())
    }
    fn feed(&self, name: &str, event: Event) -> Result<Vec<Decision>> {
        let mut runners = self.runners.lock();
        let runner = self.runner(&mut runners, name)?;
        let decisions = runner.controller.on_event(event);
        self.settle(name, runner);
        decisions
    }
    /// Publish new ledger lines as `task` events; close worker panes once finished.
    fn settle(&self, name: &str, runner: &mut Runner) {
        let root = runner.controller.campaign.root.clone();
        let lines = ledger_lines(&root);
        for line in lines.iter().skip(runner.published) {
            let _ = self
                .events
                .send(json!({"v":1,"event":"task","at":line["at"],"data":line}));
        }
        runner.published = lines.len();
        if runner.controller.snapshot().finished && !runner.closed {
            let session = self
                .registry
                .get(name)
                .map(|c| c.session)
                .unwrap_or_default();
            let mut sessions = self.sessions.lock();
            let panes: Vec<String> = sessions
                .panes(&session)
                .unwrap_or_default()
                .into_iter()
                .filter(|j| !j.closed && j.spec.env.contains_key("QREAM_TASK"))
                .map(|j| j.spec.id)
                .collect();
            for pane in panes {
                let _ = sessions.close(&pane);
            }
            runner.closed = true;
            let _ = self.events.send(json!({"v":1,"event":"notice","at":storage::now(),"data":{"campaign":name,"message":format!("{name} is finished at its declared last wave")}}));
        }
    }
    /// Feed what happened since the last call: controller mail (seals), job ends and
    /// worker agent changes; show queued worker mail at idle prompts. Returns the campaigns
    /// whose state may have changed.
    pub fn pump_runners(&self) -> Vec<String> {
        let names: Vec<String> = self.runners.lock().keys().cloned().collect();
        let mut changed = Vec::new();
        for name in names {
            match self.pump(&name) {
                Ok(true) => changed.push(name),
                Ok(false) => {}
                Err(e) => {
                    let _ = self.events.send(json!({"v":1,"event":"fault","at":storage::now(),"data":{"campaign":name,"message":e.to_string()}}));
                }
            }
        }
        changed
    }
    fn pump(&self, name: &str) -> Result<bool> {
        let (root, session, tasks, finished) = {
            let mut runners = self.runners.lock();
            let runner = self.runner(&mut runners, name)?;
            let snapshot = runner.controller.snapshot();
            (
                runner.controller.campaign.root.clone(),
                self.registry.get(name)?.session,
                snapshot.tasks.keys().cloned().collect::<Vec<_>>(),
                snapshot.finished,
            )
        };
        if finished {
            return Ok(false);
        }
        let mut events = Vec::new();
        let mail = Mail::new(&root);
        for message in mail.inbox(&format!("{name}/controller"))? {
            if message.state == mail::Delivery::Queued && message.id.contains("-sealed-") {
                let task = message.from.rsplit('/').next().unwrap_or("").to_owned();
                events.push((
                    Some(message.id.clone()),
                    Event::Seal {
                        task,
                        hash: message.id,
                    },
                ));
            }
        }
        let current: Vec<String> = tasks;
        {
            let mut runners = self.runners.lock();
            let runner = self.runner(&mut runners, name)?;
            for task in &current {
                for j in job::list(&root, task)? {
                    let key = format!("{task}/{}", j.name);
                    // A job's start is an event too: its owner now waits on it.
                    if j.ended_at.is_none() && runner.jobs.insert(format!("{key}/start")) {
                        events.push((None, Event::AgentChange { task: task.clone() }));
                    }
                    if j.ended_at.is_some() && runner.jobs.insert(key) {
                        let _ = self.events.send(json!({"v":1,"event":"job","at":storage::now(),"data":{"campaign":name,"task":task,"job":j.name,"exit_code":j.exit_code,"ended":true}}));
                        events.push((None, Event::JobEnd { task: task.clone() }));
                    }
                }
                let state = {
                    let sessions = self.sessions.lock();
                    pane_of(&sessions, &session, task)
                        .and_then(|p| sessions.journal(&p).ok()?.agent.as_ref().map(|a| a.state))
                };
                if let Some(state) = state
                    && runner.agents.insert(task.clone(), state) != Some(state)
                    && state != AgentState::Working
                {
                    events.push((None, Event::AgentChange { task: task.clone() }));
                }
            }
        }
        let fed = !events.is_empty();
        for (id, event) in events {
            let result = self.feed(name, event);
            if let Some(id) = id {
                mail.ack(&format!("{name}/controller"), &id)?;
            }
            if let Err(e) = result {
                let _ = self.events.send(json!({"v":1,"event":"fault","at":storage::now(),"data":{"campaign":name,"message":e.to_string()}}));
            }
        }
        let panes = Panes {
            sessions: &self.sessions,
            session: &session,
        };
        for task in &current {
            mail.deliver(&format!("{name}/worker/{task}"), &panes)?;
        }
        // The chief is woken by its judgment mail, typed into the seat's pane when it waits at its prompt.
        if let Some(binding) = crate::seat::load(self, name)?.filter(|b| b.harness.is_some()) {
            mail.deliver(
                "harness/chief",
                &ChiefPane {
                    sessions: &self.sessions,
                    pane: &binding.pane,
                },
            )?;
        }
        Ok(fed)
    }
    /// Publish the campaign's state after a command that may have changed it.
    pub(crate) async fn after_engine(&self, cmd: &str, args: &Value) {
        if !WRITES.contains(&cmd) {
            return;
        }
        if let Some(name) = opt(args, "campaign").or_else(|| opt(args, "name")) {
            // A submitted or accepted task leaves its usage row on the record (RW-16-R07).
            if let (true, Some(task)) = (matches!(cmd, "submit" | "run.accept"), opt(args, "task"))
            {
                let outcome = if cmd == "submit" {
                    "submitted"
                } else {
                    "accepted"
                };
                let _ = self.records(
                    "records.usage",
                    &json!({"campaign":name,"role":"worker","task":task,"outcome":outcome}),
                );
            }
            let _ = self.campaign_state(name).await;
        }
    }
    fn root_of(&self, args: &Value) -> Result<(String, PathBuf)> {
        let name = opt(args, "campaign")
            .or_else(|| opt(args, "name"))
            .ok_or_else(|| Error::Denied("Name the campaign".into()))?;
        Ok((name.into(), self.registry.get(name)?.root))
    }
    pub fn status_of(&self, name: &str, state: &CampaignState) -> Result<Value> {
        let root = self.registry.get(name)?.root;
        let mut value = read_status(&root, name, state)?;
        let runners = self.runners.lock();
        let Some(runner) = runners.get(name) else {
            return Ok(value);
        };
        let c = &runner.controller;
        let decision = match c.decide() {
            Ok(Decision::Hold(reason)) => json!({"hold":reason}),
            Ok(d) => json!(format!("{d:?}")),
            Err(e) => json!({"refused":e.to_string()}),
        };
        value["next"] = decision;
        if let Some(tasks) = value["tasks"].as_array_mut() {
            for task in tasks {
                if let Some((id, r)) = task["task"]
                    .as_str()
                    .and_then(|id| c.snapshot().tasks.get_key_value(id))
                {
                    task["gap"] = json!(r.gap);
                    task["budget"] = json!(format!(
                        "{id}: rounds {} of 2, installs {} of 3, follow-ups {} of 6",
                        r.budget.rounds, r.budget.installs, r.budget.followups
                    ));
                    task["escalated"] = json!(r.escalated);
                }
            }
        }
        Ok(value)
    }
    /// The served rows of a task with a verdict line each, as the controller reads them.
    fn review(
        &self,
        name: &str,
        root: &Path,
        task: &str,
        failing: &BTreeSet<String>,
        gap: &str,
    ) -> Result<String> {
        let rows = served(root, &self.store, name, task)?;
        Ok(rows
            .iter()
            .map(|row| {
                if failing.is_empty() || failing.contains(row) {
                    if gap.is_empty() {
                        format!("{row}: checked")
                    } else {
                        format!("{row}: not checked, {gap}")
                    }
                } else {
                    format!("{row}: checked")
                }
            })
            .collect::<Vec<_>>()
            .join("\n"))
    }
    /// The engine commands (CONTRACTS.md §2) over the controller and the exchange modules.
    pub(crate) fn engine(&self, cmd: &str, args: &Value) -> Result<Value> {
        match cmd {
            "notify" => {
                let message = text(args, "message")?;
                let _ = self.events.send(
                    json!({"v":1,"event":"notice","at":storage::now(),"data":{"message":message}}),
                );
                return Ok(json!({"notified":message}));
            }
            "profile.harnesses" => return Ok(json!({"installed":harnesses()})),
            "conversation.get" => return self.conversation(args),
            "capacity" if opt(args, "campaign").is_none() => {
                let home = self.registry.home.clone();
                return Ok(serde_json::to_value(machine_capacity(&home, 0)?)?);
            }
            _ => {}
        }
        let (name, root) = self.root_of(args)?;
        let task = || text(args, "task");
        let to = |who: &str| match who {
            "chief" | "harness/chief" => "harness/chief".to_owned(),
            "controller" => format!("{name}/controller"),
            w if w.contains('/') => w.to_owned(),
            w => format!("{name}/worker/{w}"),
        };
        let mail = Mail::new(&root);
        Ok(match cmd {
            "campaign.trigger" => {
                let mut runners = self.runners.lock();
                let runner = self.runner(&mut runners, &name)?;
                let decisions = runner.controller.trigger();
                self.settle(&name, runner);
                json!({"campaign":name,"decisions":format!("{:?}", decisions?)})
            }
            "controller.start" => {
                self.runner(&mut self.runners.lock(), &name)?;
                json!({"campaign":name,"started":true})
            }
            // The async API adapter supplies its shared read observation to status_of.
            "controller.status" | "run.status" => {
                return Err(Error::Unimplemented("shared read observation adapter"));
            }
            "deploy.dry_run" => {
                let controller = self.controller(&name, true)?;
                let decision = controller.decide()?;
                // Before Trigger, the dry run shows what Trigger would start: wave 1.
                let wave = match &decision {
                    Decision::ReleaseWave(n) => Some(*n),
                    _ if !controller.snapshot().triggered => Some(1),
                    _ => None,
                };
                let mut plan = Vec::new();
                if let Some(n) = wave {
                    let manifest: controller::Wave =
                        storage::read(&root.join(format!("wave{n}.json")))?;
                    for t in &manifest.tasks {
                        let admitted = controller::Agents::admit(
                            &controller.deps.agents,
                            &controller.campaign,
                            t,
                        );
                        plan.push(json!({"task":t.id,"argv":argv(t),"cwd":t.cwd,"first_prompt":directive_path(&root, t),"admitted":admitted.as_ref().is_ok(),"refusal":admitted.err().map(|e| e.to_string())}));
                    }
                }
                let capacity = controller::Agents::capacity(&controller.deps.agents)?;
                json!({"campaign":name,"dry_run":true,"decision":format!("{decision:?}"),"wave":wave,"capacity":capacity,"would_start":plan})
            }
            "run.accept" => {
                let task = task()?;
                let evidence = text(args, "evidence")?;
                let by = opt(args, "by").unwrap_or("chief");
                let pending = {
                    let mut runners = self.runners.lock();
                    let s = self.runner(&mut runners, &name)?.controller.snapshot();
                    s.tasks.get(task).and_then(|r| r.seal.clone()).filter(|h| {
                        let key = format!("{task}:{h}");
                        s.delivered.contains(&key) && !s.judged.contains(&key)
                    })
                };
                let event = match pending {
                    Some(hash) => Event::Judgment {
                        task: task.into(),
                        hash,
                        review: self.review(&name, &root, task, &BTreeSet::new(), "")?,
                        timing: Timing::Now,
                        reason: evidence.into(),
                    },
                    None => Event::Accept {
                        task: task.into(),
                        evidence: evidence.into(),
                        by: by.into(),
                    },
                };
                json!({"task":task,"decisions":format!("{:?}", self.feed(&name, event)?)})
            }
            "gapfill" => {
                let task = task()?;
                let reason = text(args, "reason")?;
                let timing = match opt(args, "when").unwrap_or("now") {
                    "now" => Timing::Now,
                    "after" => Timing::After,
                    _ => return Err(Error::Denied("Gap fill is now or after the wave".into())),
                };
                let failing: BTreeSet<String> = opt(args, "rows")
                    .map(|r| r.split(',').map(str::to_owned).collect())
                    .unwrap_or_default();
                let hash = {
                    let mut runners = self.runners.lock();
                    let s = self.runner(&mut runners, &name)?.controller.snapshot();
                    s.tasks
                        .get(task)
                        .and_then(|r| r.seal.clone())
                        .ok_or_else(|| Error::Denied(format!("{task} has no seal to judge")))?
                };
                let review = self.review(&name, &root, task, &failing, reason)?;
                let decisions = self.feed(
                    &name,
                    Event::Judgment {
                        task: task.into(),
                        hash,
                        review,
                        timing,
                        reason: reason.into(),
                    },
                )?;
                json!({"task":task,"decisions":format!("{decisions:?}")})
            }
            "row.check" => {
                let id = text(args, "id")?;
                let evidence = text(args, "evidence")?;
                if args.get("closing").and_then(Value::as_bool) == Some(true) {
                    let path =
                        root.join(format!("controller/reviews/closing-{}.md", short_hash(id)));
                    storage::io(
                        &root,
                        std::fs::create_dir_all(root.join("controller/reviews")),
                    )?;
                    crate::error::write(&path, evidence)?;
                    self.with_store(|s| {
                        s.evidence(id, &path, &format!("{id}: verified by a non-author"))?;
                        Ok(s.status(id, "verified")?)
                    })?;
                    json!({"row":id,"status":"verified"})
                } else {
                    let decisions = self.feed(
                        &name,
                        Event::RowChecked {
                            row: id.into(),
                            evidence: evidence.into(),
                        },
                    )?;
                    json!({"row":id,"checked":true,"decisions":format!("{decisions:?}")})
                }
            }
            "submit" => {
                let task = task()?;
                let report = PathBuf::from(text(args, "report")?);
                if args.get("seal").and_then(Value::as_bool) == Some(true) {
                    let candidate = opt(args, "candidate").map(PathBuf::from);
                    seal::seal(&root, task, candidate.as_deref(), &report)?;
                }
                let id = seal::submit(&root, &name, task, &report)?;
                let fed = if self.runners.lock().contains_key(&name) {
                    self.pump(&name)?
                } else {
                    false
                };
                json!({"submitted":id,"task":task,"controller":fed})
            }
            "run.read" => {
                let task = task()?;
                let lines = args.get("lines").and_then(Value::as_u64).unwrap_or(50) as usize;
                let session = self.registry.get(&name)?.session;
                let pane = pane_of(&self.sessions.lock(), &session, task)
                    .ok_or_else(|| Error::Denied(format!("{task} has no pane")))?;
                let text = self.live.get(&pane)?.text(lines);
                json!({"task":task,"pane":pane,"text":text})
            }
            "run.send" | "mail.send" | "mail.ask" => {
                let recipient = if cmd == "run.send" {
                    to(task()?)
                } else {
                    to(text(args, "to")?)
                };
                let body = match opt(args, "file") {
                    Some(file) => crate::error::read(Path::new(file))?,
                    None => text(args, "body")
                        .or_else(|_| text(args, "text"))?
                        .to_owned(),
                };
                let id = opt(args, "id").map_or_else(
                    || {
                        format!(
                            "m-{}",
                            short_hash(&format!("{recipient}{body}{}", storage::now()))
                        )
                    },
                    str::to_owned,
                );
                let from = opt(args, "from").map_or_else(|| format!("{name}/controller"), &to);
                let message = mail.send(&id, &from, &recipient, &body, cmd == "mail.ask")?;
                let _ = self.events.send(json!({"v":1,"event":"mail","at":storage::now(),"data":{"campaign":name,"to":recipient,"id":id}}));
                serde_json::to_value(message)?
            }
            "mail.broadcast" => {
                let body = text(args, "body")?;
                let recipients: Vec<String> = {
                    let mut runners = self.runners.lock();
                    self.runner(&mut runners, &name)?
                        .controller
                        .snapshot()
                        .tasks
                        .keys()
                        .map(|t| to(t))
                        .collect()
                };
                let id = opt(args, "id").map_or_else(
                    || format!("b-{}", short_hash(&format!("{body}{}", storage::now()))),
                    str::to_owned,
                );
                serde_json::to_value(mail.broadcast(
                    &id,
                    &format!("{name}/controller"),
                    &recipients,
                    body,
                )?)?
            }
            "mail.inbox" => serde_json::to_value(mail.inbox(&to(text(args, "to")?))?)?,
            "mail.read" => {
                serde_json::to_value(mail.read(&to(text(args, "to")?), text(args, "id")?)?)?
            }
            "mail.ack" => {
                serde_json::to_value(mail.ack(&to(text(args, "to")?), text(args, "id")?)?)?
            }
            "mail.answer" => {
                let recipient = to(text(args, "to")?);
                let id = text(args, "id")?;
                let message = mail.answer(&recipient, id, text(args, "body")?)?;
                let _ = self.events.send(json!({"v":1,"event":"mail","at":storage::now(),"data":{"campaign":name,"to":recipient,"id":id,"answered":true}}));
                serde_json::to_value(message)?
            }
            "job.start" => {
                let task = task()?;
                let command: Vec<String> = args
                    .get("command")
                    .and_then(Value::as_array)
                    .map(|c| {
                        c.iter()
                            .filter_map(|w| w.as_str().map(str::to_owned))
                            .collect()
                    })
                    .unwrap_or_default();
                let cwd = opt(args, "cwd").map_or_else(|| root.clone(), PathBuf::from);
                let monitor = std::env::current_exe()
                    .map_err(|e| Error::io("the daemon's executable", e))?
                    .with_file_name("qream-job-monitor");
                let capacity = machine_capacity(&root, 0)?;
                let memory_max = args
                    .get("memory_max")
                    .and_then(Value::as_u64)
                    .unwrap_or(capacity.memory_per_task);
                let started = job::start(&job::Start {
                    root: &root,
                    campaign: &name,
                    task,
                    name: text(args, "name")?,
                    cwd: &cwd,
                    command: &command,
                    monitor: &monitor,
                    capacity: &capacity,
                    celsius: celsius(),
                    memory_max,
                    cpu_quota_percent: 100,
                })?;
                let _ = self.events.send(json!({"v":1,"event":"job","at":storage::now(),"data":{"campaign":name,"task":task,"job":started.name,"ended":false}}));
                serde_json::to_value(started)?
            }
            "job.list" => match opt(args, "task") {
                Some(task) => serde_json::to_value(job::list(&root, task)?)?,
                None => {
                    let mut all = Vec::new();
                    for (_, tasks) in observe::waves(&root)? {
                        for task in tasks {
                            all.extend(job::list(&root, &task)?);
                        }
                    }
                    serde_json::to_value(all)?
                }
            },
            "job.stop" => {
                job::stop(&root, task()?, text(args, "name")?)?;
                json!({"stopped":text(args, "name")?})
            }
            "profile.get" => {
                let profiles = profile::get(&root).ok();
                // The window's Settings reads each role's profile at the top level (T24).
                let role = |r: &str| match &profiles {
                    Some(p) => json!(if r == "worker" {
                        &p.worker
                    } else {
                        &p.orchestrator
                    }),
                    None => Value::Null,
                };
                json!({"profiles":profiles,"installed":harnesses(),"orchestrator":role("orchestrator"),"worker":role("worker"),"kinds":["claude","codex","opencode"],"efforts":["low","medium","high","xhigh"]})
            }
            "profile.set" => self.profile_set(&root, args)?,
            "capacity" => serde_json::to_value(machine_capacity(&root, 0)?)?,
            "quota" => {
                let kind = text(args, "kind")?;
                storage::token(kind)?;
                let halt: Option<Value> =
                    storage::read(&root.join(format!("state/quota/{kind}.until"))).ok();
                let blocked = halt
                    .as_ref()
                    .is_some_and(|h| h["reset_at"].as_i64().unwrap_or(0) as u64 > unix_now());
                json!({"kind":kind,"blocked":blocked,"word":if blocked {"Blocked (quota)"} else {"open"},"halt":halt})
            }
            "fence" => {
                let path = root.join("controller/fence.json");
                let mut fenced: BTreeSet<String> = storage::read(&path).unwrap_or_default();
                if let Some(task) = opt(args, "task") {
                    if args.get("release").and_then(Value::as_bool) == Some(true) {
                        fenced.remove(task);
                    } else {
                        fenced.insert(task.into());
                    }
                    storage::write(&path, &fenced)?;
                }
                json!({"campaign":name,"fenced":fenced})
            }
            "task.directive" => {
                let task = task()?;
                let path = observe::manifests(&root)?
                    .into_values()
                    .flatten()
                    .find(|(id, _, _)| id == task)
                    .and_then(|(_, p, _)| p)
                    .map(|p| if p.is_absolute() { p } else { root.join(p) })
                    .ok_or_else(|| Error::Denied(format!("{task} has no directive")))?;
                json!({"task":task,"path":path,"markdown":crate::error::read(&path)?})
            }
            "events" => {
                let since = args.get("since").and_then(Value::as_u64).unwrap_or(0) as usize;
                let lines = ledger_lines(&root);
                json!({"campaign":name,"count":lines.len(),"events":lines.into_iter().skip(since).collect::<Vec<_>>()})
            }
            _ => return Err(Error::Unimplemented("wave 3")),
        })
    }
    /// Profiles per campaign and per wave, only for installed harnesses; a wave's choice
    /// lands in its manifest's arguments unless the wave is already pinned.
    fn profile_set(&self, root: &Path, args: &Value) -> Result<Value> {
        // The window sends `{role, kind, model, effort}`; the command line `{worker|orchestrator}`.
        let mut args = args.clone();
        if let (Some(role), Some(kind), Some(model), Some(effort)) = (
            opt(&args, "role").map(str::to_owned),
            opt(&args, "kind").map(str::to_owned),
            opt(&args, "model").map(str::to_owned),
            opt(&args, "effort").map(str::to_owned),
        ) {
            if !matches!(role.as_str(), "worker" | "orchestrator") {
                return Err(Error::Denied(
                    "The role is the orchestrator or the workers".into(),
                ));
            }
            args[role] = json!(format!("{kind}/{model}/{effort}"));
        }
        let args = &args;
        let installed = harnesses();
        let parse = |key: &str| -> Result<Option<profile::Profile>> {
            let Some(spec) = opt(args, key) else {
                return Ok(None);
            };
            let parts: Vec<&str> = spec.split('/').collect();
            let [kind, model, effort] = parts[..] else {
                return Err(Error::Denied(format!(
                    "Give the {key} as harness/model/effort"
                )));
            };
            if !installed.contains(&kind) {
                return Err(Error::Denied(format!(
                    "{kind} is not installed on this machine"
                )));
            }
            Ok(Some(profile::Profile {
                kind: kind.into(),
                model: model.into(),
                effort: effort.into(),
            }))
        };
        let mut profiles = profile::get(root).unwrap_or(profile::Profiles {
            orchestrator: profile::Profile {
                kind: "claude".into(),
                model: "claude-opus-5-5".into(),
                effort: "low".into(),
            },
            worker: profile::Profile {
                kind: "claude".into(),
                model: "claude-opus-5-5".into(),
                effort: "low".into(),
            },
            waves: BTreeMap::new(),
        });
        if let Some(p) = parse("orchestrator")? {
            profiles.orchestrator = p;
        }
        if let Some(p) = parse("worker")? {
            profiles.worker = p;
        }
        let wave = args.get("wave").and_then(Value::as_u64).map(|n| n as u32);
        let wave_profile = parse("profile")?;
        if let (Some(n), Some(p)) = (wave, &wave_profile) {
            profiles.waves.insert(n, p.clone());
        }
        profile::set(root, &profiles)?;
        let mut manifest = Value::Null;
        if let (Some(n), Some(p)) = (wave, wave_profile) {
            if root
                .join(format!("controller/wave{n}.fingerprint"))
                .exists()
            {
                return Err(Error::Denied(format!(
                    "Wave {n} is already released; its profile is pinned"
                )));
            }
            let path = root.join(format!("wave{n}.json"));
            let mut m: Value = storage::read(&path)?;
            if let Some(tasks) = m["tasks"].as_array_mut() {
                for t in tasks {
                    let cwd = PathBuf::from(t["cwd"].as_str().unwrap_or("."));
                    t["kind"] = json!(p.kind);
                    t["args"] = json!(profile::args_for_cwd(&p.kind, &p.model, &p.effort, &cwd)?);
                }
            }
            storage::write(&path, &m)?;
            manifest = m;
        }
        Ok(json!({"profiles":profiles,"manifest":manifest,"installed":installed}))
    }
    fn conversation(&self, args: &Value) -> Result<Value> {
        let file = match opt(args, "file") {
            Some(file) => PathBuf::from(file),
            None => {
                let pane = text(args, "pane")?;
                let sessions = self.sessions.lock();
                let journal = sessions.journal(pane)?;
                let launch = journal
                    .launch
                    .as_ref()
                    .ok_or_else(|| Error::Denied("The pane runs no harness".into()))?;
                let home = std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .ok_or_else(|| Error::Denied("The user home is unavailable".into()))?;
                crate::conversation::find(
                    &home,
                    launch.spec.kind,
                    &journal.spec.cwd,
                    &launch.started_at,
                    launch.conversation_id.as_deref(),
                )?
                .ok_or_else(|| Error::Denied("The harness has written no session yet".into()))?
            }
        };
        let turns = crate::conversation::turns(&file)?;
        Ok(json!({"turns":turns}))
    }
}
#[cfg(test)]
mod run_id_tests {
    #[test]
    fn a_read_only_daemon_writes_no_run_identity() {
        let dir = tempfile::tempdir().unwrap();
        let run = super::run_id(dir.path(), "record", true).unwrap();
        assert!(run.starts_with("record-"));
        assert!(!dir.path().join("controller").exists());
        let kept = super::run_id(dir.path(), "record", false).unwrap();
        assert_eq!(super::run_id(dir.path(), "record", true).unwrap(), kept);
    }
}
