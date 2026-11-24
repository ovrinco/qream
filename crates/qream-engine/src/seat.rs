//! The orchestrator's seat: a pane in the chief workspace of a campaign's harness session,
//! opened as a plain shell, then invoked with an installed harness, a model, an effort and,
//! for Claude Code and Codex, a session to resume. The binding is kept at
//! `<home>/seats/<campaign>.json`; the column reads the bound conversation in words.
use crate::{Error, Result, api::Api, storage};
use qream_term::agent::{AgentKind, AgentSpec, AgentState};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, Read},
    path::{Path, PathBuf},
    time::SystemTime,
};

/// Turns sent to the column at most; older ones stay in the session's own file.
const TURN_LIMIT: usize = 400;
const TURN_CHARS: usize = 20_000;
/// Bytes read from a session file to find its first line.
const HEAD_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Binding {
    pub campaign: String,
    pub session: String,
    pub workspace: String,
    pub pane: String,
    pub cwd: PathBuf,
    pub harness: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    /// The native conversation the seat runs: resumed, or started with a known id.
    pub conversation: Option<String>,
    /// open, invoked, paused or stopped.
    pub state: String,
    /// The process tree last seen by a stop, kept for `leftovers`.
    #[serde(default)]
    pub tree: Vec<u32>,
    pub at: String,
}

struct Harness {
    kind: &'static str,
    words: &'static str,
    models: &'static [&'static str],
    efforts: &'static [&'static str],
    resumes: bool,
}
const HARNESSES: &[Harness] = &[
    Harness {
        kind: "claude",
        words: "Claude Code",
        models: &[
            "claude-opus-5-5",
            "claude-fable-5-1",
            "claude-sonnet-5-5",
            "claude-haiku-5-5",
        ],
        efforts: &["low", "medium", "high", "xhigh", "max"],
        resumes: true,
    },
    Harness {
        kind: "codex",
        words: "Codex",
        models: &["gpt-6.1-sol", "gpt-6-luna", "gpt-6-astra"],
        efforts: &["none", "low", "medium", "high", "xhigh"],
        resumes: true,
    },
    Harness {
        kind: "opencode",
        words: "OpenCode",
        models: &[],
        efforts: &[],
        resumes: false,
    },
];

