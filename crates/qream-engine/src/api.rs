//! The daemon's socket API (CONTRACTS.md §2): one process owns the terminal layer's
//! session store, the tracker store opened in-process, and the campaign registry, and
//! answers JSON lines. Commands later waves build answer `unimplemented` (wave 3).
use crate::{
    Error, Result,
    close::Close,
    control::{self, Action, Delivery, Engine, HoldKind, UnitRecord, UnitStatus},
    leftovers,
    mask::{self, Mask},
    observe::{self, ControlView, WorkerPane},
    owned::{self, Liveness, Owned, Owner, Role},
    panes::{SessionOpener, SessionPane},
    records::{duration, fault, replay, usage},
    registry::{Campaign, Registry},
    rules::{self, LintOptions},
    seat,
    setup::{self, Route, Scope},
    states::{CampaignState, ProcessView, TaskWord},
};
use chrono::{DateTime, FixedOffset};
use parking_lot::Mutex;
use qream_term::{
    Viewer,
    agent::{AgentKind, AgentSpec, LaunchRecord},
    core::{CoreFactory, LivePanes},
    events::EventBus,
    handle::{Direction, Split},
    session::SessionStore,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{UnixListener, UnixStream},
    sync::{RwLock, broadcast, watch},
};
use tracker_store::Store;

pub const MAX_LINE: usize = 1024 * 1024;
/// The daemon leaves when no session and no campaign has been up for this long.
pub const IDLE_EXIT: Duration = Duration::from_secs(600);
const UNBUILT: &str = "wave 3";

#[derive(Deserialize)]
struct Request {
    v: u32,
    id: String,
    cmd: String,
    #[serde(default)]
    args: Value,
}

struct Attached {
    pane: String,
    connection: u64,
    viewer: Viewer,
}

pub struct Api {
    pub registry: Registry,
    pub build: String,
    pub revision: u64,
    pub states: RwLock<BTreeMap<String, CampaignState>>,
    pub events: broadcast::Sender<Value>,
    pub sessions: Arc<Mutex<SessionStore>>,
    pub live: LivePanes,
    pub(crate) store: StoreHandle,
    /// One controller per campaign up, driven by events (`live.rs`).
    pub(crate) runners: Mutex<BTreeMap<String, crate::live::Runner>>,
    /// Environment every worker pane carries so its `qream` reaches this daemon.
    pub worker_env: Mutex<Vec<(String, String)>>,
    /// This daemon itself, for the controllers' launch seam.
    pub(crate) me: std::sync::OnceLock<std::sync::Weak<Api>>,
    /// Each campaign's stop and recovery engine, opened on first use from its folder.
    controls: Mutex<BTreeMap<String, Engine>>,
    /// Everything this user's QREAM started, with its birth identity.
    pub owned: owned::Registry,
    /// Set by an agent event or a control act: the next tick derives and publishes states.
    dirty: AtomicBool,
    /// What each campaign's folder looked like at the last publication.
    fingerprints: Mutex<BTreeMap<String, String>>,
    /// The last published words of each campaign, so only a change is published.
    published: Mutex<BTreeMap<String, String>>,
    /// The full registry list last pushed to subscribed windows.
    registry_seen: Mutex<Option<String>>,
    /// When a task of each campaign was last seen working in this daemon.
    last_active: Mutex<BTreeMap<String, DateTime<FixedOffset>>>,
    viewers: Mutex<BTreeMap<String, Attached>>,
    next_viewer: AtomicU64,
    next_connection: AtomicU64,
    /// Connections subscribed to events (a window or a watcher): a connected viewer is activity.
    watchers: Mutex<BTreeSet<u64>>,
    /// The store revision last announced as a `store` event.
    store_seen: Mutex<Option<u64>>,
    idle_since: Mutex<Option<Instant>>,
    stop: watch::Sender<bool>,
    /// `QREAM_READ_ONLY=1`: store opened read-only, every write refused.
    pub read_only: bool,
    /// The replay recorder of each campaign up.
    recorders: Mutex<BTreeMap<String, tokio::task::JoinHandle<()>>>,
}

fn s<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| Error::Denied(format!("Give the {key}")))
}
fn n(args: &Value, key: &str) -> Result<u64> {
    args.get(key)
        .and_then(|v| v.as_u64().or_else(|| v.as_str()?.parse().ok()))
        .ok_or_else(|| Error::Denied(format!("Give the {key} as a number")))
}
fn size(args: &Value, key: &str) -> Result<u16> {
    u16::try_from(n(args, key)?).map_err(|_| Error::Denied(format!("The {key} is too large")))
}
fn path(args: &Value, key: &str) -> Result<PathBuf> {
    s(args, key).map(PathBuf::from)
}
fn env(args: &Value) -> BTreeMap<String, String> {
    args.get("env")
        .and_then(Value::as_object)
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_owned())))
                .collect()
        })
        .unwrap_or_default()
}
fn words(args: &Value, key: &str) -> Vec<String> {
    match args.get(key) {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect(),
        Some(Value::String(text)) => text
            .split(',')
            .filter(|w| !w.is_empty())
            .map(str::to_owned)
            .collect(),
        _ => Vec::new(),
    }
}

