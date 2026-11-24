//! Interrupt, graceful pause, immediate kill and release at campaign, wave, worker and control-role scope.
//! Every act touches only what the engine recorded as owned, writes the control ledger, keeps each
//! conversation's launch record, and writes nothing to the operator.
use crate::{
    Error, Result,
    ledger::{Ledger, Transition},
    now,
    owned::{self, Liveness, Owned, Owner, Registry, Role},
    storage::{read_json, write_json},
};
use qream_term::{
    agent::{AgentKind, AgentState, LaunchRecord},
    detect::Detector,
    handle::PaneHandle,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fmt,
    path::{Path, PathBuf},
    str::FromStr,
    thread,
    time::{Duration, Instant},
};

/// Task key of the orchestrator seat's unit.
pub const ORCHESTRATOR: &str = "orchestrator";
/// Task key of the controller's unit.
pub const CONTROLLER: &str = "controller";
/// The purpose recorded for an agent's own process group.
pub const HARNESS: &str = "agent harness";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(into = "String", try_from = "String")]
pub enum Scope {
    Campaign,
    Wave(u32),
    Worker(String),
    Orchestrator,
    Controller,
}

impl fmt::Display for Scope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Campaign => f.write_str("campaign"),
            Self::Wave(n) => write!(f, "wave:{n}"),
            Self::Worker(t) => write!(f, "worker:{t}"),
            Self::Orchestrator => f.write_str(ORCHESTRATOR),
            Self::Controller => f.write_str(CONTROLLER),
        }
    }
}

impl FromStr for Scope {
    type Err = Error;
    fn from_str(s: &str) -> Result<Scope> {
        let bad = || Error::Invalid(format!("unknown target {s}"));
        Ok(match s.split_once(':') {
            None if s == "campaign" => Self::Campaign,
            None if s == ORCHESTRATOR => Self::Orchestrator,
            None if s == CONTROLLER => Self::Controller,
            Some(("wave", n)) => Self::Wave(n.parse().map_err(|_| bad())?),
            Some(("worker", t)) if !t.is_empty() => Self::Worker(t.into()),
            _ => return Err(bad()),
        })
    }
}

impl From<Scope> for String {
    fn from(s: Scope) -> String {
        s.to_string()
    }
}

impl TryFrom<String> for Scope {
    type Error = Error;
    fn try_from(s: String) -> Result<Scope> {
        s.parse()
    }
}