fn s<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| Error::Denied(format!("Give the {key}")))
}
fn opt<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
}
fn now() -> String {
    chrono::Local::now().to_rfc3339()
}
fn home() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| Error::Denied("The user home is unavailable".into()))
}
fn installed(program: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths).any(|dir| {
            fs::metadata(dir.join(program)).is_ok_and(|m| {
                use std::os::unix::fs::PermissionsExt;
                m.is_file() && m.permissions().mode() & 0o111 != 0
            })
        })
    })
}
fn harness(kind: &str) -> Result<&'static Harness> {
    HARNESSES
        .iter()
        .find(|h| h.kind == kind)
        .ok_or_else(|| Error::Denied("The harness is Claude Code, Codex or OpenCode".into()))
}
fn binding_path(api: &Api, campaign: &str) -> PathBuf {
    api.registry
        .home
        .join("seats")
        .join(format!("{campaign}.json"))
}
pub(crate) fn load(api: &Api, campaign: &str) -> Result<Option<Binding>> {
    match api.registry.get(campaign) {
        Ok(_) => {}
        Err(Error::Io { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => {
            return Ok(None);
        }
        Err(e) => return Err(e),
    }
    let path = binding_path(api, campaign);
    if !path.exists() {
        return Ok(None);
    }
    let binding: Binding = storage::read(&path)?;
    Ok((binding.campaign == campaign).then_some(binding))
}
fn save(api: &Api, binding: &Binding) -> Result<()> {
    storage::write(&binding_path(api, &binding.campaign), binding)
}

/// A model of one family on the other's harness is refused before anything starts.
pub fn check_route(kind: &str, model: &str) -> Result<()> {
    let claude_model = model.starts_with("claude")
        || ["opus", "sonnet", "haiku", "fable"]
            .iter()
            .any(|m| model.eq_ignore_ascii_case(m));
    match kind {
        "codex" if claude_model => Err(Error::Denied(
            "Codex cannot run a Claude model; choose Claude Code for it".into(),
        )),
        "claude" if !claude_model => Err(Error::Denied(
            "Claude Code runs Claude models only; choose Codex for this model".into(),
        )),
        _ => Ok(()),
    }
}

/// The harness's own command line for a model, an effort and an optional resumed session.
pub fn argv(
    kind: &str,
    model: Option<&str>,
    effort: Option<&str>,
    resume: Option<&str>,
    fresh_id: Option<&str>,
) -> Vec<String> {
    let mut argv = vec![kind.to_owned()];
    let mut push = |items: &[&str]| argv.extend(items.iter().map(|w| (*w).to_owned()));
    match kind {
        "claude" => {
            match (resume, fresh_id) {
                (Some(id), _) => push(&["--resume", id]),
                (None, Some(id)) => push(&["--session-id", id]),
                _ => {}
            }
            if let Some(m) = model {
                push(&["--model", m]);
            }
            if let Some(e) = effort {
                push(&["--effort", e]);
            }
        }
        "codex" => {
            if let Some(id) = resume {
                push(&["resume", id]);
            }
            if let Some(m) = model {
                push(&["-m", m]);
            }
            if let Some(e) = effort {
                let effort = format!("model_reasoning_effort=\"{e}\"");
                push(&["-c", &effort]);
            }
        }
        _ => {
            if let Some(m) = model {
                push(&["--model", m]);
            }
        }
    }
    argv
}

/// The seat's pane, its root and every descendant, from procfs.
fn tree(api: &Api, pane: &str) -> Vec<u32> {
    api.live
        .get(pane)
        .ok()
        .and_then(|p| p.child_pids().ok())
        .unwrap_or_default()
}
fn alive(pid: u32) -> bool {
    // A zombie has left its work; only a running or sleeping process is a leftover.
    fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|stat| {
            stat.rsplit_once(')')
                .map(|(_, rest)| rest.trim_start().chars().next())
        })
        .flatten()
        .is_some_and(|state| state != 'Z' && state != 'X')
}
/// An id held by another process's command line means another terminal resumes it.
fn open_elsewhere(id: &str) -> bool {
    let Ok(entries) = fs::read_dir("/proc") else {
        return false;
    };
    let me = std::process::id().to_string();
    entries.flatten().any(|entry| {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        name.bytes().all(|b| b.is_ascii_digit())
            && name != me
            && fs::read(entry.path().join("cmdline"))
                .is_ok_and(|cmd| cmd.split(|b| *b == 0).any(|arg| arg == id.as_bytes()))
    })
}
fn fresh_uuid() -> Result<String> {
    let path = "/proc/sys/kernel/random/uuid";
    Ok(fs::read_to_string(path)
        .map_err(|e| Error::io(path, e))?
        .trim()
        .to_owned())
}

/// The state word a person reads in the column's header.
fn word(api: &Api, binding: Option<&Binding>) -> String {
    let Some(b) = binding else {
        return "Empty".into();
    };
    match b.state.as_str() {
        "stopped" => return "Stopped".into(),
        "paused" => return "Paused".into(),
        _ => {}
    }
    let sessions = api.sessions.lock();
    let Ok(journal) = sessions.journal(&b.pane) else {
        return "Stopped".into();
    };
    if journal.closed {
        return "Stopped".into();
    }
    if journal.launch.is_none() {
        return "Shell".into();
    }
    match journal.agent.as_ref() {
        None => "Starting".into(),
        Some(info) => match info.state {
            AgentState::Working => "Working".into(),
            AgentState::Idle => "Idle".into(),
            AgentState::Done => "Stopped".into(),
            AgentState::Unknown => "Running".into(),
            AgentState::Blocked => format!(
                "Blocked ({})",
                match info.blocked_kind {
                    Some(qream_term::agent::BlockedKind::Quota) => "quota",
                    Some(qream_term::agent::BlockedKind::Account) => "account",
                    _ => "question",
                }
            ),
        },
    }
}

fn view(api: &Api, campaign: &str, binding: Option<&Binding>) -> Value {
    let harness_words = binding
        .and_then(|b| b.harness.as_deref())
        .and_then(|k| harness(k).ok())
        .map(|h| h.words);
    let live_tree = binding
        .filter(|b| b.state != "stopped")
        .map(|b| tree(api, &b.pane))
        .unwrap_or_default();
    json!({
        "campaign": campaign,
        "word": word(api, binding),
        "binding": binding,
        "harness_words": harness_words,
        "tree": live_tree,
        "observed_at": now(),
        "wait": if binding.is_some_and(|b| b.harness.is_some() && seat_is_open(api, b)) {
            None
        } else {
            Some("waiting for a chief")
        },
    })
}

fn campaign_root(api: &Api, campaign: &str) -> Result<PathBuf> {
    Ok(api.registry.get(campaign)?.root)
}

/// The chief workspace of `qream-<campaign>-harness`, made when missing.
fn chief(api: &Api, campaign: &str, cwd: &Path) -> Result<(String, String)> {
    let session = format!("qream-{campaign}-harness");
    let mut sessions = api.sessions.lock();
    let up = sessions.list().iter().any(|s| s.name == session && s.up);
    if !up {
        sessions.up(&session, cwd)?;
    }
    let existing = sessions
        .workspaces(&session)?
        .into_iter()
        .find(|w| w.label == "chief");
    let workspace = match existing {
        Some(w) => w.id,
        None => {
            sessions
                .new_workspace(&session, "chief", cwd, Default::default())?
                .id
        }
    };
    Ok((session, workspace))
}

fn open_pane(api: &Api, campaign: &str, cwd: &Path) -> Result<Binding> {
    let (session, workspace) = chief(api, campaign, cwd)?;
    let spec = api
        .sessions
        .lock()
        .new_pane(&session, &workspace, cwd, Default::default(), None)?;
    Ok(Binding {
        campaign: campaign.into(),
        session,
        workspace,
        pane: spec.id,
        cwd: cwd.into(),
        harness: None,
        model: None,
        effort: None,
        conversation: None,
        state: "open".into(),
        tree: Vec::new(),
        at: now(),
    })
}

fn seat_is_open(api: &Api, binding: &Binding) -> bool {
    binding.state != "stopped"
        && api
            .sessions
            .lock()
            .journal(&binding.pane)
            .is_ok_and(|j| !j.closed)
        && api.live.get(&binding.pane).is_ok()
}

/// `seat.open {name}`: a plain shell in the seat, or the open seat as it is.
fn open(api: &Api, campaign: &str) -> Result<Value> {
    let root = campaign_root(api, campaign)?;
    if let Some(binding) = load(api, campaign)?
        && seat_is_open(api, &binding)
    {
        return Ok(view(api, campaign, Some(&binding)));
    }
    let binding = open_pane(api, campaign, &root)?;
    save(api, &binding)?;
    Ok(view(api, campaign, Some(&binding)))
}

/// `seat.invoke {name, kind, model?, effort?, resume?, prompt_file?}`.
fn invoke(api: &Api, campaign: &str, args: &Value) -> Result<Value> {
    campaign_root(api, campaign)?;
    let kind = s(args, "kind")?;
    let h = harness(kind)?;
    if !installed(kind) {
        return Err(Error::Denied(format!("{} is not installed", h.words)));
    }
    let model = opt(args, "model");
    let effort = opt(args, "effort");
    if let Some(m) = model {
        check_route(kind, m)?;
    }
    let resume = opt(args, "resume");
    if resume.is_some() && !h.resumes {
        return Err(Error::Denied(format!(
            "{} cannot resume a session here",
            h.words
        )));
    }
    if let Some(id) = resume {
        if id.len() > 128
            || !id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(Error::Denied(
                "A session id is letters, numbers and dashes".into(),
            ));
        }
        if open_elsewhere(id) {
            return Err(Error::Denied(
                "That session is open in another terminal; close it there first".into(),
            ));
        }
    }
    let mut binding = match load(api, campaign)? {
        Some(b) if seat_is_open(api, &b) => b,
        _ => {
            return Err(Error::Denied(
                "Open the orchestrator's terminal first".into(),
            ));
        }
    };
    if binding.harness.is_some() {
        return Err(Error::Denied(
            "The seat already runs an orchestrator; kill it first".into(),
        ));
    }
    // A resumed session runs where it was started: the harness finds it by that folder.
    let session_cwd = match resume {
        Some(id) => find_session(kind, id)?.cwd,
        None => None,
    };
    if let Some(cwd) = session_cwd.filter(|c| *c != binding.cwd && c.is_dir()) {
        api.sessions.lock().close(&binding.pane)?;
        binding = open_pane(api, campaign, &cwd)?;
    }
    let fresh = match (kind, resume) {
        ("claude", None) => Some(fresh_uuid()?),
        _ => None,
    };
    // Like workers, the chief's commands must return to this daemon and campaign.
    let mut env: std::collections::BTreeMap<String, String> =
        api.worker_env.lock().iter().cloned().collect();
    env.extend([
        ("QREAM_ROLE".into(), "orchestrator".into()),
        ("QREAM_CAMPAIGN".into(), campaign.into()),
        (
            "QREAM_ROOT".into(),
            campaign_root(api, campaign)?.display().to_string(),
        ),
        ("QREAM_HOME".into(), api.registry.home.display().to_string()),
    ]);
    let spec = AgentSpec {
        kind: match kind {
            "claude" => AgentKind::Claude,
            "codex" => AgentKind::Codex,
            _ => AgentKind::Opencode,
        },
        argv: {
            // The seat runs unattended like a worker: without these the harness stops at its
            // permission prompt (a role file outside the seat's folder; qream-repair walk fault 2).
            let mut a = argv(kind, model, effort, resume, fresh.as_deref());
            match kind {
                "claude" => a.push("--dangerously-skip-permissions".into()),
                "codex" => a.extend([
                    "--sandbox".into(),
                    "danger-full-access".into(),
                    "--ask-for-approval".into(),
                    "never".into(),
                    "-c".into(),
                    format!(
                        "projects.{}.trust_level=\"trusted\"",
                        serde_json::to_string(&binding.cwd.to_string_lossy())?
                    ),
                ]),
                _ => {}
            }
            a
        },
        env: env.clone(),
        prompt_file: opt(args, "prompt_file").map(PathBuf::from),
    };
    let conversation = resume.map(str::to_owned).or(fresh);
    // The orchestrator is an owned unit of the stop engine: one stop path for every target.
    api.launch_unit_sync(
        campaign,
        ORCHESTRATOR_UNIT,
        &env,
        &binding.pane,
        spec,
        conversation.clone(),
    )?;
    binding.harness = Some(kind.into());
    binding.model = model.map(str::to_owned);
    binding.effort = effort.map(str::to_owned);
    binding.conversation = conversation;
    binding.state = "invoked".into();
    binding.at = now();
    save(api, &binding)?;
    Ok(view(api, campaign, Some(&binding)))
}

