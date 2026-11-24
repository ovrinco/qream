use crate::{
    Error, Result,
    agent::{self, AgentInfo, AgentSpec, LaunchRecord},
    detect::Detector,
    error::io,
    events::{EventBus, now},
    handle::{PaneFactory, PaneHandle, PaneSpec, Recovery, Screen, Split},
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};
use tokio::sync::broadcast;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Workspace {
    pub id: String,
    pub label: String,
    pub cwd: PathBuf,
    pub env: BTreeMap<String, String>,
    pub focused_pane: Option<String>,
    pub next_pane: u64,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub name: String,
    pub cwd: PathBuf,
    pub up: bool,
    pub workspaces: Vec<Workspace>,
    pub panes: Vec<String>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PaneJournal {
    pub spec: PaneSpec,
    pub screen: Screen,
    pub launch: Option<LaunchRecord>,
    pub agent: Option<AgentInfo>,
    pub closed: bool,
    pub lost: Option<String>,
}
struct Runtime {
    handle: Box<dyn PaneHandle>,
    changes: broadcast::Receiver<Screen>,
}
/// Home is QREAM's home; journal paths are home/sessions/<name>/.
/// The daemon calls poll_changes at least once per second, independent of viewers.
/// Dropping a viewer must not drop this store. The core supplies durable process ownership.
pub struct SessionStore {
    home: PathBuf,
    sessions: BTreeMap<String, Session>,
    journals: BTreeMap<String, PaneJournal>,
    runtime: BTreeMap<String, Runtime>,
    factory: Box<dyn PaneFactory>,
    pub events: EventBus,
}
impl SessionStore {
    pub fn open(
        home: impl Into<PathBuf>,
        factory: Box<dyn PaneFactory>,
        events: EventBus,
    ) -> Result<Self> {
        let home = home.into();
        let dir = home.join("sessions");
        // A read-only daemon (`QREAM_READ_ONLY=1`) reads the journals and creates nothing.
        if std::env::var_os("QREAM_READ_ONLY").is_none_or(|v| v != "1") {
            fs::create_dir_all(&dir).map_err(|e| io(&dir, e))?;
        }
        let mut store = Self {
            home,
            sessions: BTreeMap::new(),
            journals: BTreeMap::new(),
            runtime: BTreeMap::new(),
            factory,
            events,
        };
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(store),
            Err(e) => return Err(io(&dir, e)),
        };
        for entry in entries {
            let entry = entry.map_err(|e| io(&dir, e))?;
            let path = entry.path().join("session.json");
            if !path.exists() {
                continue;
            }
            let session: Session = read(&path)?;
            valid_name(&session.name)?;
            if entry.file_name() != std::ffi::OsStr::new(&session.name) {
                return Err(Error::Invalid("session directory mismatch".into()));
            }
            for id in &session.panes {
                valid_pane(id)?;
                let journal: PaneJournal = read(&entry.path().join(format!("{id}.json")))?;
                if journal.spec.id != *id || store.journals.contains_key(id) {
                    return Err(Error::Invalid(
                        "duplicate or mismatched pane identity".into(),
                    ));
                }
                store.journals.insert(id.clone(), journal);
            }
            store.sessions.insert(session.name.clone(), session);
        }
        Ok(store)
    }
    pub fn list(&self) -> Vec<Session> {
        self.sessions.values().cloned().collect()
    }
    pub fn up(&mut self, name: &str, cwd: &Path) -> Result<Session> {
        valid_name(name)?;
        let mut session = self.sessions.get(name).cloned().unwrap_or(Session {
            name: name.into(),
            cwd: cwd.into(),
            up: true,
            workspaces: Vec::new(),
            panes: Vec::new(),
        });
        session.up = true;
        self.save_session(&session)?;
        self.sessions.insert(name.into(), session.clone());
        self.events
            .publish("session", json!({"session":name,"action":"up"}));
        Ok(session)
    }
    pub fn down(&mut self, name: &str) -> Result<()> {
        let mut session = self.session(name)?.clone();
        for pane in session.panes.clone() {
            self.close(&pane)?;
        }
        session = self.session(name)?.clone();
        session.up = false;
        self.save_session(&session)?;
        self.sessions.insert(name.into(), session);
        self.events
            .publish("session", json!({"session":name,"action":"down"}));
        Ok(())
    }
    pub fn workspaces(&self, name: &str) -> Result<Vec<Workspace>> {
        Ok(self.session(name)?.workspaces.clone())
    }
    pub fn new_workspace(
        &mut self,
        name: &str,
        label: &str,
        cwd: &Path,
        env: BTreeMap<String, String>,
    ) -> Result<Workspace> {
        let mut session = self.active_session(name)?.clone();
        let next = self
            .sessions
            .values()
            .flat_map(|s| &s.workspaces)
            .filter_map(|w| w.id.strip_prefix('w')?.parse::<u64>().ok())
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or_else(|| Error::Invalid("workspace id exhausted".into()))?;
        let workspace = Workspace {
            id: format!("w{next}"),
            label: label.into(),
            cwd: cwd.into(),
            env,
            focused_pane: None,
            next_pane: 1,
        };
        session.workspaces.push(workspace.clone());
        self.save_session(&session)?;
        self.sessions.insert(name.into(), session);
        self.events.publish(
            "session",
            json!({"session":name,"action":"workspace.new","workspace":workspace}),
        );
        Ok(workspace)
    }
    pub fn new_pane(
        &mut self,
        name: &str,
        workspace: &str,
        cwd: &Path,
        env: BTreeMap<String, String>,
        split: Option<Split>,
    ) -> Result<PaneSpec> {
        let mut session = self.active_session(name)?.clone();
        let ws = session
            .workspaces
            .iter_mut()
            .find(|w| w.id == workspace)
            .ok_or_else(|| Error::NotFound(workspace.into()))?;
        if let Some(split) = &split {
            if !split.ratio.is_finite() || split.ratio <= 0.0 || split.ratio >= 1.0 {
                return Err(Error::Invalid(
                    "split ratio must be between zero and one".into(),
                ));
            }
            let other = self.journal(&split.of)?;
            if other.closed || other.spec.workspace != workspace {
                return Err(Error::Invalid(
                    "split must target an open pane in this workspace".into(),
                ));
            }
        }
        let mut merged = ws.env.clone();
        merged.extend(env);
        let spec = PaneSpec {
            id: format!("{workspace}:p{}", ws.next_pane),
            workspace: workspace.into(),
            cwd: cwd.into(),
            env: merged,
            split,
        };
        ws.next_pane = ws
            .next_pane
            .checked_add(1)
            .ok_or_else(|| Error::Invalid("pane id exhausted".into()))?;
        ws.focused_pane = Some(spec.id.clone());
        let mut journal = PaneJournal {
            spec: spec.clone(),
            screen: Screen::default(),
            launch: None,
            agent: None,
            closed: false,
            lost: None,
        };
        // Reserve identity durably before allocating an owned core process.
        self.save_journal(name, &journal)?;
        session.panes.push(spec.id.clone());
        self.save_session(&session)?;
        self.sessions.insert(name.into(), session);
        self.journals.insert(spec.id.clone(), journal.clone());
        match self.factory.create(&spec, &journal.screen) {
            Ok(handle) => {
                self.insert_runtime(&spec.id, handle);
            }
            Err(e) => {
                journal.lost = Some(e.to_string());
                self.put_journal(&spec.id, journal)?;
                return Err(e);
            }
        }
        self.events.publish(
            "session",
            json!({"session":name,"action":"pane.new","pane":spec}),
        );
        Ok(spec)
    }
    pub fn panes(&self, name: &str) -> Result<Vec<PaneJournal>> {
        self.session(name)?
            .panes
            .iter()
            .map(|id| self.journal(id).cloned())
            .collect()
    }
    pub fn journal(&self, pane: &str) -> Result<&PaneJournal> {
        self.journals
            .get(pane)
            .ok_or_else(|| Error::NotFound(pane.into()))
    }
    pub fn close(&mut self, pane: &str) -> Result<()> {
        let mut journal = self.journal(pane)?.clone();
        if journal.closed {
            return Ok(());
        }
        if let Some(runtime) = self.runtime.get_mut(pane) {
            journal.screen = bounded(runtime.handle.text(10_000)?);
            runtime.handle.close()?;
        }
        self.runtime.remove(pane);
        journal.closed = true;
        self.put_journal(pane, journal)?;
        let name = self.owner(pane)?;
        let mut session = self.session(&name)?.clone();
        for ws in &mut session.workspaces {
            if ws.focused_pane.as_deref() == Some(pane) {
                ws.focused_pane = session
                    .panes
                    .iter()
                    .find(|id| {
                        self.journals
                            .get(*id)
                            .is_some_and(|j| !j.closed && j.spec.workspace == ws.id)
                    })
                    .cloned();
            }
        }
        self.save_session(&session)?;
        self.sessions.insert(name, session);
        self.events
            .publish("session", json!({"pane":pane,"action":"pane.close"}));
        Ok(())
    }
    pub fn restore(&mut self) -> Result<()> {
        let ids: Vec<_> = self.journals.keys().cloned().collect();
        for id in ids {
            let mut journal = self.journal(&id)?.clone();
            if journal.closed
                || !self.session(&self.owner(&id)?)?.up
                || self.runtime.contains_key(&id)
            {
                continue;
            }
            match self.factory.recover(&journal.spec, &journal.screen)? {
                Recovery::Attached(handle) => {
                    journal.lost = None;
                    self.insert_runtime(&id, handle);
                }
                Recovery::DiedWithDaemon => {
                    let mut handle = match self.factory.create(&journal.spec, &journal.screen) {
                        Ok(handle) => handle,
                        Err(e) => {
                            journal.lost = Some(e.to_string());
                            self.put_journal(&id, journal)?;
                            continue;
                        }
                    };
                    if let Some(record) = &mut journal.launch {
                        // Core must preserve native conversation identity from these frozen launch options.
                        // Never invent account/approval/trust flags or a replacement conversation.
                        match handle.resume(record) {
                            Ok(pid) => {
                                record.pid = Some(pid);
                                record.started_at = now();
                            }
                            Err(e) => {
                                let _ = handle.close();
                                journal.lost = Some(e.to_string());
                                self.put_journal(&id, journal)?;
                                continue;
                            }
                        }
                    }
                    journal.lost = None;
                    journal.agent = None;
                    self.insert_runtime(&id, handle);
                }
                Recovery::Lost(reason) => {
                    journal.lost = Some(reason);
                    journal.agent = None;
                }
            }
            self.put_journal(&id, journal)?;
            self.events.publish(
                "session",
                json!({"pane":id,"action":"restored","lost":self.journal(&id)?.lost}),
            );
        }
        Ok(())
    }
    pub fn start(&mut self, pane: &str, spec: AgentSpec) -> Result<LaunchRecord> {
        let mut journal = self.journal(pane)?.clone();
        if journal.launch.is_some() {
            return Err(Error::Busy);
        }
        self.runtime_mut(pane)?;
        let pending_prompt = spec
            .prompt_file
            .as_ref()
            .map(|path| agent::directive_prompt(path).map_err(|e| io(path, e)))
            .transpose()?;
        let record = LaunchRecord {
            spec,
            pid: None,
            started_at: now(),
            conversation_id: None,
            pending_prompt,
        };
        journal.launch = Some(record.clone());
        self.put_journal(pane, journal.clone())?;
        let pid = match self.runtime_mut(pane)?.handle.spawn(&record.spec) {
            Ok(pid) => pid,
            Err(e) => {
                journal.lost = Some(e.to_string());
                self.put_journal(pane, journal)?;
                return Err(e);
            }
        };
        let mut record = record;
        record.pid = Some(pid);
        journal.launch = Some(record.clone());
        self.put_journal(pane, journal)?;
        self.observe(pane)?;
        Ok(record)
    }
    pub fn bind_conversation(&mut self, pane: &str, identity: &str) -> Result<()> {
        if identity.is_empty() {
            return Err(Error::Invalid("empty conversation identity".into()));
        }
        let mut journal = self.journal(pane)?.clone();
        let record = journal
            .launch
            .as_mut()
            .ok_or_else(|| Error::NotFound("launch".into()))?;
        if record
            .conversation_id
            .as_deref()
            .is_some_and(|old| old != identity)
        {
            return Err(Error::Invalid(
                "conversation binding cannot be replaced".into(),
            ));
        }
        record.conversation_id = Some(identity.into());
        self.put_journal(pane, journal)
    }
    /// Resumes the pane's recorded conversation with `argv` (the harness's resume form over
    /// the frozen arguments) in the same pane: the launch record and its conversation stay,
    /// only the process and its start change. A pane whose process ended with a previous
    /// daemon is rebuilt from its journal first; a second pane is never created.
    pub fn resume(&mut self, pane: &str, argv: Vec<String>) -> Result<LaunchRecord> {
        let mut journal = self.journal(pane)?.clone();
        let mut record = journal
            .launch
            .clone()
            .ok_or_else(|| Error::NotFound("launch".into()))?;
        if journal.closed {
            return Err(Error::Lost(format!("{pane}: the pane is closed")));
        }
        if !self.runtime.contains_key(pane) {
            let handle = self.factory.create(&journal.spec, &journal.screen)?;
            self.insert_runtime(pane, handle);
        }
        let spec = AgentSpec {
            argv,
            prompt_file: None,
            ..record.spec.clone()
        };
        let pid = self.runtime_mut(pane)?.handle.spawn(&spec)?;
        record.pid = Some(pid);
        record.started_at = now();
        record.pending_prompt = None;
        journal.launch = Some(record.clone());
        journal.lost = None;
        journal.agent = None;
        self.put_journal(pane, journal)?;
        self.observe(pane)?;
        Ok(record)
    }
    /// Queues the first prompt for delivery when the agent next shows an idle prompt.
    pub fn queue_prompt(&mut self, pane: &str, text: &str) -> Result<()> {
        let mut journal = self.journal(pane)?.clone();
        let record = journal
            .launch
            .as_mut()
            .ok_or_else(|| Error::NotFound("launch".into()))?;
        record.pending_prompt = Some(text.into());
        self.put_journal(pane, journal)
    }
    /// The pane's journal environment (a worker pane carries `QREAM_TASK`).
    pub fn pane_env(&self, pane: &str) -> Result<BTreeMap<String, String>> {
        Ok(self.journal(pane)?.spec.env.clone())
    }
    pub fn prompt(&mut self, pane: &str, text: &str) -> Result<()> {
        let info = self.observe(pane)?; // Recheck immediately; a cached idle observation cannot admit input.
        agent::prompt(self.runtime_mut(pane)?.handle.as_mut(), &info, text)
    }
    pub fn observe(&mut self, pane: &str) -> Result<AgentInfo> {
        let mut journal = self.journal(pane)?.clone();
        let record = journal
            .launch
            .clone()
            .ok_or_else(|| Error::NotFound("launch".into()))?;
        let runtime = self.runtime_mut(pane)?;
        let screen = runtime.handle.text(10_000)?;
        let info = Detector::for_kind(record.spec.kind)?.observe(
            &screen.lines,
            &runtime.handle.child_pids()?,
            runtime.handle.exited()?,
            record.pid,
            &record.started_at,
        );
        // Screen durability remains change-driven in poll_changes/record_screen.
        let changed = journal.agent.as_ref().is_none_or(|old| {
            old.state != info.state
                || old.blocked_kind != info.blocked_kind
                || old.interactive_ready != info.interactive_ready
                || old.evidence != info.evidence
                || old.pid != info.pid
        });
        journal.agent = Some(info.clone());
        self.journals.insert(pane.into(), journal);
        if changed {
            self.events
                .publish("agent", json!({"pane":pane,"agent":info}));
        }
        Ok(info)
    }
    /// Change subscriptions are consumed even with no window. Lag recovers a full snapshot.
    pub fn poll_changes(&mut self) -> Result<()> {
        let ids: Vec<_> = self.runtime.keys().cloned().collect();
        for id in ids {
            let mut latest = None;
            let runtime = self.runtime_mut(&id)?;
            loop {
                match runtime.changes.try_recv() {
                    Ok(screen) => latest = Some(screen),
                    Err(broadcast::error::TryRecvError::Lagged(_)) => {
                        latest = Some(runtime.handle.text(10_000)?)
                    }
                    Err(
                        broadcast::error::TryRecvError::Empty
                        | broadcast::error::TryRecvError::Closed,
                    ) => break,
                }
            }
            if let Some(screen) = latest {
                self.record_screen(&id, screen)?;
            }
            if self.journal(&id)?.launch.is_some() {
                let info = self.observe(&id)?;
                let mut journal = self.journal(&id)?.clone();
                if let Some(text) = journal
                    .launch
                    .as_ref()
                    .and_then(|r| r.pending_prompt.clone())
                    && info.state == agent::AgentState::Idle
                    && info.interactive_ready
                {
                    match agent::prompt(self.runtime_mut(&id)?.handle.as_mut(), &info, &text) {
                        Ok(()) => {
                            if let Some(record) = &mut journal.launch {
                                record.pending_prompt = None;
                            }
                            self.journals.insert(id.clone(), journal);
                        }
                        // Backpressure is not acceptance: retain the pending prompt for a later tick.
                        Err(Error::Busy) => {}
                        Err(error) => return Err(error),
                    }
                }
            }
        }
        Ok(())
    }
    /// Journal every owned pane's current screen now, e.g. before the daemon exits.
    pub fn flush(&mut self) -> Result<()> {
        let ids: Vec<_> = self.runtime.keys().cloned().collect();
        for id in ids {
            let screen = self.runtime_mut(&id)?.handle.text(10_000)?;
            self.record_screen(&id, screen)?;
        }
        Ok(())
    }
    pub fn record_screen(&mut self, pane: &str, screen: Screen) -> Result<()> {
        let mut journal = self.journal(pane)?.clone();
        journal.screen = bounded(screen);
        if self.journal(pane)?.screen != journal.screen {
            self.put_journal(pane, journal.clone())?;
            let rows = journal.screen.state.get("rows").cloned().unwrap_or_else(|| {
                json!(journal.screen.lines.iter().enumerate().map(|(y,text)| json!({"y":y,"runs":[{"text":text,"fg":null,"bg":null,"bold":false,"italic":false,"underline":false}]})).collect::<Vec<_>>())
            });
            self.events.publish("pane", json!({"pane":pane,"viewer":null,"rows":rows,"cursor":journal.screen.state.get("cursor"),"scrollback_len":journal.screen.scrollback.len()}));
        }
        Ok(())
    }
    pub fn resize(&mut self, pane: &str, cols: u16, rows: u16) -> Result<()> {
        if cols == 0 || rows == 0 {
            return Err(Error::Invalid("zero terminal size".into()));
        }
        self.runtime_mut(pane)?.handle.resize(cols, rows)?;
        let screen = self.runtime_mut(pane)?.handle.text(10_000)?;
        self.record_screen(pane, screen)
    }
    fn runtime_mut(&mut self, pane: &str) -> Result<&mut Runtime> {
        self.runtime
            .get_mut(pane)
            .ok_or_else(|| Error::Lost(format!("{pane}: no owned handle")))
    }
    fn insert_runtime(&mut self, pane: &str, handle: Box<dyn PaneHandle>) {
        let changes = handle.subscribe();
        self.runtime
            .insert(pane.into(), Runtime { handle, changes });
    }
    fn session(&self, name: &str) -> Result<&Session> {
        self.sessions
            .get(name)
            .ok_or_else(|| Error::NotFound(name.into()))
    }
    fn active_session(&self, name: &str) -> Result<&Session> {
        let session = self.session(name)?;
        if !session.up {
            return Err(Error::Invalid("session is down".into()));
        }
        Ok(session)
    }
    fn owner(&self, pane: &str) -> Result<String> {
        self.sessions
            .values()
            .find(|s| s.panes.iter().any(|p| p == pane))
            .map(|s| s.name.clone())
            .ok_or_else(|| Error::NotFound(pane.into()))
    }
    fn save_session(&self, session: &Session) -> Result<()> {
        write(
            &self
                .home
                .join("sessions")
                .join(&session.name)
                .join("session.json"),
            session,
        )
    }
    fn save_journal(&self, name: &str, journal: &PaneJournal) -> Result<()> {
        write(
            &self
                .home
                .join("sessions")
                .join(name)
                .join(format!("{}.json", journal.spec.id)),
            journal,
        )
    }
    fn put_journal(&mut self, pane: &str, journal: PaneJournal) -> Result<()> {
        let name = self.owner(pane)?;
        self.save_journal(&name, &journal)?;
        self.journals.insert(pane.into(), journal);
        Ok(())
    }
}
fn bounded(mut screen: Screen) -> Screen {
    let excess = screen.scrollback.len().saturating_sub(10_000);
    screen.scrollback.drain(..excess);
    screen
}
fn valid_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > 128
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err(Error::Invalid("unsafe session name".into()));
    }
    Ok(())
}
fn valid_pane(id: &str) -> Result<()> {
    let valid = id.split_once(":p").is_some_and(|(w, p)| {
        w.strip_prefix('w')
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
            && !p.is_empty()
            && p.bytes().all(|b| b.is_ascii_digit())
    });
    if !valid {
        return Err(Error::Invalid("unsafe pane identity".into()));
    }
    Ok(())
}
fn read<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    Ok(serde_json::from_slice(
        &fs::read(path).map_err(|e| io(path, e))?,
    )?)
}
fn write<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| Error::Invalid("journal has no parent".into()))?;
    fs::create_dir_all(parent).map_err(|e| io(parent, e))?;
    let temporary = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(value)?;
    use std::io::Write;
    let mut file = fs::File::create(&temporary).map_err(|e| io(&temporary, e))?;
    file.write_all(&bytes).map_err(|e| io(&temporary, e))?;
    file.sync_all().map_err(|e| io(&temporary, e))?;
    fs::rename(&temporary, path).map_err(|e| io(path, e))?;
    fs::File::open(parent)
        .and_then(|dir| dir.sync_all())
        .map_err(|e| io(parent, e))?;
    Ok(())
}