impl Api {
    /// A daemon over `home` with no tracker store (store commands refuse).
    pub fn new(home: PathBuf, build: String, revision: u64) -> Result<Arc<Self>> {
        Self::open(home, None, build, revision)
    }
    /// Open the session journals under `home` and, lazily, the tracker store at `store`.
    pub fn open(
        home: PathBuf,
        store: Option<PathBuf>,
        build: String,
        revision: u64,
    ) -> Result<Arc<Self>> {
        let (events, _) = broadcast::channel(1024);
        let (stop, _) = watch::channel(false);
        let live = LivePanes::default();
        let sessions = SessionStore::open(
            &home,
            Box::new(CoreFactory::new(live.clone())),
            EventBus::new(1024),
        )?;
        let owned = owned::Registry::open(&home.join("state/owned.json"));
        Ok(Arc::new(Self {
            owned,
            controls: Mutex::new(BTreeMap::new()),
            dirty: AtomicBool::new(true),
            fingerprints: Mutex::new(BTreeMap::new()),
            published: Mutex::new(BTreeMap::new()),
            registry_seen: Mutex::new(None),
            last_active: Mutex::new(BTreeMap::new()),
            registry: Registry::new(home),
            build,
            revision,
            states: RwLock::new(BTreeMap::new()),
            events,
            sessions: Arc::new(Mutex::new(sessions)),
            live,
            store: StoreHandle::new(store),
            runners: Mutex::new(BTreeMap::new()),
            worker_env: Mutex::new(Vec::new()),
            me: std::sync::OnceLock::new(),
            viewers: Mutex::new(BTreeMap::new()),
            next_viewer: AtomicU64::new(1),
            next_connection: AtomicU64::new(1),
            watchers: Mutex::new(BTreeSet::new()),
            store_seen: Mutex::new(None),
            idle_since: Mutex::new(None),
            stop,
            read_only: false,
            recorders: Mutex::new(BTreeMap::new()),
        })
        .with_self())
    }
    /// Records the daemon's own weak handle, for the controllers' launch seam.
    fn with_self(self: Arc<Self>) -> Arc<Self> {
        let _ = self.me.set(Arc::downgrade(&self));
        self
    }
    pub fn hello(&self) -> Value {
        let revision = self
            .with_store(|store| Ok(store.revision()?))
            .unwrap_or(self.revision);
        json!({"v":1,"event":"hello","data":{"build":self.build,"home":self.registry.home,"revision":revision}})
    }
    pub(crate) fn with_store<T>(&self, f: impl FnOnce(&Store) -> Result<T>) -> Result<T> {
        self.store.with(f)
    }
    pub async fn publish_state(&self, state: CampaignState) {
        self.states
            .write()
            .await
            .insert(state.campaign.clone(), state.clone());
        let _ = self
            .events
            .send(json!({"v":1,"event":"campaign","at":state.observed_at,"data":state}));
    }
    /// Restore every session that was up from its journal (panes come back with history).
    /// A read-only daemon restores nothing: rebuilding a pane writes its journal.
    pub fn restore(&self) -> Result<()> {
        if self.read_only {
            return Ok(());
        }
        Ok(self.sessions.lock().restore()?)
    }
    /// The same daemon with every write refused.
    pub fn into_read_only(self: Arc<Self>) -> Arc<Self> {
        match Arc::try_unwrap(self) {
            Ok(mut api) => {
                api.read_only = true;
                api.store.read_only = true;
                api.me = std::sync::OnceLock::new();
                Arc::new(api).with_self()
            }
            Err(shared) => shared,
        }
    }
    /// Journal output, observe agents, deliver pending first prompts; once a second.
    pub fn poll(&self) -> Result<()> {
        Ok(self.sessions.lock().poll_changes()?)
    }
    /// Viewer damage as `pane` events, coalesced per viewer.
    pub fn pump_viewers(&self) {
        let mut viewers = self.viewers.lock();
        for (id, attached) in viewers.iter_mut() {
            let update = attached.viewer.next_update();
            if update.rows.is_empty() && !update.reset {
                continue;
            }
            let mut event = json!({"v":1,"event":"pane","at":chrono::Local::now().to_rfc3339(),"data":{"pane":attached.pane,"viewer":id,"rows":update.rows,"cursor":update.cursor,"scrollback_len":update.scrollback_len,"reset":update.reset}});
            mask_texts(&mut event["data"]["rows"]);
            let _ = self.events.send(event);
        }
    }
    /// True once nothing has been up for the idle window.
    pub fn idle_expired(&self) -> bool {
        let busy = !self.watchers.lock().is_empty()
            || self.sessions.lock().list().iter().any(|s| s.up)
            || self
                .registry
                .list()
                .is_ok_and(|l| l.iter().any(|c| c.status == "up"));
        let mut since = self.idle_since.lock();
        if busy {
            *since = None;
            return false;
        }
        since.get_or_insert_with(Instant::now).elapsed() >= IDLE_EXIT
    }
    pub fn request_stop(&self) {
        let _ = self.stop.send(true);
    }
    pub(crate) fn worker_panes(&self, session: &str) -> Vec<WorkerPane> {
        let sessions = self.sessions.lock();
        let Ok(journals) = sessions.panes(session) else {
            return Vec::new();
        };
        journals
            .into_iter()
            .filter(|j| !j.closed)
            .filter_map(|j| {
                let task = j.spec.env.get("QREAM_TASK")?.clone();
                let children = self
                    .live
                    .get(&j.spec.id)
                    .ok()
                    .and_then(|pane| {
                        let root = pane.pid();
                        pane.child_pids()
                            .ok()
                            .map(|c| c.into_iter().filter(|p| Some(*p) != root).collect())
                    })
                    .unwrap_or_default();
                Some(WorkerPane {
                    pane: j.spec.id.clone(),
                    task,
                    agent: j.agent,
                    children,
                })
            })
            .collect()
    }
    pub(crate) async fn campaign_state(&self, name: &str) -> Result<CampaignState> {
        let mut state =
            match self.registry.get(name) {
                Ok(campaign) => {
                    let state = self.observe_state(name, &campaign, false)?;
                    self.publish_state(state.clone()).await;
                    state
                }
                Err(_) => {
                    self.states.read().await.get(name).cloned().ok_or_else(|| {
                        Error::Denied("The campaign has no observation yet".into())
                    })?
                }
            };
        let now = chrono::Local::now().fixed_offset();
        state.age_seconds = (now - state.observed_at).num_seconds().max(0);
        state.stale = state.age_seconds > 15;
        for task in &mut state.tasks {
            task.age_seconds = (now - task.observed_at).num_seconds().max(0);
            task.stale = task.age_seconds > 15;
        }
        Ok(state)
    }
    /// The one truth of a campaign, derived once from its folder, ledger, seals, panes, holds
    /// and owned processes.
    fn derive_state(&self, name: &str, campaign: &Campaign) -> Result<CampaignState> {
        self.observe_state(name, campaign, !self.read_only)
    }
    /// Reads and background updates gather the same sources; only updates persist estimates.
    fn observe_state(
        &self,
        name: &str,
        campaign: &Campaign,
        persist_close: bool,
    ) -> Result<CampaignState> {
        let panes = self.worker_panes(&campaign.session);
        let now = chrono::Local::now().fixed_offset();
        let mut view = self.control_view(name, campaign)?;
        view.persist_close = persist_close;
        let state = if self.store.path.is_some() {
            self.with_store(|store| {
                observe::campaign_state(name, &campaign.root, panes, Some(store), now, &view)
            })?
        } else {
            observe::campaign_state(name, &campaign.root, panes, None, now, &view)?
        };
        if state
            .tasks
            .iter()
            .any(|t| matches!(t.word, TaskWord::Working | TaskWord::WaitingOnJob))
        {
            self.last_active.lock().insert(name.into(), now);
        }
        Ok(state)
    }
    /// Holds from the campaign's records, its live owned processes and its live panes.
    fn control_view(&self, name: &str, campaign: &Campaign) -> Result<ControlView> {
        let holds = control::load_holds(&campaign.root)?;
        let whole = |kind: HoldKind| {
            holds
                .iter()
                .any(|h| h.scope == control::Scope::Campaign && h.kind == kind && !h.pending)
        };
        let mut processes = Vec::new();
        for o in self.owned.list()? {
            if o.root != campaign.root {
                continue;
            }
            if let Liveness::Alive(pids) = owned::liveness(&o)? {
                processes.push(ProcessView {
                    owner: o
                        .task
                        .clone()
                        .unwrap_or_else(|| format!("{:?}", o.role).to_lowercase()),
                    role: format!("{:?}", o.role).to_lowercase(),
                    purpose: o.purpose.clone(),
                    what: match &o.handle {
                        owned::Handle::Group { pgid, .. } => format!("process group {pgid}"),
                        owned::Handle::Unit { name, .. } => format!("unit {name}"),
                    },
                    pids,
                });
            }
        }
        let recorded: BTreeSet<u32> = processes.iter().flat_map(|p| p.pids.clone()).collect();
        for (pane, task, pids) in self.session_processes(&campaign.session) {
            if pids.iter().all(|p| recorded.contains(p)) {
                continue;
            }
            processes.push(ProcessView {
                owner: task.unwrap_or_else(|| "pane".into()),
                role: "pane".into(),
                purpose: "terminal pane".into(),
                what: format!("pane {pane}"),
                pids,
            });
        }
        Ok(ControlView {
            paused: whole(HoldKind::Pause),
            killed: whole(HoldKind::Kill),
            last_active: self.last_active.lock().get(name).copied(),
            processes,
            persist_close: !self.read_only,
        })
    }
    /// Each live pane of a session: its id, its task, its root process and descendants.
    fn session_processes(&self, session: &str) -> Vec<(String, Option<String>, Vec<u32>)> {
        let journals = self.sessions.lock().panes(session).unwrap_or_default();
        journals
            .into_iter()
            .filter_map(|j| {
                let pane = self.live.get(&j.spec.id).ok()?;
                if pane.exited().is_some() {
                    return None;
                }
                let pids = pane.child_pids().ok()?;
                (!pids.is_empty()).then(|| {
                    (
                        j.spec.id.clone(),
                        j.spec.env.get("QREAM_TASK").cloned(),
                        pids,
                    )
                })
            })
            .collect()
    }
    /// Runs `act` on the campaign's stop and recovery engine, opening it on first use; units
    /// this daemon runs reattach to their panes, the rest wait for the recovery act.
    fn with_control<T>(&self, name: &str, act: impl FnOnce(&mut Engine) -> Result<T>) -> Result<T> {
        let campaign = self.registry.get(name)?;
        let mut controls = self.controls.lock();
        if !controls.contains_key(name) {
            let mut engine =
                Engine::open(&campaign.root, name, name, "operator", self.owned.clone())?;
            let panes: Vec<(String, String)> = engine
                .units()
                .values()
                .filter_map(|u| Some((u.task.clone(), u.pane.clone()?)))
                .collect();
            for (task, pane) in panes {
                if self.live.get(&pane).is_ok() {
                    engine.attach(&task, Box::new(self.pane_handle(&pane)));
                }
            }
            controls.insert(name.into(), engine);
        }
        let engine = controls
            .get_mut(name)
            .ok_or_else(|| Error::NotFound(format!("campaign {name}")))?;
        let result = act(engine);
        self.dirty.store(true, Ordering::Relaxed);
        result
    }
    fn pane_handle(&self, pane: &str) -> SessionPane {
        SessionPane::new(pane, Arc::clone(&self.sessions), self.live.clone())
    }
    /// The campaign whose session holds this pane.
    fn campaign_of_pane(&self, pane: &str) -> Option<Campaign> {
        let campaigns = self.registry.list().ok()?;
        let sessions = self.sessions.lock();
        campaigns.into_iter().find(|c| {
            sessions
                .panes(&c.session)
                .is_ok_and(|panes| panes.iter().any(|j| j.spec.id == pane))
        })
    }
    /// Starts a worker or control-role agent as an owned unit: the launch is registered before
    /// its process exists, so a stop that arrives during the launch ends it before its first prompt.
    async fn launch_unit(
        &self,
        campaign: &str,
        task: &str,
        env: &BTreeMap<String, String>,
        pane: &str,
        spec: AgentSpec,
        conversation: Option<String>,
    ) -> Result<Value> {
        let (launched, first) = self.launch_begin(campaign, task, env, pane, spec, conversation)?;
        if let Some(ms) = std::env::var("QREAM_LAUNCH_SETTLE_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
        {
            // A harness that takes this long to come up: the window in which a stop may land.
            tokio::time::sleep(Duration::from_millis(ms)).await;
        }
        self.launch_finish(campaign, task, pane, &launched, first)?;
        Ok(serde_json::to_value(launched)?)
    }
    /// The controller's wave release: the same owned-unit launch, without the settle wait.
    pub(crate) fn launch_unit_sync(
        &self,
        campaign: &str,
        task: &str,
        env: &BTreeMap<String, String>,
        pane: &str,
        spec: AgentSpec,
        conversation: Option<String>,
    ) -> Result<Value> {
        let (launched, first) = self.launch_begin(campaign, task, env, pane, spec, conversation)?;
        self.launch_finish(campaign, task, pane, &launched, first)?;
        Ok(serde_json::to_value(launched)?)
    }
    fn launch_begin(
        &self,
        campaign: &str,
        task: &str,
        env: &BTreeMap<String, String>,
        pane: &str,
        spec: AgentSpec,
        conversation: Option<String>,
    ) -> Result<(qream_term::agent::LaunchRecord, Option<String>)> {
        let role = match env.get("QREAM_ROLE").map(String::as_str) {
            Some("orchestrator") => Role::Orchestrator,
            Some("controller") => Role::Controller,
            _ => Role::Worker,
        };
        let pending = match &spec.prompt_file {
            Some(path) => Some(Delivery {
                id: "directive".into(),
                text: qream_term::agent::directive_prompt(path).map_err(|e| Error::io(path, e))?,
            }),
            None => None,
        };
        let first = pending.as_ref().map(|d| d.text.clone());
        let record = LaunchRecord {
            spec: spec.clone(),
            pid: None,
            started_at: crate::now(),
            conversation_id: conversation.clone(),
            pending_prompt: None,
        };
        self.with_control(campaign, |e| {
            e.begin_launch(UnitRecord {
                task: task.into(),
                role,
                wave: env.get("QREAM_WAVE").and_then(|w| w.parse().ok()),
                record,
                pane: Some(pane.into()),
                pending,
                status: UnitStatus::Launching,
                reason: None,
            })
        })?;
        // The engine is not held during the spawn: a kill that lands meanwhile wins at finish.
        // The first prompt waits for the landing, so a stopped launch never receives it.
        let spec = AgentSpec {
            prompt_file: None,
            ..spec
        };
        let started = self.sessions.lock().start(pane, spec);
        let launched = match started {
            Ok(launched) => launched,
            Err(e) => {
                self.with_control(campaign, |en| {
                    en.set_status(task, UnitStatus::Stopped, "the launch failed")?;
                    en.log(
                        task,
                        "launching",
                        "stopped",
                        &format!("the launch failed: {e}"),
                    )
                })?;
                return Err(e.into());
            }
        };
        if let Some(id) = &conversation {
            self.sessions.lock().bind_conversation(pane, id)?;
        }
        Ok((launched, first))
    }
    fn launch_finish(
        &self,
        campaign: &str,
        task: &str,
        pane: &str,
        launched: &qream_term::agent::LaunchRecord,
        first: Option<String>,
    ) -> Result<()> {
        let pid = launched
            .pid
            .ok_or_else(|| Error::Invalid("the harness started without a process id".into()))?;
        let handle = Box::new(self.pane_handle(pane));
        self.with_control(campaign, |e| e.finish_launch(task, handle, pid))?;
        if let Some(text) = first {
            self.sessions.lock().queue_prompt(pane, &text)?;
        }
        Ok(())
    }
    /// A `store` event `{revision, rows}` when the store's revision moved, while anyone watches.
    fn publish_store(&self) {
        if self.watchers.lock().is_empty() || self.store.path.is_none() {
            return;
        }
        let seen = *self.store_seen.lock();
        let Ok((revision, rows)) = self.with_store(|st| {
            let revision = st.revision()?;
            let rows: BTreeSet<String> = match seen {
                Some(old) if old < revision => st
                    .events_since(old)?
                    .iter()
                    .filter_map(|e| e.get("id").and_then(Value::as_str).map(str::to_owned))
                    .collect(),
                _ => BTreeSet::new(),
            };
            Ok((revision, rows))
        }) else {
            return;
        };
        *self.store_seen.lock() = Some(revision);
        if seen.is_some_and(|old| old != revision) {
            let _ = self.events.send(json!({"v":1,"event":"store","at":chrono::Local::now().to_rfc3339(),"data":{"revision":revision,"rows":rows}}));
        }
    }
    /// Ends every live pane process of the campaign's session (journals and history stay).
    fn end_session_panes(&self, name: &str) -> Result<usize> {
        let campaign = self.registry.get(name)?;
        let mut ended = 0;
        for (pane, task, _) in self.session_processes(&campaign.session) {
            let Ok(live) = self.live.get(&pane) else {
                continue;
            };
            let Some(pid) = live.pid() else { continue };
            let owner = Owner {
                campaign: name,
                root: &campaign.root,
                task: task.as_deref(),
                wave: None,
                role: Role::Session,
                purpose: "terminal pane",
            };
            // The pane's process leads its own session; only that recorded group is ended.
            if let Ok(o) = Owned::group(&owner, pid) {
                match owned::end(&o, Duration::from_secs(3)) {
                    Ok(_) | Err(Error::NotOwned(_)) => ended += 1,
                    Err(e) => return Err(e),
                }
            }
        }
        Ok(ended)
    }
    /// `term.agent.start`: a pane that carries `QREAM_TASK` and belongs to a campaign's session
    /// starts an owned unit through the stop engine; any other pane starts its agent directly.
    async fn agent_start(&self, args: &Value) -> Result<Value> {
        let kind = match s(args, "kind")? {
            "claude" => AgentKind::Claude,
            "codex" => AgentKind::Codex,
            "opencode" => AgentKind::Opencode,
            "shell" => AgentKind::Shell,
            _ => {
                return Err(Error::Denied(
                    "The harness is claude, codex, opencode or shell".into(),
                ));
            }
        };
        // A shell task's args name its own program; a harness is started by name.
        let mut argv = if kind == AgentKind::Shell {
            Vec::new()
        } else {
            vec![s(args, "kind")?.to_owned()]
        };
        argv.extend(words(args, "args"));
        let spec = AgentSpec {
            kind,
            argv,
            env: env(args),
            prompt_file: args
                .get("prompt_file")
                .and_then(Value::as_str)
                .map(PathBuf::from),
        };
        let pane = s(args, "pane")?;
        let conversation = args
            .get("conversation_id")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let pane_env = self.sessions.lock().pane_env(pane)?;
        if let (Some(task), Some(campaign)) =
            (pane_env.get("QREAM_TASK"), self.campaign_of_pane(pane))
        {
            return self
                .launch_unit(&campaign.name, task, &pane_env, pane, spec, conversation)
                .await;
        }
        let launched = self.sessions.lock().start(pane, spec)?;
        if let Some(id) = &conversation {
            self.sessions.lock().bind_conversation(pane, id)?;
        }
        Ok(serde_json::to_value(launched)?)
    }
    fn campaign_control(&self, name: &str, args: &Value) -> Result<Value> {
        let action: Action = s(args, "action")?.parse()?;
        let target: control::Scope = s(args, "target")?.parse()?;
        let units = self.with_control(name, |e| e.act(action, &target))?;
        if target == control::Scope::Campaign && action == Action::Kill {
            self.end_session_panes(name)?;
        }
        if target == control::Scope::Orchestrator {
            seat::note_control(self, name, s(args, "action")?)?;
        }
        let holds = self.with_control(name, |e| Ok(e.holds().to_vec()))?;
        Ok(
            json!({"campaign":name,"action":s(args, "action")?,"target":target.to_string(),"units":units,"holds":holds}),
        )
    }
    /// The one recovery act over every campaign up, or the one named.
    fn recover(&self, args: &Value) -> Result<Value> {
        let names: Vec<String> = match args.get("campaign").and_then(Value::as_str) {
            Some(name) => vec![name.into()],
            None => self
                .registry
                .list()?
                .into_iter()
                .filter(|c| c.status == "up")
                .map(|c| c.name)
                .collect(),
        };
        let mut reports = Vec::new();
        for name in names {
            let mut opener = SessionOpener {
                sessions: Arc::clone(&self.sessions),
                live: self.live.clone(),
            };
            let report = match args.get("task").and_then(Value::as_str) {
                Some(task) => self.with_control(&name, |e| e.refresh(task, &mut opener))?,
                None => self.with_control(&name, |e| e.recover(&mut opener))?,
            };
            reports.push(serde_json::to_value(report)?);
        }
        Ok(Value::Array(reports))
    }
    fn leftovers(&self, args: &Value) -> Result<Value> {
        let campaign = args.get("campaign").and_then(Value::as_str);
        if args.get("kill").and_then(Value::as_bool) == Some(true) {
            Ok(serde_json::to_value(leftovers::kill(
                &self.owned,
                campaign,
                Duration::from_secs(3),
            )?)?)
        } else {
            Ok(serde_json::to_value(leftovers::list(
                &self.owned,
                campaign,
            )?)?)
        }
    }
    /// Refuses acceptance while a process the task started is alive. Seam (wave 4): the
    /// controller's `run.accept` calls this before it writes `accepted` to the ledger.
    pub fn accept_guard(&self, campaign: &str, task: &str) -> Result<()> {
        self.with_control(campaign, |e| e.accept_guard(task))
    }
    /// Advances pending pauses to their safe points and keeps a settled campaign hold empty
    /// of processes; never blocks the tick behind a long recovery act.
    fn tick_controls(&self) -> Result<()> {
        let settled: Vec<String> = {
            let Some(mut controls) = self.controls.try_lock() else {
                return Ok(());
            };
            let mut settled = Vec::new();
            for (name, engine) in controls.iter_mut() {
                if engine.pausing() {
                    engine.tick()?;
                    self.dirty.store(true, Ordering::Relaxed);
                }
                if engine.campaign_hold().is_some_and(|h| !h.pending) {
                    settled.push(name.clone());
                }
            }
            settled
        };
        for name in settled {
            if self.end_session_panes(&name)? > 0 {
                self.dirty.store(true, Ordering::Relaxed);
            }
        }
        Ok(())
    }
    /// What changes a campaign's state on disk: its ledger, holds, units and seals.
    fn fingerprint(root: &Path) -> String {
        let stamp = |p: PathBuf| {
            std::fs::metadata(&p)
                .ok()
                .map(|m| format!("{}:{:?}", m.len(), m.modified().ok()))
                .unwrap_or_default()
        };
        [
            "state/control.jsonl",
            "state/holds.json",
            "state/units.json",
            "artifacts",
            "CLOSE.json",
            "STOP",
        ]
        .iter()
        .map(|p| stamp(root.join(p)))
        .collect::<Vec<_>>()
        .join("|")
    }
    /// Publishes each up campaign's state when an event or its records changed it.
    async fn publish_changes(&self) {
        self.publish_registry_changes();
        let dirty = self.dirty.swap(false, Ordering::Relaxed);
        let Ok(campaigns) = self.registry.list() else {
            return;
        };
        for campaign in campaigns.into_iter().filter(|c| c.status == "up") {
            let print = Self::fingerprint(&campaign.root);
            let moved = self
                .fingerprints
                .lock()
                .insert(campaign.name.clone(), print.clone())
                .is_none_or(|old| old != print);
            if !dirty && !moved {
                continue;
            }
            let Ok(state) = self.derive_state(&campaign.name, &campaign) else {
                continue;
            };
            let words = serde_json::to_string(&json!({
                "word": state.word,
                "counts": state.counts,
                "tasks": state.tasks.iter().map(|t| (&t.task, &t.word, t.blocker.as_ref().map(|b| format!("{:?}", b.kind)), &t.job)).collect::<Vec<_>>(),
                "close": state.close,
                "processes": state.processes.len(),
                "stalled": state.stalled_minutes,
            }))
            .unwrap_or_default();
            let changed = self
                .published
                .lock()
                .insert(campaign.name.clone(), words.clone())
                .is_none_or(|old| old != words);
            if changed {
                self.publish_state(state).await;
            }
        }
    }
    fn campaign_list(&self) -> Result<Value> {
        let list = self
            .registry
            .list()?
            .into_iter()
            .map(|campaign| {
                let division = division(&campaign);
                let mut summary = serde_json::to_value(campaign)?;
                summary["division"] = json!(division);
                Ok(summary)
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Value::Array(list))
    }
    fn publish_registry_changes(&self) {
        let Ok(list) = self.campaign_list() else {
            return;
        };
        let Ok(serialized) = serde_json::to_string(&list) else {
            return;
        };
        let mut seen = self.registry_seen.lock();
        if seen.as_ref() == Some(&serialized) {
            return;
        }
        *seen = Some(serialized);
        let at = crate::now();
        let _ = self
            .events
            .send(json!({"v":1,"event":"campaigns","at":at,"data":list}));
        // Existing windows subscribe to `campaign` and re-read the list on this shape.
        let _ = self
            .events
            .send(json!({"v":1,"event":"campaign","at":at,"data":{"campaigns":list}}));
    }
    /// The replay recorder of a campaign: one recording at once, then one on each change
    /// of the campaign's state (its tasks' words), at most every five seconds. No polling.
    fn start_recorder(&self, name: &str, root: &Path) {
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let mut changes = self.events.subscribe();
        let events = self.events.clone();
        let (name_owned, root) = (name.to_owned(), root.to_path_buf());
        let recorder = runtime.spawn(async move {
            let mut last: Option<Instant> = None;
            loop {
                if last.is_none_or(|t| t.elapsed() >= Duration::from_secs(5)) {
                    let at = root.clone();
                    let done = tokio::task::spawn_blocking(move || record_replay(&at)).await;
                    if let Ok(Err(e)) = done {
                        let _ = events.send(json!({"v":1,"event":"fault","at":chrono::Local::now().to_rfc3339(),"data":{"message":format!("Replay recording of {name_owned}: {e}")}}));
                    }
                    last = Some(Instant::now());
                }
                loop {
                    match changes.recv().await {
                        Ok(event)
                            if matches!(
                                event.get("event").and_then(Value::as_str),
                                Some("campaign" | "task" | "agent")
                            ) =>
                        {
                            break;
                        }
                        Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                        Err(broadcast::error::RecvError::Closed) => return,
                    }
                }
            }
        });
        if let Some(old) = self.recorders.lock().insert(name.into(), recorder) {
            old.abort();
        }
    }
    /// The records verbs: faults, replay, usage and durations (RW-20-R01, -R03; RW-16-R07).
    pub(crate) fn records(&self, cmd: &str, args: &Value) -> Result<Value> {
        let root = || -> Result<PathBuf> {
            match args.get("root").and_then(Value::as_str) {
                Some(root) => Ok(PathBuf::from(root)),
                None => Ok(self
                    .registry
                    .get(
                        args.get("campaign")
                            .and_then(Value::as_str)
                            .ok_or_else(|| Error::Denied("Name the campaign or its root".into()))?,
                    )?
                    .root),
            }
        };
        let ledger = || -> Result<fault::Ledger> {
            let store = self
                .store
                .path
                .as_ref()
                .ok_or_else(|| Error::Denied("This daemon has no tracker store".into()))?;
            Ok(fault::Ledger::new(
                &self.registry.home.join("faults.jsonl"),
                store,
                "qream-controller",
            ))
        };
        let task = args.get("task").and_then(Value::as_str);
        Ok(match cmd {
            "faults.add" => {
                let id = ledger()?.add(&fault::Fault {
                    area: masked(args, "area"),
                    fault: masked(args, "fault"),
                    effect: masked(args, "effect"),
                    source: masked(args, "source"),
                    status: args
                        .get("status")
                        .and_then(Value::as_str)
                        .unwrap_or("open")
                        .into(),
                    fix: masked(args, "fix"),
                    ..fault::Fault::default()
                })?;
                let _ = self.events.send(json!({"v":1,"event":"fault","at":chrono::Local::now().to_rfc3339(),"data":{"id":id}}));
                json!({"id":id})
            }
            "faults.amend" => json!({"id":ledger()?.amend(
                s(args, "id")?,
                s(args, "status")?,
                &masked(args, "fix"),
            )?}),
            "faults.list" => {
                let ledger = ledger()?;
                json!({"ledger":ledger.list()?,"tab":ledger.tab()?})
            }
            "replay.record" => json!({"events":record_replay(&root()?)?}),
            "replay.list" => json!({"tasks":replay::list(&root()?)?}),
            "replay.summary" => serde_json::to_value(replay::summary(&root()?)?)?,
            "replay.events" => serde_json::to_value(replay::events(
                &root()?,
                task,
                args.get("from").and_then(Value::as_str),
                args.get("to").and_then(Value::as_str),
            )?)?,
            "records.duration" => self.with_store(|store| {
                duration::record(
                    store,
                    &duration::Span {
                        campaign: s(args, "campaign")?,
                        task: s(args, "task")?,
                        kind: s(args, "kind")?,
                        started: local_time(args, "started_at")?,
                        ended: local_time(args, "ended_at")?,
                        outcome: s(args, "outcome")?,
                        note: &masked(args, "note"),
                    },
                )?;
                Ok(json!({"recorded":true}))
            })?,
            "records.durations" => {
                self.with_store(|store| Ok(json!(duration::list(store, s(args, "campaign")?)?)))?
            }
            "records.usage" => {
                let session = self.usage_session(args).inspect_err(|e| {
                    let _ = self.events.send(json!({"v":1,"event":"fault","at":crate::now(),"data":{"campaign":args["campaign"],"task":args["task"],"message":Mask::house().apply(&e.to_string())}}));
                })?;
                let tokens = match session.as_deref() {
                    Some(session) => usage::tokens(session)?,
                    None => usage::Tokens::default(),
                };
                let explicit_pid = args.get("pid").and_then(Value::as_u64);
                let pid = explicit_pid.or_else(|| {
                    let campaign = self.registry.get(args["campaign"].as_str()?).ok()?;
                    let sessions = self.sessions.lock();
                    sessions
                        .panes(&campaign.session)
                        .ok()?
                        .into_iter()
                        .find(|p| {
                            !p.closed
                                && p.spec
                                    .env
                                    .get("QREAM_TASK")
                                    .is_some_and(|t| Some(t.as_str()) == task)
                        })?
                        .launch
                        .as_ref()?
                        .pid
                        .map(u64::from)
                });
                let time = match pid {
                    Some(pid) => Some(usage::process_time(
                        u32::try_from(pid).map_err(|_| Error::Denied("No such process".into()))?,
                    )),
                    None => None,
                };
                let time = match (explicit_pid, time) {
                    (Some(_), Some(time)) => Some(time?),
                    (None, Some(time)) => time.ok(), // An ended harness may already be reaped.
                    (_, None) => None,
                };
                self.with_store(|store| {
                    let seq = usage::record(
                        store,
                        &usage::Record {
                            campaign: s(args, "campaign")?,
                            role: s(args, "role")?,
                            task: task.unwrap_or(""),
                            run: args.get("run").and_then(Value::as_str).unwrap_or(""),
                            tokens: &tokens,
                            time: time.as_ref(),
                            outcome: args.get("outcome").and_then(Value::as_str).unwrap_or(""),
                        },
                    )?;
                    Ok(json!({"seq":seq,"tokens":tokens}))
                })?
            }
            _ => return Err(Error::Unimplemented(UNBUILT)),
        })
    }
    fn campaign_up(&self, args: &Value) -> Result<Value> {
        let name = s(args, "name")?;
        let root = path(args, "root")?;
        if !root.is_dir() {
            return Err(Error::Denied("The campaign folder does not exist".into()));
        }
        let root = std::fs::canonicalize(&root).map_err(|e| Error::io(&root, e))?;
        let now = chrono::Local::now().fixed_offset();
        let text = |key: &str, default: String| {
            args.get(key)
                .and_then(Value::as_str)
                .map_or(default, str::to_owned)
        };
        let campaign = self.registry.prepared(Campaign {
            name: name.into(),
            state: args
                .get("state")
                .and_then(Value::as_str)
                .map_or_else(|| root.join("state"), PathBuf::from),
            session: text(
                "session",
                if name.starts_with("qream") {
                    name.into()
                } else {
                    format!("qream-{name}")
                },
            ),
            mode: text("mode", "waves".into()),
            title: text("title", name.into()),
            division: args
                .get("division")
                .and_then(Value::as_str)
                .map(str::to_owned),
            status: "up".into(),
            root: root.clone(),
            created: now,
            updated: now,
        })?;
        // The campaign's session journals live in the daemon's home, never in the folder.
        self.sessions.lock().up(&campaign.session, &root)?;
        self.start_recorder(name, &root);
        Ok(serde_json::to_value(campaign)?)
    }
    fn campaign_down(&self, name: &str) -> Result<Value> {
        let campaign = self.registry.down(name)?;
        if let Some(recorder) = self.recorders.lock().remove(name) {
            recorder.abort();
        }
        // The last recording is taken as the campaign goes down.
        let _ = record_replay(&campaign.root);
        let mut sessions = self.sessions.lock();
        if sessions
            .list()
            .iter()
            .any(|s| s.name == campaign.session && s.up)
        {
            sessions.down(&campaign.session)?;
        }
        Ok(serde_json::to_value(campaign)?)
    }
    /// End the daemon: each session not kept goes down first (its panes close and their
    /// screens are journaled); kept sessions are journaled and restore on the next start.
    fn daemon_stop(&self, args: &Value) -> Result<Value> {
        let keep: BTreeSet<String> = words(args, "keep").into_iter().collect();
        let keep_all = args.get("keep_all").and_then(Value::as_bool) == Some(true);
        let mut sessions = self.sessions.lock();
        sessions.poll_changes()?;
        sessions.flush()?;
        let mut down = Vec::new();
        let mut kept = Vec::new();
        for session in sessions.list().into_iter().filter(|s| s.up) {
            if keep_all || keep.contains(&session.name) {
                kept.push(session.name);
            } else {
                sessions.down(&session.name)?;
                down.push(session.name);
            }
        }
        Ok(json!({"stopping":true,"down":down,"kept":kept}))
    }
    fn lint(&self, args: &Value) -> Result<Value> {
        let target = path(args, "path")?;
        self.with_store(|store| {
            let mut options = LintOptions::new(store)?;
            options.campaign_root = args
                .get("campaign_root")
                .and_then(Value::as_str)
                .map(PathBuf::from);
            options.charter = args
                .get("charter")
                .and_then(Value::as_str)
                .map(PathBuf::from);
            options.lanes = args.get("lanes").and_then(Value::as_str).map(PathBuf::from);
            if let Some(extra) = args.get("rules").and_then(Value::as_str) {
                options.extra_rules(Path::new(extra))?;
            }
            Ok(serde_json::to_value(rules::lint(&target, &options)?)?)
        })
    }
    fn setup(&self, cmd: &str, args: &Value) -> Result<Value> {
        let root = || path(args, "root");
        Ok(match cmd {
            "setup.show" => serde_json::to_value(setup::show(&root()?)?)?,
            "setup.draft.get" => serde_json::to_value(setup::draft(&root()?)?)?,
            "setup.move" => serde_json::to_value(setup::move_task(
                &root()?,
                s(args, "task")?,
                u32::try_from(n(args, "wave")?)
                    .map_err(|_| Error::Denied("No such wave".into()))?,
            )?)?,
            "setup.manifest" => {
                let tasks: BTreeMap<String, Route> = match args.get("tasks") {
                    Some(Value::Object(_)) => serde_json::from_value(args["tasks"].clone())?,
                    Some(Value::String(file)) => crate::folder::json(Path::new(file))?,
                    _ => BTreeMap::new(),
                };
                serde_json::to_value(setup::manifest(
                    &root()?,
                    u32::try_from(n(args, "wave")?)
                        .map_err(|_| Error::Denied("No such wave".into()))?,
                    &Route {
                        kind: s(args, "kind")?.into(),
                        model: s(args, "model")?.into(),
                        effort: s(args, "effort")?.into(),
                    },
                    &tasks,
                )?)?
            }
            "setup.new" => self.with_store(|store| {
                Ok(serde_json::to_value(setup::new(
                    &root()?,
                    s(args, "name")?,
                    &Scope {
                        rows: words(args, "rows"),
                        groups: words(args, "groups"),
                    },
                    store,
                )?)?)
            })?,
            "setup.check" => {
                self.with_store(|store| Ok(serde_json::to_value(setup::check(&root()?, store)?)?))?
            }
            "setup.add_directive" => self.with_store(|store| {
                Ok(serde_json::to_value(setup::add_directive(
                    &root()?,
                    u32::try_from(n(args, "wave")?)
                        .map_err(|_| Error::Denied("No such wave".into()))?,
                    s(args, "task")?,
                    &path(args, "file")?,
                    store,
                    store,
                )?)?)
            })?,
            "setup.confirm" => {
                let root = root()?;
                let root = std::fs::canonicalize(&root).map_err(|e| Error::io(&root, e))?;
                let close = match args.get("close") {
                    Some(close) if close.is_object() => Some(crate::folder::Close {
                        // The registered name, then the manifests' name, then the folder's.
                        campaign: self
                            .registry
                            .list()?
                            .into_iter()
                            .find(|c| c.root == root)
                            .map(|c| c.name)
                            .or_else(|| {
                                crate::folder::Campaign::open(&root)
                                    .ok()?
                                    .waves
                                    .iter()
                                    .find_map(|w| w.manifest.as_ref().map(|m| m.campaign.clone()))
                            })
                            .filter(|n| !n.is_empty())
                            .unwrap_or_else(|| {
                                root.file_name()
                                    .unwrap_or_default()
                                    .to_string_lossy()
                                    .into()
                            }),
                        last_wave: u32::try_from(n(close, "last_wave")?)
                            .map_err(|_| Error::Denied("No such wave".into()))?,
                        expected_close: s(close, "expected_close")?.into(),
                        declared_at: chrono::Local::now().to_rfc3339(),
                        by: close
                            .get("by")
                            .and_then(Value::as_str)
                            .unwrap_or("planning-agent")
                            .into(),
                        rule: "declared before trigger".into(),
                        actual_close: None,
                    }),
                    _ => None,
                };
                let mut confirmed = self.with_store(|store| setup::confirm(&root, close, store))?;
                if confirmed["ok"] == true {
                    let name = s(&confirmed["close"], "campaign")?;
                    let mut registration = self
                        .registry
                        .list()?
                        .into_iter()
                        .find(|c| c.root == root)
                        .map(serde_json::to_value)
                        .transpose()?
                        .unwrap_or_else(|| json!({"name":name}));
                    registration["root"] = json!(root);
                    confirmed["campaign"] = self.campaign_up(&registration)?;
                }
                confirmed
            }
            _ => return Err(Error::Unimplemented(UNBUILT)),
        })
    }
    fn term(&self, cmd: &str, args: &Value, connection: u64) -> Result<Value> {
        let session = || s(args, "session");
        Ok(match cmd {
            "term.sessions" => serde_json::to_value(self.sessions.lock().list())?,
            "term.session.up" => serde_json::to_value(
                self.sessions
                    .lock()
                    .up(s(args, "name")?, &path(args, "cwd")?)?,
            )?,
            "term.session.down" => {
                self.sessions.lock().down(s(args, "name")?)?;
                json!({"down":s(args, "name")?})
            }
            "term.workspaces" => {
                serde_json::to_value(self.sessions.lock().workspaces(session()?)?)?
            }
            "term.workspace.new" => serde_json::to_value(self.sessions.lock().new_workspace(
                session()?,
                s(args, "label")?,
                &path(args, "cwd")?,
                env(args),
            )?)?,
            "term.pane.new" => {
                let split = match args.get("split") {
                    Some(split) if split.is_object() => Some(Split {
                        of: s(split, "of")?.into(),
                        dir: if split.get("dir").and_then(Value::as_str) == Some("vertical") {
                            Direction::Vertical
                        } else {
                            Direction::Horizontal
                        },
                        ratio: split.get("ratio").and_then(Value::as_f64).unwrap_or(0.5) as f32,
                    }),
                    _ => None,
                };
                serde_json::to_value(self.sessions.lock().new_pane(
                    session()?,
                    s(args, "workspace")?,
                    &path(args, "cwd")?,
                    env(args),
                    split,
                )?)?
            }
            "term.pane.close" => {
                self.sessions.lock().close(s(args, "pane")?)?;
                json!({"closed":s(args, "pane")?})
            }
            "term.pane.attach" => {
                let pane_id = s(args, "pane")?;
                // Keep the launch identity and subscription in the same session transaction.
                let sessions = self.sessions.lock();
                let launch = sessions.journal(pane_id)?.launch.as_ref();
                let pane = self.live.get(pane_id)?;
                let started_at = launch.map(|record| record.started_at.as_str());
                let generation = format!(
                    "{}:{}",
                    started_at.unwrap_or_default(),
                    pane.pid().map(|pid| pid.to_string()).unwrap_or_default()
                );
                let mut viewer = pane.subscribe(size(args, "cols")?, size(args, "rows")?);
                let screen = viewer.full();
                let id = format!("v{}", self.next_viewer.fetch_add(1, Ordering::Relaxed));
                self.viewers.lock().insert(
                    id.clone(),
                    Attached {
                        pane: pane_id.into(),
                        connection,
                        viewer,
                    },
                );
                let mut reply = json!({"viewer":id,"pane":pane_id,"screen":screen,
                    "generation":generation,"started_at":started_at});
                drop(sessions);
                mask_texts(&mut reply["screen"]);
                reply
            }
            "term.pane.detach" => {
                let id = s(args, "viewer")?;
                self.viewers
                    .lock()
                    .remove(id)
                    .ok_or_else(|| Error::Denied("No such viewer".into()))?;
                json!({"detached":id})
            }
            "term.pane.resize" => {
                let (cols, rows) = (size(args, "cols")?, size(args, "rows")?);
                let pane = {
                    let mut viewers = self.viewers.lock();
                    let attached = viewers
                        .get_mut(s(args, "viewer")?)
                        .ok_or_else(|| Error::Denied("No such viewer".into()))?;
                    attached.viewer.set_size(cols, rows);
                    attached.pane.clone()
                };
                // An explicit resize from a viewer is the operator's; the latest one wins.
                self.sessions.lock().resize(&pane, cols, rows)?;
                json!({"pane":pane,"cols":cols,"rows":rows})
            }
            "term.pane.scroll" => {
                let mut viewers = self.viewers.lock();
                let attached = viewers
                    .get_mut(s(args, "viewer")?)
                    .ok_or_else(|| Error::Denied("No such viewer".into()))?;
                attached.viewer.scroll(n(args, "offset")? as usize);
                let mut reply =
                    json!({"offset":attached.viewer.offset(),"screen":attached.viewer.full()});
                mask_texts(&mut reply["screen"]);
                reply
            }
            "term.pane.input" => {
                let bytes = decode_base64(s(args, "bytes_b64")?)?;
                self.live.get(s(args, "pane")?)?.write(&bytes)?;
                json!({"written":bytes.len()})
            }
            "term.pane.text" => {
                let lines = args.get("lines").and_then(Value::as_u64).unwrap_or(50) as usize;
                let pane = s(args, "pane")?;
                let text = match self.live.get(pane) {
                    Ok(live) => live.text(lines),
                    Err(_) => {
                        let sessions = self.sessions.lock();
                        let screen = &sessions.journal(pane)?.screen;
                        let all: Vec<&String> =
                            screen.scrollback.iter().chain(&screen.lines).collect();
                        all[all.len().saturating_sub(lines)..]
                            .iter()
                            .map(|l| l.as_str())
                            .collect::<Vec<_>>()
                            .join("\n")
                    }
                };
                json!({"pane":pane,"text":Mask::house().apply(&text)})
            }
            "term.agent.get" => {
                serde_json::to_value(self.sessions.lock().observe(s(args, "pane")?)?)?
            }
            "term.agent.list" => {
                let sessions = self.sessions.lock();
                let agents: Vec<Value> = sessions
                    .panes(session()?)?
                    .into_iter()
                    .filter(|j| j.launch.is_some())
                    .map(|j| json!({"pane":j.spec.id,"task":j.spec.env.get("QREAM_TASK"),"agent":j.agent,"lost":j.lost}))
                    .collect();
                Value::Array(agents)
            }
            "term.agent.prompt" => {
                self.sessions
                    .lock()
                    .prompt(s(args, "pane")?, s(args, "text")?)?;
                json!({"prompted":s(args, "pane")?})
            }
            _ => return Err(Error::Unimplemented(UNBUILT)),
        })
    }
    async fn dispatch(&self, request: &Request, connection: u64) -> Result<Value> {
        let args = &request.args;
        let name = || {
            args.get("name")
                .or_else(|| args.get("campaign"))
                .and_then(Value::as_str)
                .ok_or_else(|| Error::Denied("Name the campaign".into()))
        };
        let cmd = request.cmd.as_str();
        if self.read_only && writes(cmd) {
            return Err(Error::ReadOnly);
        }
        match cmd {
            "ping" => Ok(json!({"build":self.build})),
            "campaigns.list" => self.campaign_list(),
            "campaign.get" => Ok(serde_json::to_value(self.registry.get(name()?)?)?),
            "campaign.scope" => {
                let campaign = self.registry.get(name()?)?;
                self.with_store(|store| {
                    setup::campaign_scope(&campaign.root, &campaign.name, store)
                })
            }
            "campaign.up" => self.campaign_up(args),
            "campaign.down" => self.campaign_down(name()?),
            "close.get" | "campaign.close" => Ok(serde_json::to_value(Close::read(
                &self.registry.get(name()?)?.root,
            )?)?),
            "controller.status" | "run.status" => {
                let name = name()?;
                let state = self.campaign_state(name).await?;
                self.status_of(name, &state)
            }
            "controller.start" => {
                self.engine(cmd, args)?;
                let name = name()?;
                let state = self.campaign_state(name).await?;
                self.status_of(name, &state)
            }
            "states" => Ok(serde_json::to_value(self.campaign_state(name()?).await?)?),
            "campaign.control" => self.campaign_control(name()?, args),
            "recover" => self.recover(args),
            "refresh" => {
                s(args, "task")?;
                self.recover(&json!({"campaign":name()?,"task":s(args, "task")?}))
            }
            "leftovers" => {
                // The stop engine's owned processes, then what still lives under a stopped seat.
                let mut found = self.leftovers(args)?;
                if let Value::Object(map) = &mut found {
                    map.insert("seat".into(), seat::dispatch(self, cmd, args)?);
                }
                Ok(found)
            }
            "run.accept" => {
                let campaign = args
                    .get("campaign")
                    .or_else(|| args.get("run"))
                    .and_then(Value::as_str)
                    .ok_or_else(|| Error::Denied("Name the campaign".into()))?;
                // No acceptance while a process the task started is alive; then the
                // controller writes `accepted` and the row status.
                self.accept_guard(campaign, s(args, "task")?)?;
                let result = self.engine(cmd, args)?;
                self.record_acceptance(campaign, s(args, "task")?)?;
                self.after_engine(cmd, args).await;
                Ok(result)
            }
            "term.agent.start" => self.agent_start(args).await,
            "daemon.stop" => self.daemon_stop(args),
            "lint" => self.lint(args),
            "store.events" => self.with_store(|st| {
                // Read-only, from the store crate; filtered by ids and an upper `until` time.
                let since = args.get("since").and_then(Value::as_u64).unwrap_or(0);
                let ids: Option<BTreeSet<String>> =
                    args.get("ids").and_then(Value::as_array).map(|a| {
                        a.iter()
                            .filter_map(Value::as_str)
                            .map(str::to_owned)
                            .collect()
                    });
                let until = args.get("until").and_then(Value::as_str);
                let events: Vec<Value> = st
                    .events_since(since)?
                    .into_iter()
                    .filter(|e| {
                        ids.as_ref().is_none_or(|ids| {
                            e.get("id")
                                .and_then(Value::as_str)
                                .is_some_and(|i| ids.contains(i))
                        })
                    })
                    .filter(|e| {
                        until.is_none_or(|u| {
                            e.get("at")
                                .and_then(Value::as_str)
                                .is_some_and(|at| at <= u)
                        })
                    })
                    .map(Value::Object)
                    .collect();
                Ok(Value::Array(events))
            }),
            "store.revision" => self.with_store(|st| Ok(json!({"revision":st.revision()?}))),
            "store.row" => self.with_store(|st| Ok(Value::Object(st.get(s(args, "id")?)?))),
            "store.members" => self.with_store(|st| Ok(json!(st.members(s(args, "group")?)?))),
            "store.rows" => self.with_store(|st| {
                let text = |k: &str| args.get(k).and_then(Value::as_str);
                Ok(serde_json::to_value(st.list(
                    text("tab"),
                    text("status"),
                    text("text"),
                    args.get("limit").and_then(Value::as_u64).unwrap_or(100) as usize,
                    args.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize,
                )?)?)
            }),
            "notice" => {
                let line = masked(args, "line").trim().to_owned();
                if line.is_empty() {
                    return Err(Error::Denied("A notice needs a line of words".into()));
                }
                let _ = self.events.send(json!({"v":1,"event":"notice","at":chrono::Local::now().to_rfc3339(),"data":{"message":line}}));
                Ok(json!({"message":line}))
            }
            "submit" => {
                // A report holding an unmasked secret is refused before the seal or delivery.
                let report = path(args, "report")?;
                let lines = mask::gate_report(&report)?;
                if args.get("campaign").and_then(Value::as_str).is_none()
                    || args.get("task").and_then(Value::as_str).is_none()
                {
                    return Err(Error::Denied(
                        "Name the campaign and the task (or submit from the campaign's report folder)"
                            .into(),
                    ));
                }
                let mut result = self.engine(cmd, args)?;
                result["lines"] = json!(lines);
                self.after_engine(cmd, args).await;
                Ok(result)
            }
            _ if cmd.starts_with("faults.")
                || cmd.starts_with("replay.")
                || cmd.starts_with("records.") =>
            {
                self.records(cmd, args)
            }
            // The seat's and every worker's conversation are read by the seat module.
            "conversation.get" => seat::dispatch(self, cmd, args),
            "sessions.recent" => seat::dispatch(self, cmd, args),
            _ if cmd.starts_with("seat.") => seat::dispatch(self, cmd, args),
            _ if cmd.starts_with("setup.") => self.setup(cmd, args),
            _ if cmd.starts_with("term.") => self.term(cmd, args, connection),
            _ if crate::live::COMMANDS.contains(&cmd) => {
                let result = self.engine(cmd, args)?;
                if cmd == "gapfill" {
                    self.record_acceptance(s(args, "campaign")?, s(args, "task")?)?;
                }
                self.after_engine(cmd, args).await;
                Ok(result)
            }
            _ => Err(Error::Unimplemented(UNBUILT)),
        }
    }
    fn record_acceptance(&self, campaign: &str, task: &str) -> Result<()> {
        let root = self.registry.get(campaign)?.root;
        let ledger = crate::ledger::Ledger::new(&root).read()?;
        if ledger
            .iter()
            .rev()
            .find(|t| t.campaign == campaign && t.task == task)
            .is_some_and(|t| t.to == "accepted")
        {
            self.with_store(|store| duration::accepted(store, &root, campaign, task))?;
        }
        Ok(())
    }
    /// Bind usage to the pane's exact native history, falling back to retained replay bindings.
    fn usage_session(&self, args: &Value) -> Result<Option<PathBuf>> {
        if let Some(path) = args.get("session").and_then(Value::as_str) {
            return Ok(Some(PathBuf::from(path)));
        }
        let Some(task) = args.get("task").and_then(Value::as_str) else {
            return Ok(None);
        };
        let campaign = self.registry.get(s(args, "campaign")?)?;
        let launch = {
            let sessions = self.sessions.lock();
            sessions
                .panes(&campaign.session)
                .ok()
                .and_then(|panes| {
                    panes.into_iter().find(|p| {
                        !p.closed && p.spec.env.get("QREAM_TASK").is_some_and(|t| t == task)
                    })
                })
                .and_then(|p| p.launch.clone().map(|launch| (p.spec.cwd.clone(), launch)))
        };
        if let Some((cwd, launch)) = launch {
            let home = std::env::var_os("HOME")
                .map(PathBuf::from)
                .ok_or_else(|| Error::Denied("The user home is unavailable".into()))?;
            if let Some(path) = crate::conversation::find(
                &home,
                launch.spec.kind,
                &cwd,
                &launch.started_at,
                launch.conversation_id.as_deref(),
            )? {
                return Ok(Some(path));
            }
            // Never substitute another history when the pane has a native identity.
            return Err(Error::Denied(format!(
                "{task}'s harness has written no session yet"
            )));
        }
        let bindings_file = campaign.root.join("state/replay/bindings.json");
        let bindings: Vec<replay::Binding> = if bindings_file.is_file() {
            crate::storage::read(&bindings_file)?
        } else {
            let (codex, claude) = replay::sources()?;
            replay::discover(&campaign.root, &codex, &claude)?
        };
        let path = bindings
            .into_iter()
            .filter(|b| b.task == task && b.path.is_file())
            .max_by_key(|b| std::fs::metadata(&b.path).and_then(|m| m.modified()).ok())
            .map(|b| b.path);
        path.map(Some)
            .ok_or_else(|| Error::Denied(format!("{task} has no bound native session for usage")))
    }
    pub async fn serve(self: Arc<Self>, path: &Path) -> Result<()> {
        let parent = path
            .parent()
            .ok_or_else(|| Error::Denied("Socket needs a parent directory".into()))?;
        std::fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
        if UnixStream::connect(path).await.is_ok() {
            return Err(Error::Denied(
                "A daemon already answers on this socket".into(),
            ));
        }
        // A socket file nobody answers on is left by a daemon that died.
        if path.exists() {
            std::fs::remove_file(path).map_err(|e| Error::io(path, e))?;
        }
        let listener = UnixListener::bind(path).map_err(|e| Error::io(path, e))?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| Error::io(path, e))?;
        let mut stop = self.stop.subscribe();
        let mut clients = tokio::task::JoinSet::new();
        let bridge = tokio::spawn(Arc::clone(&self).bridge_terminal_events());
        let ticker = tokio::spawn(Arc::clone(&self).tick());
        loop {
            while clients.try_join_next().is_some() {}
            match race(stop.changed(), listener.accept()).await {
                Either::First(changed) => {
                    if changed.is_err() || *stop.borrow() {
                        break;
                    }
                }
                Either::Second(accepted) => {
                    let (stream, _) = accepted.map_err(|e| Error::io(path, e))?;
                    let api = self.clone();
                    let socket = path.to_owned();
                    clients.spawn(async move { api.connection(stream, &socket).await });
                }
            }
        }
        bridge.abort();
        ticker.abort();
        clients.abort_all();
        while clients.join_next().await.is_some() {}
        std::fs::remove_file(path).map_err(|e| Error::io(path, e))?;
        Ok(())
    }
    /// The terminal layer's session, pane and agent events become socket events.
    async fn bridge_terminal_events(self: Arc<Self>) {
        let mut events = self.sessions.lock().events.subscribe();
        loop {
            match events.recv().await {
                Ok(event) => {
                    if event.event.starts_with("agent") || event.event.starts_with("session") {
                        self.dirty.store(true, Ordering::Relaxed);
                    }
                    let _ = self
                        .events
                        .send(serde_json::to_value(event).unwrap_or(Value::Null));
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => return,
            }
        }
    }
    async fn tick(self: Arc<Self>) {
        let mut beat = 0u32;
        let mut registry = self.registry.changes();
        loop {
            match race(
                registry.changed(),
                tokio::time::sleep(Duration::from_millis(100)),
            )
            .await
            {
                Either::First(Ok(())) => {
                    self.publish_registry_changes();
                    continue;
                }
                Either::First(Err(_)) => return,
                Either::Second(()) => {}
            }
            self.pump_viewers();
            beat = beat.wrapping_add(1);
            if beat.is_multiple_of(10) {
                if let Err(e) = self.tick_controls() {
                    let _ = self.events.send(json!({"v":1,"event":"fault","at":chrono::Local::now().to_rfc3339(),"data":{"message":e.to_string()}}));
                }
                let polled = self.poll();
                if !self.pump_runners().is_empty() {
                    self.dirty.store(true, Ordering::Relaxed);
                }
                self.publish_changes().await;
                self.publish_store();
                if let Err(e) = polled {
                    let _ = self.events.send(json!({"v":1,"event":"fault","at":chrono::Local::now().to_rfc3339(),"data":{"message":e.to_string()}}));
                }
                if self.idle_expired() {
                    self.request_stop();
                }
            }
        }
    }
    async fn connection(&self, stream: UnixStream, path: &Path) -> Result<()> {
        let id = self.next_connection.fetch_add(1, Ordering::Relaxed);
        let result = self.converse(stream, path, id).await;
        // A viewer lives as long as the connection that attached it.
        self.viewers.lock().retain(|_, a| a.connection != id);
        self.watchers.lock().remove(&id);
        result
    }
    async fn converse(&self, stream: UnixStream, path: &Path, connection: u64) -> Result<()> {
        let (read, mut write) = stream.into_split();
        let mut reader = BufReader::new(read);
        send(&mut write, &self.hello(), path).await?;
        let mut events = self.events.subscribe();
        let mut topics = BTreeSet::new();
        let mut pending = vec![];
        loop {
            match race(reader.fill_buf(), events.recv()).await {
                Either::First(data) => {
                    let data = data.map_err(|e| Error::io(path, e))?;
                    if data.is_empty() {
                        return Ok(());
                    }
                    let end = data.iter().position(|b| *b == b'\n');
                    let n = end.map_or(data.len(), |n| n + 1);
                    if pending.len() + n > MAX_LINE {
                        send(
                            &mut write,
                            &failure("", "too_large", "A request exceeds one MiB"),
                            path,
                        )
                        .await?;
                        return Ok(());
                    }
                    pending.extend_from_slice(&data[..n]);
                    reader.consume(n);
                    if end.is_none() {
                        continue;
                    }
                    let request = serde_json::from_slice::<Request>(&pending);
                    pending.clear();
                    let request = match request {
                        Ok(r) => r,
                        Err(_) => {
                            send(
                                &mut write,
                                &failure(
                                    "",
                                    "invalid",
                                    "Send a JSON request with a version, id and command",
                                ),
                                path,
                            )
                            .await?;
                            continue;
                        }
                    };
                    let reply = if request.v != 1 {
                        failure(
                            &request.id,
                            "version",
                            "Only protocol version one is supported",
                        )
                    } else if request.cmd == "subscribe" {
                        match request.args.get("topics").and_then(Value::as_array) {
                            Some(values) if values.iter().all(Value::is_string) => {
                                topics = values
                                    .iter()
                                    .filter_map(Value::as_str)
                                    .map(str::to_owned)
                                    .collect();
                                self.watchers.lock().insert(connection);
                                json!({"v":1,"id":request.id,"ok":true,"result":{"subscribed":topics}})
                            }
                            _ => failure(
                                &request.id,
                                "invalid",
                                "Name the event topics as a list of words",
                            ),
                        }
                    } else if request.cmd == "unsubscribe" {
                        topics.clear();
                        self.watchers.lock().remove(&connection);
                        json!({"v":1,"id":request.id,"ok":true,"result":{}})
                    } else {
                        match self.dispatch(&request, connection).await {
                            Ok(result) => {
                                if writes(&request.cmd) {
                                    self.publish_registry_changes();
                                }
                                json!({"v":1,"id":request.id,"ok":true,"result":result})
                            }
                            Err(Error::Unimplemented(reason)) => {
                                failure(&request.id, "unimplemented", reason)
                            }
                            Err(e) => failure(&request.id, e.code(), &e.to_string()),
                        }
                    };
                    let stopping = request.v == 1
                        && request.cmd == "daemon.stop"
                        && reply.get("ok") == Some(&Value::Bool(true));
                    send(&mut write, &reply, path).await?;
                    if stopping {
                        self.request_stop();
                        return Ok(());
                    }
                }
                Either::Second(event) => match event {
                    Ok(event) => {
                        if event
                            .get("event")
                            .and_then(Value::as_str)
                            .is_some_and(|t| topics.contains(t))
                        {
                            send(&mut write, &event, path).await?;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        send(&mut write,&json!({"v":1,"event":"notice","at":chrono::Local::now().to_rfc3339(),"data":{"message":"State events were missed; read states again"}}),path).await?;
                    }
                    Err(broadcast::error::RecvError::Closed) => return Ok(()),
                },
            }
        }
    }
}
/// Citium, OVRIN or Zavent from an explicit field or the root under 02B, 02A or 02C.
fn division(campaign: &Campaign) -> String {
    if let Some(division) = &campaign.division {
        return division.clone();
    }
    let root = campaign.root.to_string_lossy();
    [("/02A_", "OVRIN"), ("/02B_", "Citium"), ("/02C_", "Zavent")]
        .iter()
        .find(|(prefix, _)| root.contains(prefix))
        .map_or("none", |(_, name)| name)
        .into()
}
/// Commands that change a record, a session or a pane; refused when read-only.
fn writes(cmd: &str) -> bool {
    crate::live::WRITES.contains(&cmd)
        || matches!(
            cmd,
            "campaign.up"
                | "campaign.down"
                | "campaign.trigger"
                | "campaign.control"
                | "recover"
                | "refresh"
                | "leftovers"
                | "run.accept"
                | "setup.new"
                | "setup.add_directive"
                | "setup.move"
                | "setup.manifest"
                | "setup.confirm"
                | "term.session.up"
                | "term.session.down"
                | "term.workspace.new"
                | "term.pane.new"
                | "term.pane.close"
                | "term.pane.input"
                | "term.pane.resize"
                | "term.agent.start"
                | "term.agent.prompt"
                | "faults.add"
                | "faults.amend"
                | "replay.record"
                | "records.duration"
                | "records.usage"
                | "seat.open"
                | "seat.invoke"
        )
}
/// Every `text` string inside a screen or its damage rows, masked in place.
/// A row's colour runs are masked as one line: a secret split across two runs never passes.
fn mask_texts(value: &mut Value) {
    match value {
        Value::Object(map) if map.get("runs").is_some_and(Value::is_array) => {
            mask_row(map);
            for (key, item) in map.iter_mut() {
                if key != "runs" {
                    mask_texts(item);
                }
            }
        }
        Value::Object(map) => {
            for (key, item) in map.iter_mut() {
                match item {
                    Value::String(text) if key == "text" => *text = Mask::house().apply(text),
                    _ => mask_texts(item),
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(mask_texts),
        _ => {}
    }
}
/// Masks the whole row's text; a row holding a secret becomes one run in its first run's style.
fn mask_row(row: &mut serde_json::Map<String, Value>) {
    let Some(Value::Array(runs)) = row.get_mut("runs") else {
        return;
    };
    let whole: String = runs
        .iter()
        .filter_map(|r| r.get("text").and_then(Value::as_str))
        .collect();
    if Mask::house().findings(&whole).is_empty() {
        return;
    }
    let mut first = runs.first().cloned().unwrap_or_else(|| json!({}));
    first["text"] = json!(Mask::house().apply(&whole));
    *runs = vec![first];
}
/// One recording of the campaign's session files into its `state/replay/`.
fn record_replay(root: &Path) -> Result<BTreeMap<String, usize>> {
    let (codex, claude) = replay::sources()?;
    replay::record(root, &replay::discover(root, &codex, &claude)?)
}
/// Text fields of a record, masked before they are written.
fn masked(args: &Value, key: &str) -> String {
    Mask::house().apply(args.get(key).and_then(Value::as_str).unwrap_or(""))
}
fn local_time(args: &Value, key: &str) -> Result<chrono::DateTime<chrono::Local>> {
    chrono::DateTime::parse_from_rfc3339(s(args, key)?)
        .map(|t| t.with_timezone(&chrono::Local))
        .map_err(|_| Error::Denied(format!("Give the {key} as an RFC 3339 time")))
}
fn failure(id: &str, code: &str, message: &str) -> Value {
    json!({"v":1,"id":id,"ok":false,"error":{"code":code,"message":message}})
}
async fn send(
    write: &mut tokio::net::unix::OwnedWriteHalf,
    value: &Value,
    path: &Path,
) -> Result<()> {
    let mut line = serde_json::to_vec(value)?;
    line.push(b'\n');
    write.write_all(&line).await.map_err(|e| Error::io(path, e))
}

/// Standard base64 (RFC 4648) for typed input; padding optional.
/// The tracker store, opened lazily on first use and shared with the controllers.
#[derive(Clone)]
pub struct StoreHandle {
    pub path: Option<PathBuf>,
    pub read_only: bool,
    store: Arc<Mutex<Option<Store>>>,
}
impl StoreHandle {
    pub fn new(path: Option<PathBuf>) -> Self {
        Self {
            path,
            read_only: false,
            store: Arc::new(Mutex::new(None)),
        }
    }
    pub fn with<T>(&self, f: impl FnOnce(&Store) -> Result<T>) -> Result<T> {
        let mut guard = self.store.lock();
        if guard.is_none() {
            let path = self
                .path
                .as_ref()
                .ok_or_else(|| Error::Denied("This daemon has no tracker store".into()))?;
            *guard = Some(if self.read_only {
                Store::open_read_only(path)?
            } else {
                Store::open(path, "qream-controller")?
            });
        }
        let store = guard
            .as_ref()
            .ok_or_else(|| Error::Denied("The tracker store did not open".into()))?;
        f(store)
    }
}

pub fn decode_base64(text: &str) -> Result<Vec<u8>> {
    let value = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        } as u32)
    };
    let bytes: Vec<u8> = text.bytes().filter(|b| *b != b'=').collect();
    let mut out = Vec::with_capacity(bytes.len() * 3 / 4);
    for chunk in bytes.chunks(4) {
        let mut acc = 0u32;
        for (i, c) in chunk.iter().enumerate() {
            let v = value(*c).ok_or_else(|| Error::Denied("The input is not base64".into()))?;
            acc |= v << (18 - 6 * i);
        }
        let take = match chunk.len() {
            4 => 3,
            3 => 2,
            2 => 1,
            _ => return Err(Error::Denied("The input is not base64".into())),
        };
        out.extend_from_slice(&acc.to_be_bytes()[1..1 + take]);
    }
    Ok(out)
}
/// Standard base64 with padding, for the command line's typed input.
pub fn encode_base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let acc = chunk
            .iter()
            .enumerate()
            .fold(0u32, |acc, (i, b)| acc | u32::from(*b) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(TABLE[(acc >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

enum Either<A, B> {
    First(A),
    Second(B),
}
async fn race<A: std::future::Future, B: std::future::Future>(
    a: A,
    b: B,
) -> Either<A::Output, B::Output> {
    let mut a = std::pin::pin!(a);
    let mut b = std::pin::pin!(b);
    std::future::poll_fn(|cx| {
        if let std::task::Poll::Ready(value) = a.as_mut().poll(cx) {
            return std::task::Poll::Ready(Either::First(value));
        }
        if let std::task::Poll::Ready(value) = b.as_mut().poll(cx) {
            return std::task::Poll::Ready(Either::Second(value));
        }
        std::task::Poll::Pending
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn base64_round_trips() {
        for text in ["", "a", "ab", "abc", "echo hello\r"] {
            assert_eq!(
                decode_base64(&encode_base64(text.as_bytes())).expect("valid"),
                text.as_bytes()
            );
        }
        assert_eq!(encode_base64(b"abc"), "YWJj");
    }
}

#[cfg(test)]
mod row_mask {
    #[test]
    fn a_secret_split_across_two_runs_is_masked() {
        let key = format!("sk-ant-api03-{}", "Abcdefghijklmnopqrstuvwxyz012345");
        let (a, b) = key.split_at(10);
        let mut rows = serde_json::json!([{"y":0,"runs":[{"text":format!("key {a}"),"fg":"red"},{"text":format!("{b} end"),"fg":"blue"}]}]);
        super::mask_texts(&mut rows);
        let text = rows.to_string();
        assert!(!text.contains(b), "{text}");
        assert!(text.contains("[masked]"), "{text}");
    }
}