/// Seat a registered campaign's chief, resolving omitted choices from its profile.
/// Repeating the same act keeps the pane, launch record and conversation intact.
fn seat_chief(api: &Api, campaign: &str, args: &Value) -> Result<Value> {
    if api.read_only {
        return Err(Error::ReadOnly);
    }
    let root = campaign_root(api, campaign)?;
    if let Some(binding) = load(api, campaign)?
        && binding.harness.is_some()
        && seat_is_open(api, &binding)
    {
        for (key, bound) in [
            ("kind", binding.harness.as_deref()),
            ("model", binding.model.as_deref()),
            ("effort", binding.effort.as_deref()),
            ("resume", binding.conversation.as_deref()),
        ] {
            if opt(args, key).is_some_and(|choice| Some(choice) != bound) {
                return Err(Error::Denied(
                    "The seat already runs a different chief; stop it before changing its choices"
                        .into(),
                ));
            }
        }
        return Ok(view(api, campaign, Some(&binding)));
    }
    let mut choices = args.clone();
    if ["kind", "model", "effort"]
        .iter()
        .any(|key| opt(args, key).is_none())
    {
        let profile = crate::profile::get(&root)?.orchestrator;
        for (key, default) in [
            ("kind", profile.kind),
            ("model", profile.model),
            ("effort", profile.effort),
        ] {
            if opt(args, key).is_none() {
                choices[key] = json!(default);
            }
        }
    }
    let kind = s(&choices, "kind")?;
    harness(kind)?;
    check_route(kind, s(&choices, "model")?)?;
    if !installed(kind) {
        return Err(Error::Denied(format!(
            "{} is not installed",
            harness(kind)?.words
        )));
    }
    open(api, campaign)?;
    let role = binding_path(api, campaign).with_extension("role.md");
    let prompt = format!(
        "# Chief of {campaign}\n\nYour campaign root is {}. Judge submitted seals against the served rows' own definitions of done. Read the sealed report and evidence. Accept proven tasks and check completed rows with the review attached as evidence. Return rejected seals to their owner with one batched list of every blocking finding, choosing gap fill now or after the wave and recording the reason. At most two correction rounds per task; a third is an escalation with a diagnosis. Never add acceptance criteria. The controller is code: never start workers or integrate code. Preserve this conversation and campaign stop controls. Wait for judgment mail; never poll, send heartbeats or perform idle check-ins.\n",
        root.display()
    );
    storage::write(&role, &prompt)?;
    choices["prompt_file"] = json!(role);
    invoke(api, campaign, &choices)
}