impl Scope {
    /// Whether something with this role, wave and task falls inside the scope.
    pub fn covers(&self, role: Role, wave: Option<u32>, task: Option<&str>) -> bool {
        match self {
            Self::Campaign => true,
            Self::Wave(n) => wave == Some(*n),
            Self::Worker(t) => task == Some(t.as_str()),
            Self::Orchestrator => role == Role::Orchestrator,
            Self::Controller => role == Role::Controller,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Interrupt,
    Pause,
    Kill,
    Release,
}

impl FromStr for Action {
    type Err = Error;
    fn from_str(s: &str) -> Result<Action> {
        Ok(match s {
            "interrupt" => Self::Interrupt,
            "pause" => Self::Pause,
            "kill" => Self::Kill,
            "release" => Self::Release,
            _ => return Err(Error::Invalid(format!("unknown action {s}"))),
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HoldKind {
    Pause,
    Kill,
}

/// A fence on a scope: nothing in it starts until the operator releases it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hold {
    pub scope: Scope,
    pub kind: HoldKind,
    pub at: String,
    pub by: String,
    /// A pause still waiting for its running turns to reach a safe point.
    pub pending: bool,
    /// The conversations the pause stopped, kept for the operator's resume.
    #[serde(default)]
    pub stopped: Vec<Stopped>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stopped {
    pub task: String,
    pub conversation_id: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnitStatus {
    Launching,
    Live,
    Stopped,
}

/// The directive or the last follow-up a unit has not yet sealed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Delivery {
    pub id: String,
    pub text: String,
}

/// One agent the engine runs: its frozen launch record outlives its process.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UnitRecord {
    pub task: String,
    pub role: Role,
    pub wave: Option<u32>,
    pub record: LaunchRecord,
    /// The pane the unit runs in; recovery reopens this pane and never another.
    #[serde(default)]
    pub pane: Option<String>,
    pub pending: Option<Delivery>,
    pub status: UnitStatus,
    pub reason: Option<String>,
}

#[derive(Default, Serialize, Deserialize)]
struct UnitsFile {
    units: BTreeMap<String, UnitRecord>,
}

#[derive(Default, Serialize, Deserialize)]
struct HoldsFile {
    holds: Vec<Hold>,
}

/// The interrupt key every installed harness reads as "stop this turn".
pub fn interrupt_key(kind: AgentKind) -> &'static [u8] {
    match kind {
        AgentKind::Claude | AgentKind::Codex | AgentKind::Opencode => b"\x1b",
        // A shell task's program reads the terminal's interrupt.
        AgentKind::Shell => b"\x03",
    }
}

pub(crate) fn units_path(root: &Path) -> PathBuf {
    root.join("state/units.json")
}
pub(crate) fn holds_path(root: &Path) -> PathBuf {
    root.join("state/holds.json")
}

/// A campaign's units as last recorded; used by leftovers without an engine.
pub fn load_units(root: &Path) -> Result<BTreeMap<String, UnitRecord>> {
    Ok(read_json::<UnitsFile>(&units_path(root))?.units)
}
pub fn load_holds(root: &Path) -> Result<Vec<Hold>> {
    Ok(read_json::<HoldsFile>(&holds_path(root))?.holds)
}

pub struct Engine {
    pub root: PathBuf,
    pub campaign: String,
    pub run: String,
    pub by: String,
    pub registry: Registry,
    pub ledger: Ledger,
    pub(crate) units: BTreeMap<String, UnitRecord>,
    pub(crate) handles: BTreeMap<String, Box<dyn PaneHandle>>,
    holds: Vec<Hold>,
    pause_since: BTreeMap<String, Instant>,
    /// How long a pause waits for a running turn before it stops it anyway.
    pub safe_point_limit: Duration,
    /// How long an ended group gets between terminate and kill.
    pub grace: Duration,
    /// How long recovery waits for a resumed agent's prompt before leaving its work pending.
    pub deliver_wait: Duration,
}

impl Engine {
    pub fn open(
        root: &Path,
        campaign: &str,
        run: &str,
        by: &str,
        registry: Registry,
    ) -> Result<Engine> {
        Ok(Engine {
            root: root.into(),
            campaign: campaign.into(),
            run: run.into(),
            by: by.into(),
            registry,
            ledger: Ledger::new(root),
            units: load_units(root)?,
            handles: BTreeMap::new(),
            holds: load_holds(root)?,
            pause_since: BTreeMap::new(),
            safe_point_limit: Duration::from_secs(600),
            grace: Duration::from_secs(3),
            deliver_wait: Duration::from_secs(120),
        })
    }

    pub fn units(&self) -> &BTreeMap<String, UnitRecord> {
        &self.units
    }
    pub fn holds(&self) -> &[Hold] {
        &self.holds
    }
    pub fn stop_marker(&self) -> PathBuf {
        self.root.join("STOP")
    }

    /// The hold that fences this unit, a kill before a pause.
    pub fn held(&self, role: Role, wave: Option<u32>, task: &str) -> Option<&Hold> {
        let all: Vec<&Hold> = self
            .holds
            .iter()
            .filter(|h| h.scope.covers(role, wave, Some(task)))
            .collect();
        all.iter()
            .find(|h| h.kind == HoldKind::Kill)
            .or_else(|| all.first())
            .copied()
    }

    /// Registers a launch before its process exists; refused while a STOP marker or a hold fences it.
    pub fn begin_launch(&mut self, unit: UnitRecord) -> Result<()> {
        if self.stop_marker().exists() {
            return Err(Error::Stop(self.stop_marker()));
        }
        if let Some(hold) = self.held(unit.role, unit.wave, &unit.task) {
            return Err(Error::Held(format!("{} holds {}", hold.scope, unit.task)));
        }
        if let Some(old) = self.units.get(&unit.task)
            && old.status != UnitStatus::Stopped
        {
            return Err(Error::Invalid(format!("{} is already running", unit.task)));
        }
        let task = unit.task.clone();
        self.units.insert(
            task.clone(),
            UnitRecord {
                status: UnitStatus::Launching,
                reason: None,
                ..unit
            },
        );
        self.save_units()?;
        self.log(&task, "queued", "launching", "launch begun")
    }

    /// Takes the spawned pane. A stop that arrived meanwhile wins: the process ends before its first prompt.
    pub fn finish_launch(
        &mut self,
        task: &str,
        mut handle: Box<dyn PaneHandle>,
        pid: u32,
    ) -> Result<()> {
        let unit = self
            .units
            .get(task)
            .cloned()
            .ok_or_else(|| Error::NotFound(format!("launch of {task}")))?;
        let owned = Owned::group(&self.owner(&unit, "agent harness"), pid);
        if let Ok(owned) = &owned {
            self.registry.add(owned.clone())?;
        }
        let fenced = self.held(unit.role, unit.wave, task).is_some() || self.stop_marker().exists();
        if unit.status != UnitStatus::Launching || fenced {
            let closed = handle.close();
            self.end_task_processes(task)?;
            self.set_status(task, UnitStatus::Stopped, "stopped during launch")?;
            self.log(
                task,
                "launching",
                "stopped",
                "a stop arrived during the launch; ended before its first prompt",
            )?;
            closed?;
            return Err(Error::Stopped(format!(
                "{task} was stopped during its launch"
            )));
        }
        owned?;
        if let Some(u) = self.units.get_mut(task) {
            u.record.pid = Some(pid);
            u.status = UnitStatus::Live;
        }
        self.handles.insert(task.into(), handle);
        self.save_units()?;
        self.log(task, "launching", "live", &format!("pid {pid}"))
    }

    /// Adds a pane the engine already runs (after a restart or for tests of the owner path).
    pub fn attach(&mut self, task: &str, handle: Box<dyn PaneHandle>) {
        self.handles.insert(task.into(), handle);
    }

    /// Records a job, test or helper a task started, so stops and leftovers can end it.
    pub fn own(
        &mut self,
        task: Option<&str>,
        wave: Option<u32>,
        role: Role,
        purpose: &str,
        pid: u32,
    ) -> Result<Owned> {
        let owned = Owned::group(
            &Owner {
                campaign: &self.campaign,
                root: &self.root,
                task,
                wave,
                role,
                purpose,
            },
            pid,
        )?;
        self.registry.add(owned.clone())?;
        Ok(owned)
    }

    pub fn act(&mut self, action: Action, scope: &Scope) -> Result<Vec<String>> {
        match action {
            Action::Interrupt => self.interrupt(scope),
            Action::Pause => self.pause(scope),
            Action::Kill => self.kill(scope),
            Action::Release => self.release(scope),
        }
    }

    /// Sends each live owned pane in scope its harness's interrupt key; nothing else is touched.
    pub fn interrupt(&mut self, scope: &Scope) -> Result<Vec<String>> {
        let mut done = Vec::new();
        for task in self.in_scope(scope, &[UnitStatus::Live]) {
            let kind = self.units[&task].record.spec.kind;
            let conversation = self.remember_conversation(&task)?;
            let evidence = format!("the harness's interrupt key; {conversation}");
            // Record identity before a key can end the harness's current turn.
            if self.handles.contains_key(&task) {
                self.log(&task, "live", "interrupted", &evidence)?;
            }
            if let Some(handle) = self.handles.get_mut(&task) {
                handle.write(interrupt_key(kind))?;
                done.push(task);
            }
        }
        Ok(done)
    }

    /// Whether a hold of this kind covers the whole campaign, and whether it has settled.
    pub fn campaign_hold(&self) -> Option<&Hold> {
        let all: Vec<&Hold> = self
            .holds
            .iter()
            .filter(|h| h.scope == Scope::Campaign)
            .collect();
        all.iter()
            .find(|h| h.kind == HoldKind::Kill)
            .or_else(|| all.first())
            .copied()
    }
    /// True while a pause still waits for a safe point (the daemon ticks it).
    pub fn pausing(&self) -> bool {
        self.holds.iter().any(|h| h.pending)
    }
    /// A unit's pane id, when it runs in one.
    pub fn pane_of(&self, task: &str) -> Option<&str> {
        self.units.get(task)?.pane.as_deref()
    }

    /// Holds starts in scope; running turns reach their next safe point in `tick`, then everything stops.
    pub fn pause(&mut self, scope: &Scope) -> Result<Vec<String>> {
        if self
            .holds
            .iter()
            .any(|h| h.kind == HoldKind::Kill && self.within(scope, &h.scope))
        {
            return Ok(Vec::new()); // A kill already holds it: kill wins over a pause.
        }
        let key = scope.to_string();
        if !self
            .holds
            .iter()
            .any(|h| h.scope == *scope && h.kind == HoldKind::Pause)
        {
            self.holds.push(Hold {
                scope: scope.clone(),
                kind: HoldKind::Pause,
                at: now(),
                by: self.by.clone(),
                pending: true,
                stopped: Vec::new(),
            });
            self.pause_since.insert(key.clone(), Instant::now());
            self.save_holds()?;
        }
        let tasks = self.in_scope(scope, &[UnitStatus::Live, UnitStatus::Launching]);
        for task in &tasks {
            self.log(
                task,
                "live",
                "pausing",
                "starts held; waiting for a safe point",
            )?;
        }
        self.tick()?;
        Ok(tasks)
    }

    /// Advances pending pauses: a unit at a safe point (not mid-turn) or past the limit is stopped;
    /// when nothing of the scope runs, its jobs end and the pause record is written.
    pub fn tick(&mut self) -> Result<()> {
        let pending: Vec<Scope> = self
            .holds
            .iter()
            .filter(|h| h.pending && h.kind == HoldKind::Pause)
            .map(|h| h.scope.clone())
            .collect();
        for scope in pending {
            let since = *self
                .pause_since
                .entry(scope.to_string())
                .or_insert_with(Instant::now);
            let overdue = since.elapsed() >= self.safe_point_limit;
            let mut stopped = Vec::new();
            for task in self.in_scope(&scope, &[UnitStatus::Live]) {
                if overdue || !self.mid_turn(&task)? {
                    self.stop_unit(&task, "paused")?;
                    stopped.push(task);
                }
            }
            let left = self.in_scope(&scope, &[UnitStatus::Live, UnitStatus::Launching]);
            if left.is_empty() {
                self.end_scope_processes(&scope)?;
                let record: Vec<Stopped> = self
                    .in_scope(&scope, &[UnitStatus::Stopped])
                    .into_iter()
                    .map(|task| Stopped {
                        conversation_id: self.units[&task].record.conversation_id.clone(),
                        task,
                    })
                    .collect();
                if let Some(h) = self
                    .holds
                    .iter_mut()
                    .find(|h| h.scope == scope && h.kind == HoldKind::Pause)
                {
                    h.pending = false;
                    h.stopped = record;
                }
                self.pause_since.remove(&scope.to_string());
                self.save_holds()?;
                self.log(
                    &scope.to_string(),
                    "pausing",
                    "paused",
                    "no process of the scope runs",
                )?;
            }
        }
        Ok(())
    }

    /// Pauses and drives `tick` until the scope is stopped or `limit` passes.
    pub fn pause_and_wait(&mut self, scope: &Scope, limit: Duration) -> Result<bool> {
        self.pause(scope)?;
        let until = Instant::now() + limit;
        loop {
            let pending = self.holds.iter().any(|h| h.scope == *scope && h.pending);
            if !pending {
                return Ok(true);
            }
            if Instant::now() >= until {
                return Ok(false);
            }
            thread::sleep(Duration::from_millis(100));
            self.tick()?;
        }
    }

    /// Ends the owned process groups of the scope now; a pending pause inside it is cancelled and a
    /// launch in flight is ended when it lands, before its first prompt.
    pub fn kill(&mut self, scope: &Scope) -> Result<Vec<String>> {
        let cancelled: Vec<Scope> = self
            .holds
            .iter()
            .filter(|h| h.kind == HoldKind::Pause && h.pending && self.within(&h.scope, scope))
            .map(|h| h.scope.clone())
            .collect();
        self.holds
            .retain(|h| !(h.kind == HoldKind::Pause && cancelled.contains(&h.scope)));
        for c in &cancelled {
            self.pause_since.remove(&c.to_string());
            self.log(
                &c.to_string(),
                "pausing",
                "cancelled",
                "a kill won over the pending pause",
            )?;
        }
        if !self
            .holds
            .iter()
            .any(|h| h.scope == *scope && h.kind == HoldKind::Kill)
        {
            self.holds.push(Hold {
                scope: scope.clone(),
                kind: HoldKind::Kill,
                at: now(),
                by: self.by.clone(),
                pending: false,
                stopped: Vec::new(),
            });
        }
        self.save_holds()?;
        let mut done = Vec::new();
        for task in self.in_scope(scope, &[UnitStatus::Launching]) {
            self.set_status(&task, UnitStatus::Stopped, "killed during launch")?;
            self.log(
                &task,
                "launching",
                "stopped",
                "killed; the launch is ended when it lands",
            )?;
            done.push(task);
        }
        for task in self.in_scope(scope, &[UnitStatus::Live]) {
            self.stop_unit(&task, "killed")?;
            done.push(task);
        }
        self.end_scope_processes(scope)?;
        Ok(done)
    }

    /// Lifts the holds inside the scope and resumes the workers those pauses stopped, in
    /// their own panes. A kill release only lifts its fence; no fresh history is opened.
    pub fn release(&mut self, scope: &Scope) -> Result<Vec<String>> {
        let tasks: Vec<String> = self
            .holds
            .iter()
            .filter(|h| h.kind == HoldKind::Pause && self.within(&h.scope, scope))
            .flat_map(|h| h.stopped.iter().map(|s| s.task.clone()))
            .filter(|task| {
                self.units.get(task).is_some_and(|u| {
                    u.reason.as_deref() == Some("paused")
                        && !self
                            .held(u.role, u.wave, task)
                            .is_some_and(|h| h.kind == HoldKind::Kill)
                })
            })
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        let lifted: Vec<String> = self
            .holds
            .iter()
            .filter(|h| self.within(&h.scope, scope))
            .map(|h| h.scope.to_string())
            .collect();
        self.holds
            .retain(|h| !lifted.contains(&h.scope.to_string()));
        for scope in &lifted {
            self.pause_since.remove(scope);
            self.log(scope, "held", "released", "the operator's release")?;
        }
        self.save_holds()?;
        let mut words = lifted;
        let report = self.recover_tasks(&tasks, &mut crate::recover::ExistingPane)?;
        words.extend(
            report
                .restored
                .into_iter()
                .map(|r| format!("{} resumed in its recorded conversation", r.task)),
        );
        words.extend(
            report
                .missed
                .into_iter()
                .map(|m| format!("{} was not resumed: {}", m.task, m.reason)),
        );
        Ok(words)
    }

    /// Refuses acceptance while any process the task started is alive: a tool its harness runs,
    /// or a job, test or helper recorded for it. The idle harness itself is the worker, not its work.
    pub fn accept_guard(&self, task: &str) -> Result<()> {
        if let Some(handle) = self.handles.get(task) {
            // A harness's resident helpers (Codex 0.162's REPL and plugins) live for the whole
            // session and are never a tool the task left running (QF-2026-10-11-REPAIR-06).
            let mut pids = handle.child_pids()?;
            if let Some(unit) = self.units.get(task) {
                let detector = qream_term::detect::Detector::for_kind(unit.record.spec.kind)?;
                pids.retain(|pid| !detector.is_resident(*pid));
            }
            if !pids.is_empty() {
                return Err(Error::LiveProcess {
                    task: task.into(),
                    purpose: "a tool its harness started".into(),
                    pids,
                });
            }
        }
        for owned in self.registry.list()? {
            if owned.root != self.root
                || owned.task.as_deref() != Some(task)
                || owned.purpose.starts_with(HARNESS)
            {
                continue;
            }
            if let Liveness::Alive(pids) = owned::liveness(&owned)? {
                return Err(Error::LiveProcess {
                    task: task.into(),
                    purpose: owned.purpose,
                    pids,
                });
            }
        }
        Ok(())
    }

    // ---- inside ----

    pub(crate) fn owner<'a>(&'a self, unit: &'a UnitRecord, purpose: &'a str) -> Owner<'a> {
        Owner {
            campaign: &self.campaign,
            root: &self.root,
            task: Some(&unit.task),
            wave: unit.wave,
            role: unit.role,
            purpose,
        }
    }

    /// Whether `inner` lies inside `outer` (a worker inside its wave, anything inside the campaign).
    fn within(&self, inner: &Scope, outer: &Scope) -> bool {
        match (inner, outer) {
            (a, b) if a == b => true,
            (_, Scope::Campaign) => true,
            (Scope::Worker(t), Scope::Wave(n)) => {
                self.units.get(t).is_some_and(|u| u.wave == Some(*n))
            }
            _ => false,
        }
    }

    fn in_scope(&self, scope: &Scope, statuses: &[UnitStatus]) -> Vec<String> {
        self.units
            .values()
            .filter(|u| statuses.contains(&u.status) && scope.covers(u.role, u.wave, Some(&u.task)))
            .map(|u| u.task.clone())
            .collect()
    }

    /// True while the agent is in a turn: its harness shows its working marker. A tool left
    /// running under an idle prompt is a job, which the pause ends rather than waits for.
    fn mid_turn(&mut self, task: &str) -> Result<bool> {
        let unit = &self.units[task];
        let Some(handle) = self.handles.get(task) else {
            return Ok(false);
        };
        let screen = handle.text(200)?;
        let info = Detector::for_kind(unit.record.spec.kind)?.observe(
            &screen.lines,
            &handle.child_pids()?,
            handle.exited()?,
            unit.record.pid,
            &unit.record.started_at,
        );
        Ok(info.state == AgentState::Working && info.evidence.rule != "process.tool")
    }

    /// Closes the unit's pane and ends what it owns; its launch record and history stay.
    pub(crate) fn stop_unit(&mut self, task: &str, why: &str) -> Result<()> {
        let from = format!("{:?}", self.units[task].status).to_lowercase();
        let conversation = if why == "paused" {
            self.remember_conversation(task)?
        } else {
            // Immediate kill never waits on journal discovery; any already bound id stays.
            self.units[task]
                .record
                .conversation_id
                .as_ref()
                .map_or_else(
                    || "no recorded conversation".into(),
                    |id| format!("conversation id: {id}"),
                )
        };
        let evidence = format!("{why}; {conversation}");
        self.log(task, &from, "stopping", &evidence)?;
        let closed = match self.handles.get_mut(task) {
            Some(handle) => handle.close(),
            None => Ok(()),
        };
        self.end_task_processes(task)?;
        self.set_status(task, UnitStatus::Stopped, why)?;
        self.log(task, &from, "stopped", &evidence)?;
        closed.map_err(Error::from)
    }

    fn remember_conversation(&mut self, task: &str) -> Result<String> {
        let id = crate::conversation::identity(&self.units[task].record)?;
        if let Some(id) = id {
            if self.units[task].record.conversation_id.as_deref() != Some(id.as_str()) {
                if let Some(unit) = self.units.get_mut(task) {
                    unit.record.conversation_id = Some(id.clone());
                }
                self.save_units()?;
            }
            Ok(format!("conversation id: {id}"))
        } else {
            Ok("no recorded conversation".into())
        }
    }

    /// Ends every owned process recorded for this task in this campaign.
    pub(crate) fn end_task_processes(&mut self, task: &str) -> Result<()> {
        let mine: Vec<Owned> = self
            .registry
            .list()?
            .into_iter()
            .filter(|o| o.root == self.root && o.task.as_deref() == Some(task))
            .collect();
        self.end_all(mine)
    }

    fn end_scope_processes(&mut self, scope: &Scope) -> Result<()> {
        let mine: Vec<Owned> = self
            .registry
            .list()?
            .into_iter()
            .filter(|o| o.root == self.root && scope.covers(o.role, o.wave, o.task.as_deref()))
            .collect();
        self.end_all(mine)
    }

    fn end_all(&mut self, owned: Vec<Owned>) -> Result<()> {
        for o in owned {
            match owned::end(&o, self.grace) {
                Ok(_) => self.registry.remove(&o.id)?,
                // Its id belongs to someone else now: never signalled, forgotten.
                Err(Error::NotOwned(_)) => self.registry.remove(&o.id)?,
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    pub(crate) fn set_status(&mut self, task: &str, status: UnitStatus, why: &str) -> Result<()> {
        if let Some(u) = self.units.get_mut(task) {
            u.status = status;
            u.reason = Some(why.into());
        }
        self.save_units()
    }

    pub(crate) fn save_units(&self) -> Result<()> {
        write_json(
            &units_path(&self.root),
            &UnitsFile {
                units: self.units.clone(),
            },
        )
    }

    fn save_holds(&self) -> Result<()> {
        write_json(
            &holds_path(&self.root),
            &HoldsFile {
                holds: self.holds.clone(),
            },
        )
    }

    pub(crate) fn log(&self, task: &str, from: &str, to: &str, evidence: &str) -> Result<()> {
        self.ledger.append(Transition {
            at: chrono::Local::now().fixed_offset(),
            campaign: self.campaign.clone(),
            run: self.run.clone(),
            task: task.into(),
            from: from.into(),
            to: to.into(),
            by: self.by.clone(),
            evidence: evidence.into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scopes_parse_and_cover() {
        for s in [
            "campaign",
            "wave:2",
            "worker:T13",
            "orchestrator",
            "controller",
        ] {
            assert_eq!(
                s.parse::<Scope>().map(|x| x.to_string()).ok().as_deref(),
                Some(s)
            );
        }
        assert!("wave:x".parse::<Scope>().is_err());
        assert!("worker:".parse::<Scope>().is_err());
        let w = Scope::Worker("T1".into());
        assert!(w.covers(Role::Job, Some(1), Some("T1")));
        assert!(!w.covers(Role::Worker, Some(1), Some("T2")));
        assert!(Scope::Wave(1).covers(Role::Worker, Some(1), Some("T2")));
        assert!(!Scope::Orchestrator.covers(Role::Worker, None, Some("T2")));
    }
}