/// The orchestrator's unit name in the stop engine.
pub const ORCHESTRATOR_UNIT: &str = "orchestrator";

/// After the stop engine acted on `target: orchestrator`, the seat's binding records the word;
/// a kill also closes the seat's pane and keeps the tree it ended.
pub fn note_control(api: &Api, campaign: &str, action: &str) -> Result<()> {
    let Some(mut binding) = load(api, campaign)? else {
        return Ok(());
    };
    match action {
        "pause" => binding.state = "paused".into(),
        "release" => {
            binding.state = if binding.harness.is_some() {
                "invoked"
            } else {
                "open"
            }
            .into();
        }
        "kill" => {
            binding.tree = tree(api, &binding.pane);
            let _ = api.sessions.lock().close(&binding.pane);
            binding.state = "stopped".into();
        }
        _ => return Ok(()),
    }
    binding.at = now();
    save(api, &binding)
}

/// `leftovers {campaign?}`: processes still alive under a stopped seat.
fn leftovers(api: &Api, campaign: Option<&str>) -> Result<Value> {
    let dir = api.registry.home.join("seats");
    let mut checked = Vec::new();
    let mut left = Vec::new();
    if dir.exists() {
        for registered in api.registry.list()? {
            let Some(binding) = load(api, &registered.name)? else {
                continue;
            };
            if campaign.is_some_and(|c| c != binding.campaign) {
                continue;
            }
            if binding.state == "stopped" {
                let mut pids = binding.tree.clone();
                pids.extend(tree(api, &binding.pane));
                pids.retain(|p| alive(*p));
                pids.sort_unstable();
                pids.dedup();
                for pid in &pids {
                    left.push(json!({"campaign":binding.campaign,"unit":"orchestrator","pid":pid}));
                }
            }
            checked.push(
                json!({"campaign":binding.campaign,"unit":"orchestrator","state":binding.state}),
            );
        }
    }
    Ok(json!({"checked":checked,"leftovers":left}))
}

// ---- Recent sessions and conversations, read from the harnesses' own files ----

#[derive(Clone, Debug, Serialize)]
pub struct Recent {
    pub id: String,
    pub first_line: String,
    pub at: String,
    pub cwd: Option<PathBuf>,
    #[serde(skip)]
    pub path: PathBuf,
}

fn mtime(path: &Path) -> Option<SystemTime> {
    fs::metadata(path).and_then(|m| m.modified()).ok()
}
fn stamp_of(path: &Path) -> String {
    let meta = fs::metadata(path).ok();
    let size = meta.as_ref().map_or(0, std::fs::Metadata::len);
    let at = meta
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_millis());
    format!("{size}:{at}")
}
fn rfc3339(t: SystemTime) -> String {
    chrono::DateTime::<chrono::Local>::from(t).to_rfc3339()
}
fn jsonl_files(dir: &Path) -> Vec<PathBuf> {
    fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|e| e == "jsonl"))
                .collect()
        })
        .unwrap_or_default()
}
fn subdirs(dir: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .collect()
        })
        .unwrap_or_default();
    dirs.sort();
    dirs.reverse();
    dirs
}
fn head_lines(path: &Path) -> impl Iterator<Item = Value> {
    let reader = fs::File::open(path)
        .ok()
        .map(|f| BufReader::new(f.take(HEAD_BYTES)));
    reader
        .into_iter()
        .flat_map(|r| r.lines().map_while(std::result::Result::ok))
        .filter_map(|l| serde_json::from_str::<Value>(&l).ok())
}
/// The first line of a person's message, without the harness's own wrappers.
fn first_line(text: &str) -> Option<String> {
    let text = text.trim_start();
    if text.starts_with("<command-") || text.starts_with("<local-command") {
        return None;
    }
    let text = match text.strip_prefix("<pasted_content") {
        Some(rest) => rest.split_once('>').map_or(rest, |(_, r)| r),
        None => text,
    };
    let line = text.lines().map(str::trim).find(|l| !l.is_empty())?;
    if line.starts_with("<environment_context") || line.starts_with("<user_instructions") {
        return None;
    }
    Some(line.chars().take(120).collect())
}
fn claude_user_text(record: &Value) -> Option<String> {
    if record["type"] != "user" || record["isMeta"] == true {
        return None;
    }
    match &record["message"]["content"] {
        Value::String(text) => Some(text.clone()),
        Value::Array(parts) => {
            let texts: Vec<&str> = parts
                .iter()
                .filter(|p| p["type"] == "text")
                .filter_map(|p| p["text"].as_str())
                .collect();
            (!texts.is_empty()).then(|| texts.join("\n"))
        }
        _ => None,
    }
}
fn claude_recent(path: &Path) -> Option<Recent> {
    let id = path.file_stem()?.to_string_lossy().into_owned();
    let mut cwd = None;
    let mut first = None;
    for record in head_lines(path) {
        if cwd.is_none() {
            cwd = record["cwd"].as_str().map(PathBuf::from);
        }
        if let Some(line) = claude_user_text(&record).and_then(|t| first_line(&t)) {
            first = Some(line);
            break;
        }
    }
    Some(Recent {
        id,
        first_line: first?,
        at: rfc3339(mtime(path)?),
        cwd,
        path: path.into(),
    })
}
fn codex_user_text(record: &Value) -> Option<String> {
    let p = &record["payload"];
    if record["type"] == "event_msg" && p["type"] == "user_message" {
        return p["message"].as_str().map(str::to_owned);
    }
    // Codex 0.162 records the person's message only as a user response item; the harness's own
    // injected context (AGENTS.md, environment) comes the same way and is skipped.
    if record["type"] == "response_item" && p["type"] == "message" && p["role"] == "user" {
        let text: Vec<&str> = p["content"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|c| c["text"].as_str())
            .collect();
        let text = text.join("\n");
        let head = text.trim_start();
        if head.starts_with("# AGENTS.md")
            || head.starts_with("<environment_context")
            || head.starts_with("<user_instructions")
            || head.starts_with("<INSTRUCTIONS>")
        {
            return None;
        }
        return Some(text);
    }
    None
}
fn codex_recent(path: &Path) -> Option<Recent> {
    let mut id = None;
    let mut cwd = None;
    let mut first = None;
    for record in head_lines(path) {
        if record["type"] == "session_meta" {
            id = record["payload"]["id"].as_str().map(str::to_owned);
            cwd = record["payload"]["cwd"].as_str().map(PathBuf::from);
        }
        if let Some(line) = codex_user_text(&record).and_then(|t| first_line(&t)) {
            first = Some(line);
            break;
        }
    }
    Some(Recent {
        id: id?,
        first_line: first?,
        at: rfc3339(mtime(path)?),
        cwd,
        path: path.into(),
    })
}
/// Session files of a harness, newest first, at most `limit` with a first line.
fn recent(kind: &str, limit: usize) -> Result<Vec<Recent>> {
    let home = home()?;
    let mut out = Vec::new();
    match kind {
        "claude" => {
            let mut files: Vec<(SystemTime, PathBuf)> = subdirs(&home.join(".claude/projects"))
                .iter()
                .flat_map(|d| jsonl_files(d))
                .filter_map(|p| Some((mtime(&p)?, p)))
                .collect();
            files.sort_by(|a, b| b.0.cmp(&a.0));
            for (_, path) in files {
                if out.len() >= limit {
                    break;
                }
                out.extend(claude_recent(&path));
            }
        }
        "codex" => {
            let root = home.join(".codex/sessions");
            'days: for year in subdirs(&root) {
                for month in subdirs(&year) {
                    for day in subdirs(&month) {
                        let mut files = jsonl_files(&day);
                        files.sort();
                        files.reverse();
                        for path in files {
                            if out.len() >= limit {
                                break 'days;
                            }
                            out.extend(codex_recent(&path));
                        }
                    }
                }
            }
            out.sort_by(|a, b| b.at.cmp(&a.at));
        }
        _ => {}
    }
    Ok(out)
}
/// One session of a harness by id, wherever its file is.
fn find_session(kind: &str, id: &str) -> Result<Recent> {
    let home = home()?;
    let missing = || Error::Denied("No session of that harness has this id".into());
    match kind {
        "claude" => subdirs(&home.join(".claude/projects"))
            .iter()
            .map(|d| d.join(format!("{id}.jsonl")))
            .find(|p| p.is_file())
            .and_then(|p| {
                claude_recent(&p).or_else(|| {
                    let cwd = head_lines(&p).find_map(|r| r["cwd"].as_str().map(PathBuf::from));
                    Some(Recent {
                        id: id.into(),
                        first_line: String::new(),
                        at: rfc3339(mtime(&p)?),
                        cwd,
                        path: p,
                    })
                })
            })
            .ok_or_else(missing),
        "codex" => {
            let root = home.join(".codex/sessions");
            for year in subdirs(&root) {
                for month in subdirs(&year) {
                    for day in subdirs(&month) {
                        if let Some(p) = jsonl_files(&day)
                            .into_iter()
                            .find(|p| p.to_string_lossy().ends_with(&format!("{id}.jsonl")))
                        {
                            return codex_recent(&p).ok_or_else(missing);
                        }
                    }
                }
            }
            Err(missing())
        }
        _ => Err(missing()),
    }
}

/// A turn of the conversation in words. A tool call is named by its tool only: its
/// arguments (file names, paths) stay in the session's own file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Turn {
    pub role: &'static str,
    pub text: String,
    pub at: String,
}
fn cut(text: &str) -> String {
    if text.chars().count() <= TURN_CHARS {
        return mask(text);
    }
    let mut t: String = text.chars().take(TURN_CHARS).collect();
    t.push_str(" …");
    mask(&t)
}
/// Keys and tokens met in a conversation are shown as [masked] (CONTRACTS §2, 22:2x amendment).
pub fn mask(text: &str) -> String {
    let text = &crate::mask::Mask::house().apply(text);
    static SECRET: std::sync::OnceLock<Option<regex::Regex>> = std::sync::OnceLock::new();
    let pattern = SECRET.get_or_init(|| {
        regex::Regex::new(
            r"sk-[A-Za-z0-9_-]{16,}|gh[pousr]_[A-Za-z0-9]{20,}|xox[abprs]-[A-Za-z0-9-]{10,}|AKIA[0-9A-Z]{16}|(?i)(?:token|secret|password|api[_-]?key)\s*[=:]\s*[^\s]{8,}",
        )
        .ok()
    });
    match pattern {
        Some(re) => re.replace_all(text, "[masked]").into_owned(),
        None => text.to_owned(),
    }
}
fn tool_words(name: &str) -> String {
    format!("Used {name}")
}
pub fn claude_turns(lines: impl Iterator<Item = Value>) -> Vec<Turn> {
    let mut turns = Vec::new();
    for record in lines {
        let at = record["timestamp"].as_str().unwrap_or("").to_owned();
        match record["type"].as_str() {
            Some("user") => {
                if let Some(text) = claude_user_text(&record) {
                    let trimmed = text.trim();
                    if !trimmed.is_empty() && !trimmed.starts_with("<command-") {
                        turns.push(Turn {
                            role: "user",
                            text: cut(trimmed),
                            at,
                        });
                    }
                }
            }
            Some("assistant") => {
                if let Value::Array(parts) = &record["message"]["content"] {
                    for part in parts {
                        match part["type"].as_str() {
                            Some("text") => {
                                let text = part["text"].as_str().unwrap_or("").trim();
                                if !text.is_empty() {
                                    turns.push(Turn {
                                        role: "assistant",
                                        text: cut(text),
                                        at: at.clone(),
                                    });
                                }
                            }
                            Some("tool_use") => turns.push(Turn {
                                role: "tool",
                                text: tool_words(part["name"].as_str().unwrap_or("a tool")),
                                at: at.clone(),
                            }),
                            _ => {}
                        }
                    }
                }
            }
            _ => {}
        }
    }
    turns
}
pub fn codex_turns(lines: impl Iterator<Item = Value>) -> Vec<Turn> {
    let mut turns = Vec::new();
    for record in lines {
        let at = record["timestamp"].as_str().unwrap_or("").to_owned();
        let p = &record["payload"];
        if (record["type"] == "event_msg" && p["type"] == "user_message")
            || (record["type"] == "response_item" && p["type"] == "message" && p["role"] == "user")
        {
            let owned = codex_user_text(&record).unwrap_or_default();
            let text = owned.trim();
            if !text.is_empty() {
                turns.push(Turn {
                    role: "user",
                    text: cut(text),
                    at,
                });
            }
        } else if record["type"] == "response_item" {
            match p["type"].as_str() {
                Some("message") if p["role"] == "assistant" => {
                    let text: Vec<&str> = p["content"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|c| c["text"].as_str())
                        .collect();
                    let text = text.join("\n");
                    if !text.trim().is_empty() {
                        turns.push(Turn {
                            role: "assistant",
                            text: cut(text.trim()),
                            at,
                        });
                    }
                }
                Some("function_call" | "custom_tool_call" | "local_shell_call") => {
                    turns.push(Turn {
                        role: "tool",
                        text: tool_words(p["name"].as_str().unwrap_or("a tool")),
                        at,
                    })
                }
                _ => {}
            }
        }
    }
    turns
}
fn all_lines(path: &Path) -> Result<impl Iterator<Item = Value>> {
    let file = fs::File::open(path).map_err(|e| Error::io(path, e))?;
    Ok(BufReader::new(file)
        .lines()
        .map_while(std::result::Result::ok)
        .filter_map(|l| serde_json::from_str::<Value>(&l).ok()))
}
/// The newest Codex session started in `cwd` since `since`: a fresh Codex start has no id flag.
fn codex_started(cwd: &Path, since: &str) -> Option<Recent> {
    recent("codex", 20)
        .ok()?
        .into_iter()
        .find(|r| r.cwd.as_deref() == Some(cwd) && r.at.as_str() >= since)
}
/// `conversation.get {campaign, task?, from?, stamp?}`: the seat's conversation in words.
/// The seat answers for `task` orchestrator (or none); a worker's conversation is the engine's.
/// `from` returns the turns from that index; `stamp` answers `unchanged` when nothing was added.
fn conversation(api: &Api, campaign: &str, args: &Value) -> Result<Value> {
    if let Some(task) = opt(args, "task").filter(|t| *t != "orchestrator") {
        return worker_conversation(api, campaign, task, args);
    }
    let stamp = opt(args, "stamp");
    let from = args.get("from").and_then(Value::as_u64).map(|n| n as usize);
    let binding = load(api, campaign)?;
    let Some(b) = binding.filter(|b| b.harness.is_some()) else {
        return Ok(json!({"turns":[],"total":0,"stamp":"","bound":false}));
    };
    let kind = b.harness.as_deref().unwrap_or("");
    let found = match &b.conversation {
        Some(id) => find_session(kind, id).ok(),
        None if kind == "codex" => codex_started(&b.cwd, &b.at),
        None => None,
    };
    let Some(found) = found else {
        return Ok(json!({"turns":[],"total":0,"stamp":"","bound":true}));
    };
    let new_stamp = stamp_of(&found.path);
    if stamp == Some(new_stamp.as_str()) {
        return Ok(json!({"unchanged":true,"stamp":new_stamp,"bound":true}));
    }
    let turns = match kind {
        "claude" => claude_turns(all_lines(&found.path)?),
        "codex" => codex_turns(all_lines(&found.path)?),
        _ => Vec::new(),
    };
    let total = turns.len();
    let start = from.map_or(total.saturating_sub(TURN_LIMIT), |f| f.min(total));
    let tail = &turns[start.max(total.saturating_sub(TURN_LIMIT))..];
    Ok(json!({"turns":tail,"total":total,"stamp":new_stamp,"bound":true,"conversation":found.id}))
}

/// A worker's conversation in words: the harness session its pane started, found by the pane's
/// working folder and launch time (the window's Conversation view; OD-2026-10-11-09-R02).
fn worker_conversation(api: &Api, campaign: &str, task: &str, args: &Value) -> Result<Value> {
    let session = api.registry.get(campaign)?.session;
    let journal = api
        .sessions
        .lock()
        .panes(&session)?
        .into_iter()
        .rev()
        .find(|j| {
            j.launch.is_some() && j.spec.env.get("QREAM_TASK").map(String::as_str) == Some(task)
        });
    let Some(journal) = journal else {
        return Ok(json!({"turns":[],"total":0,"stamp":"","bound":false}));
    };
    let Some(launch) = journal.launch.as_ref() else {
        return Ok(json!({"turns":[],"total":0,"stamp":"","bound":false}));
    };
    let kind = serde_json::to_value(launch.spec.kind)?
        .as_str()
        .unwrap_or_default()
        .to_owned();
    let cwd = journal.spec.cwd.clone();
    let since = launch.started_at.clone();
    let found = match kind.as_str() {
        "codex" => {
            // The rollout files of the launch day, newest first; the one whose recorded folder is the pane's.
            let day = since.get(..10).unwrap_or_default().replace('-', "/");
            let mut files = jsonl_files(&home()?.join(".codex/sessions").join(day));
            files.sort();
            files.reverse();
            files
                .into_iter()
                .filter_map(|p| codex_recent(&p))
                .find(|r| {
                    r.cwd.as_deref() == Some(cwd.as_path()) && r.at.as_str() >= since.as_str()
                })
        }
        "claude" => recent("claude", 40).ok().and_then(|all| {
            all.into_iter().find(|r| {
                r.cwd.as_deref() == Some(cwd.as_path()) && r.at.as_str() >= since.as_str()
            })
        }),
        _ => None,
    };
    let Some(found) = found else {
        return Ok(json!({"turns":[],"total":0,"stamp":"","bound":true}));
    };
    let stamp = opt(args, "stamp");
    let from = args.get("from").and_then(Value::as_u64).map(|n| n as usize);
    let new_stamp = stamp_of(&found.path);
    if stamp == Some(new_stamp.as_str()) {
        return Ok(json!({"unchanged":true,"stamp":new_stamp,"bound":true}));
    }
    let turns = match kind.as_str() {
        "claude" => claude_turns(all_lines(&found.path)?),
        _ => codex_turns(all_lines(&found.path)?),
    };
    let total = turns.len();
    let start = from.map_or(total.saturating_sub(TURN_LIMIT), |f| f.min(total));
    let tail = &turns[start.max(total.saturating_sub(TURN_LIMIT))..];
    Ok(json!({"turns":tail,"total":total,"stamp":new_stamp,"bound":true,"conversation":found.id}))
}
/// Seat commands: `seat.harnesses`, `seat.get`, `seat.open`, `seat.invoke`, `sessions.recent`,
/// `conversation.get`, `leftovers`.
pub fn dispatch(api: &Api, cmd: &str, args: &Value) -> Result<Value> {
    let name = || {
        args.get("name")
            .or_else(|| args.get("campaign"))
            .and_then(Value::as_str)
            .ok_or_else(|| Error::Denied("Name the campaign".into()))
    };
    Ok(match cmd {
        "seat.harnesses" => Value::Array(
            HARNESSES
                .iter()
                .map(|h| {
                    json!({"kind":h.kind,"words":h.words,"installed":installed(h.kind),
                    "models":h.models,"efforts":h.efforts,"resumes":h.resumes})
                })
                .collect(),
        ),
        "seat.get" => {
            let campaign = name()?;
            view(api, campaign, load(api, campaign)?.as_ref())
        }
        "seat.open" => open(api, name()?)?,
        "seat.invoke" if args["chief"] == true => seat_chief(api, name()?, args)?,
        "seat.invoke" => invoke(api, name()?, args)?,
        "sessions.recent" => {
            let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(20) as usize;
            serde_json::to_value(recent(s(args, "kind")?, limit.min(100))?)?
        }
        "conversation.get" => conversation(api, name()?, args)?,
        "leftovers" => leftovers(api, opt(args, "campaign").or(opt(args, "name")))?,
        _ => return Err(Error::Unimplemented("wave 3")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_claude_model_on_codex_is_refused() {
        assert!(check_route("codex", "claude-opus-5-5").is_err());
        assert!(check_route("claude", "gpt-6.1-sol").is_err());
        assert!(check_route("claude", "claude-fable-5-1").is_ok());
        assert!(check_route("codex", "gpt-6.1-sol").is_ok());
    }
    #[test]
    fn resume_arguments_follow_each_harness() {
        assert_eq!(
            argv(
                "claude",
                Some("claude-opus-5-5"),
                Some("high"),
                Some("abc"),
                None
            ),
            [
                "claude",
                "--resume",
                "abc",
                "--model",
                "claude-opus-5-5",
                "--effort",
                "high"
            ]
        );
        assert_eq!(
            argv("codex", Some("gpt-6.1-sol"), Some("low"), Some("abc"), None),
            [
                "codex",
                "resume",
                "abc",
                "-m",
                "gpt-6.1-sol",
                "-c",
                "model_reasoning_effort=\"low\""
            ]
        );
        assert_eq!(
            argv("claude", None, None, None, Some("u1")),
            ["claude", "--session-id", "u1"]
        );
    }
    #[test]
    fn codex_0_162_user_messages_are_read_and_its_injected_context_is_not() {
        let lines = [
            json!({"type":"session_meta","timestamp":"t0","payload":{"id":"s1","cwd":"/w"}}),
            json!({"type":"response_item","timestamp":"t1","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"# AGENTS.md instructions\n\n<INSTRUCTIONS>x"}]}}),
            json!({"type":"response_item","timestamp":"t2","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"Your directive is the file /d/T01.md."}]}}),
            json!({"type":"response_item","timestamp":"t3","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Reading it."}]}}),
        ];
        let turns = codex_turns(lines.clone().into_iter());
        let words: Vec<(&str, &str)> = turns.iter().map(|t| (t.role, t.text.as_str())).collect();
        assert_eq!(
            words,
            vec![
                ("user", "Your directive is the file /d/T01.md."),
                ("assistant", "Reading it.")
            ]
        );
        assert_eq!(
            codex_user_text(&lines[2]).as_deref(),
            Some("Your directive is the file /d/T01.md.")
        );
        assert!(codex_user_text(&lines[1]).is_none());
    }
    #[test]
    fn tool_calls_keep_their_paths_out_of_the_turns() {
        let lines = [
            json!({"type":"user","timestamp":"t1","message":{"content":"Please look"}}),
            json!({"type":"assistant","timestamp":"t2","message":{"content":[
                {"type":"thinking","thinking":"hidden"},
                {"type":"text","text":"Reading it."},
                {"type":"tool_use","name":"Read","input":{"file_path":"/home/x/secret.md"}}]}}),
            json!({"type":"user","timestamp":"t3","message":{"content":[{"type":"tool_result","content":"/home/x"}]}}),
        ];
        let turns = claude_turns(lines.into_iter());
        assert_eq!(turns.len(), 3);
        assert_eq!(turns[2].text, "Used Read");
        assert!(turns.iter().all(|t| !t.text.contains("/home/x")));
    }
    #[test]
    fn keys_in_turns_are_masked() {
        let text = mask("use sk-ant-api03-abcdefghijklmnopqrstuv and token=abcd1234efgh then go");
        assert_eq!(text, "use [masked] and [masked] then go");
    }
    #[test]
    fn first_lines_skip_wrappers() {
        assert_eq!(
            first_line("\n\n<pasted_content id=\"1\">\nQREAM run x\nmore").as_deref(),
            Some("QREAM run x")
        );
        assert_eq!(first_line("<command-name>/clear</command-name>"), None);
    }
}
