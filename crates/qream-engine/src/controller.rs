//! Event-driven controller. Every external operation has a stable idempotency key.
//! Adapters must preserve native approval/account states and the owner's exact history.
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Campaign {
    pub name: String,
    pub run: String,
    pub root: PathBuf,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub cwd: PathBuf,
    pub directive: PathBuf,
    pub kind: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub rows: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Wave {
    pub name: String,
    pub parallel: usize,
    pub campaign: String,
    pub tasks: Vec<Task>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Close {
    pub campaign: String,
    pub last_wave: u32,
    pub declared_at: String,
    pub expected_close: String,
    pub by: String,
    pub rule: String,
    #[serde(default)]
    pub actual_close: Option<String>,
    /// Fields the planning agent added (notes, freeze time) survive the controller's rewrite.
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AgentInfo {
    pub active: bool,
    #[serde(default)]
    pub ended: bool,
    pub owned_job: Option<String>,
    pub blocked: Option<String>,
    pub interactive_ready: bool,
    pub observed_at: u64,
}
/// A batch reservation/start is all-or-nothing, idempotent by key, within capacity.
/// Admission checks runtime, inputs, quota and native fences, without answering dialogs.
pub trait Agents {
    fn capacity(&self) -> Result<usize>;
    fn admit(&self, campaign: &Campaign, task: &Task) -> Result<()>;
    fn start(&mut self, key: &str, campaign: &Campaign, tasks: &[Task]) -> Result<()>;
    fn stop(&mut self, campaign: &Campaign) -> Result<()>;
    fn info(&self, campaign: &Campaign, task: &str) -> Result<AgentInfo>;
    fn deliver(
        &mut self,
        key: &str,
        campaign: &Campaign,
        task: &str,
        text: &str,
        refresh: bool,
    ) -> Result<()>;
}
pub trait Rows {
    /// Resolve the served rows through tracker assignments for canonical manifests.
    fn served(&self, campaign: &Campaign, task: &str) -> Result<Vec<String>>;
    /// The rows the task's directive serves only in part (`(part)`): a check records the part and
    /// never makes the row implemented (qream-repair walk fault 1).
    fn partial(
        &self,
        _campaign: &Campaign,
        _task: &str,
    ) -> Result<std::collections::BTreeSet<String>> {
        Ok(std::collections::BTreeSet::new())
    }
    fn status(&self, row: &str) -> Result<String>;
    fn set_status(&mut self, key: &str, row: &str, status: &str) -> Result<()>;
    fn check(&mut self, key: &str, row: &str, evidence: &str) -> Result<()>;
    fn checked(&self, row: &str) -> Result<bool>;
    fn closing_fault(&mut self, key: &str, row: &str, verdict: &str) -> Result<()>;
}
/// Send atomically deduplicates by key. Never types into the chief's terminal.
#[derive(Clone, Debug)]
pub struct JudgmentMail {
    pub key: String,
    pub task: String,
    pub seal: String,
    pub question: String,
}
pub trait Mail {
    /// Coalesce unseen revisions, atomically deduplicating each item's key even if a retry's batch changes.
    fn judgments(&mut self, campaign: &Campaign, items: &[JudgmentMail]) -> Result<()>;
    fn send(&mut self, key: &str, campaign: &Campaign, text: &str) -> Result<()>;
}
pub trait Clock {
    fn now(&self) -> u64;
    fn timestamp(&self) -> String;
}
pub struct Deps<A, R, M, C> {
    pub agents: A,
    pub rows: R,
    pub mail: M,
    pub clock: C,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Timing {
    Now,
    After,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Gap {
    pub timing: Timing,
    pub reason: String,
    pub findings: Vec<String>,
    pub delivered: bool,
    pub resolved: bool,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Budget {
    pub rounds: u32,
    pub installs: u32,
    pub followups: u32,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TaskRecord {
    pub seal: Option<String>,
    pub accepted: bool,
    pub checked_rows: BTreeSet<String>,
    pub gap: Option<Gap>,
    pub budget: Budget,
    pub escalated: bool,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Snapshot {
    pub triggered: bool,
    pub paused: bool,
    pub finished: bool,
    pub current_wave: u32,
    pub close: Option<Close>,
    pub tasks: BTreeMap<String, TaskRecord>,
    pub seals: BTreeMap<String, BTreeSet<String>>,
    pub delivered: BTreeSet<String>,
    pub judged: BTreeSet<String>,
    pub pins: BTreeMap<u32, String>,
    pub last_progress: u64,
    pub idle_since: Option<u64>,
    pub unsubmitted_blockers: BTreeSet<String>,
    pub observed_at: u64,
    pub closing_faults: BTreeSet<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Transition {
    pub at: String,
    pub campaign: String,
    pub run: String,
    pub task: Option<String>,
    pub from: String,
    pub to: String,
    pub by: String,
    pub evidence: String,
    /// The journal is the recovery authority, status.json only a projection.
    pub snapshot: Snapshot,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    Hold(String),
    ReleaseWave(u32),
    DeliverJudgment(Vec<(String, String)>),
    RouteGapFill(String),
    Finished,
}
#[derive(Clone, Debug)]
pub enum Event {
    Seals {
        seals: Vec<(String, String)>,
    },
    Seal {
        task: String,
        hash: String,
    },
    Judgment {
        task: String,
        hash: String,
        review: String,
        timing: Timing,
        reason: String,
    },
    Accept {
        task: String,
        evidence: String,
        by: String,
    },
    RowChecked {
        row: String,
        evidence: String,
    },
    GapResolved {
        task: String,
        evidence: String,
    },
    JobEnd {
        task: String,
    },
    AgentChange {
        task: String,
    },
    Redeliver {
        task: String,
        request: String,
        text: String,
        refresh: bool,
    },
    Install {
        task: String,
    },
    Stop,
    Resume,
    ClosingVerdict {
        faults: BTreeMap<String, String>,
        next_campaign: String,
        close: Close,
    },
}
pub struct Controller<A, R, M, C> {
    pub campaign: Campaign,
    pub deps: Deps<A, R, M, C>,
    state: Snapshot,
}
fn read(path: &Path) -> Result<Vec<u8>> {
    fs::read(path).map_err(|source| Error::Io {
        path: path.into(),
        source,
    })
}
fn write(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    let parent = path
        .parent()
        .ok_or_else(|| Error::Denied("a file needs a parent directory".into()))?;
    fs::create_dir_all(parent).map_err(|source| Error::Io {
        path: parent.into(),
        source,
    })?;
    let mut file = fs::File::create(&tmp).map_err(|source| Error::Io {
        path: tmp.clone(),
        source,
    })?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|source| Error::Io {
            path: tmp.clone(),
            source,
        })?;
    fs::rename(&tmp, path).map_err(|source| Error::Io {
        path: path.into(),
        source,
    })
}
impl<A: Agents, R: Rows, M: Mail, C: Clock> Controller<A, R, M, C> {
    pub fn new(campaign: Campaign, deps: Deps<A, R, M, C>) -> Result<Self> {
        let mut state = Snapshot {
            last_progress: deps.clock.now(),
            ..Snapshot::default()
        };
        for value in crate::ledger::Journal::new(&campaign.root).read()? {
            // Old and sequenced lines share the same snapshot schema.
            if value.get("snapshot").is_none() {
                continue;
            }
            let entry: Transition = serde_json::from_value(value)?;
            if entry.campaign != campaign.name || entry.run != campaign.run {
                return Err(Error::Denied(
                    "control ledger belongs to another campaign or run".into(),
                ));
            }
            state = entry.snapshot;
        }
        let controller = Self {
            campaign,
            deps,
            state,
        };
        if controller.state.current_wave > 0 {
            let last = controller
                .state
                .close
                .as_ref()
                .map_or(0, |close| close.last_wave);
            if !controller.state.triggered
                || controller.state.current_wave > last
                || !controller
                    .state
                    .pins
                    .contains_key(&controller.state.current_wave)
            {
                return Err(Error::Denied(
                    "control ledger has an inconsistent released wave".into(),
                ));
            }
            for task in controller.wave(controller.state.current_wave)?.0.tasks {
                if !controller.state.tasks.contains_key(&task.id) {
                    return Err(Error::Denied(
                        "control ledger is missing a released task".into(),
                    ));
                }
            }
        }
        Ok(controller)
    }
    pub fn snapshot(&self) -> &Snapshot {
        &self.state
    }
    fn stopped(&self) -> bool {
        self.state.paused || self.campaign.root.join("STOP").exists()
    }
    fn commit(
        &mut self,
        task: Option<&str>,
        from: &str,
        to: &str,
        by: &str,
        evidence: &str,
    ) -> Result<()> {
        let now = self.deps.clock.now();
        let mut live = false;
        for task in self.state.tasks.keys() {
            let info = self.deps.agents.info(&self.campaign, task)?;
            live |= now.saturating_sub(info.observed_at) <= 15
                && (info.active || info.owned_job.is_some());
        }
        if live {
            self.state.idle_since = None;
        } else if self.state.idle_since.is_none() {
            self.state.idle_since = Some(now);
        }
        self.state.observed_at = now;
        let entry = Transition {
            at: self.deps.clock.timestamp(),
            campaign: self.campaign.name.clone(),
            run: self.campaign.run.clone(),
            task: task.map(str::to_owned),
            from: from.into(),
            to: to.into(),
            by: by.into(),
            evidence: evidence.into(),
            snapshot: self.state.clone(),
        };
        crate::ledger::Journal::new(&self.campaign.root).append(&entry)?;
        write(
            &self.campaign.root.join("controller/status.json"),
            &serde_json::to_vec_pretty(&self.state)?,
        )
    }
    fn wave(&self, n: u32) -> Result<(Wave, String, Vec<u8>)> {
        if n == 0 {
            return Err(Error::Denied(
                "No wave is released yet: trigger the campaign first".into(),
            ));
        }
        let path = self.campaign.root.join(format!("wave{n}.json"));
        let bytes = read(&path)?;
        let mut wave: Wave = serde_json::from_slice(&bytes)?;
        for task in &mut wave.tasks {
            if task.rows.is_empty() {
                task.rows = self.deps.rows.served(&self.campaign, &task.id)?;
            }
        }
        if wave.campaign != self.campaign.name || wave.parallel == 0 || wave.tasks.is_empty() {
            return Err(Error::Denied(format!(
                "wave {n} has an invalid campaign, width or empty task list"
            )));
        }
        let mut digest = Sha256::new();
        digest.update(&bytes);
        digest.update(serde_json::to_vec(&wave)?);
        let mut ids = BTreeSet::new();
        for task in &wave.tasks {
            if !ids.insert(&task.id) || task.rows.is_empty() {
                return Err(Error::Denied(format!(
                    "wave {n} has duplicate tasks or a task with no served rows"
                )));
            }
            let directive = if task.directive.is_absolute() {
                task.directive.clone()
            } else {
                self.campaign.root.join(&task.directive)
            };
            let data = read(&directive)?;
            digest.update((data.len() as u64).to_le_bytes());
            digest.update(&data);
        }
        Ok((
            wave,
            digest
                .finalize()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>(),
            bytes,
        ))
    }
    fn pinned_waves(&self) -> Result<()> {
        for (n, hash) in &self.state.pins {
            if &self.wave(*n)?.1 != hash {
                return Err(Error::Denied(format!(
                    "wave {n} was pinned at controller/wave{n}.staged.json; the edited manifest or directive needs its own prepared wave within the declared close"
                )));
            }
        }
        Ok(())
    }
    fn task(&self, id: &str) -> Result<Task> {
        let (wave, _, _) = self.wave(self.state.current_wave)?;
        wave.tasks
            .into_iter()
            .find(|t| t.id == id)
            .ok_or_else(|| Error::Denied(format!("task {id} is not in the current wave")))
    }
    fn record(&mut self, id: &str) -> Result<&mut TaskRecord> {
        self.state
            .tasks
            .get_mut(id)
            .ok_or_else(|| Error::Denied(format!("task {id} has not started")))
    }
    pub fn trigger(&mut self) -> Result<Vec<Decision>> {
        if self.state.finished {
            return Err(Error::Denied(
                "campaign is closed; prepare a new campaign".into(),
            ));
        }
        if self.state.triggered {
            return Err(Error::Denied("campaign already triggered".into()));
        }
        if self.stopped() {
            return Err(Error::Denied("STOP is present: no starts".into()));
        }
        if !self.campaign.root.join("CLOSE.json").exists() {
            return Err(Error::Denied(
                "campaign needs a declared close before Trigger".into(),
            ));
        }
        let close: Close = serde_json::from_slice(&read(&self.campaign.root.join("CLOSE.json"))?)?;
        if close.actual_close.is_some() {
            return Err(Error::Denied(
                "campaign is closed past its last wave; prepare a new campaign".into(),
            ));
        }
        if close.last_wave == 0
            || close.campaign != self.campaign.name
            || close.expected_close.is_empty()
        {
            return Err(Error::Denied(
                "campaign needs a declared last wave and expected close before Trigger".into(),
            ));
        }
        let mut ids = BTreeSet::new();
        for n in 1..=close.last_wave {
            for task in self.wave(n)?.0.tasks {
                if !ids.insert(task.id) {
                    return Err(Error::Denied(
                        "task ids must be unique across declared waves".into(),
                    ));
                }
            }
        }
        // Tentative in-memory admission state only. No Running/Trigger record, store
        // status or staged input is published until native batch launch succeeds.
        let before = self.state.clone();
        self.state.close = Some(close);
        self.state.triggered = true;
        if let Err(error) = self.release(1) {
            if self.state.current_wave == before.current_wave {
                self.state = before;
            }
            return Err(error);
        }
        self.commit(None, "prepared", "running", "operator", "Trigger")?;
        write(
            &self.campaign.root.join("controller/TRIGGERED"),
            self.deps.clock.timestamp().as_bytes(),
        )?;
        let mut decisions = vec![Decision::ReleaseWave(1)];
        decisions.extend(self.drive()?);
        Ok(decisions)
    }
    pub fn request_release(&self, wave: u32) -> Result<Decision> {
        let last = self
            .state
            .close
            .as_ref()
            .map(|c| c.last_wave)
            .ok_or_else(|| Error::Denied("campaign has no declared close".into()))?;
        if wave > last {
            return Err(Error::Denied(format!(
                "wave {wave} is refused: this campaign closes after wave {last}; prepare a new campaign"
            )));
        }
        let decision = self.decide()?;
        if decision != Decision::ReleaseWave(wave) {
            return Err(Error::Denied(format!("wave {wave} is held: {decision:?}")));
        }
        Ok(decision)
    }
    pub fn decide(&self) -> Result<Decision> {
        if self.stopped() {
            return Ok(Decision::Hold(
                "STOP is present or campaign paused: no starts".into(),
            ));
        }
        if !self.state.triggered {
            return Ok(Decision::Hold("Prepared; waiting for Trigger".into()));
        }
        self.pinned_waves()?;
        if self.state.finished {
            return Ok(Decision::Finished);
        }
        if self.state.current_wave == 0 {
            return Ok(Decision::ReleaseWave(1));
        }
        let wave = self.wave(self.state.current_wave)?.0;
        let pending: Vec<_> = wave
            .tasks
            .iter()
            .flat_map(|task| {
                self.state
                    .seals
                    .get(&task.id)
                    .into_iter()
                    .flatten()
                    .filter(|hash| {
                        !self
                            .state
                            .delivered
                            .contains(&format!("{}:{hash}", task.id))
                    })
                    .map(|hash| (task.id.clone(), hash.clone()))
            })
            .collect();
        if !pending.is_empty() {
            return Ok(Decision::DeliverJudgment(pending));
        }
        // 'after' means after the other tasks finish their first attempt, never after the gate.
        let after_ready = wave.tasks.iter().all(|t| {
            self.state
                .tasks
                .get(&t.id)
                .is_some_and(|r| r.accepted || r.seal.is_some())
        });
        for t in &wave.tasks {
            let r = &self.state.tasks[&t.id];
            if r.gap.as_ref().is_some_and(|g| {
                !g.resolved && !g.delivered && (g.timing == Timing::Now || after_ready)
            }) && !r.escalated
            {
                let info = self.deps.agents.info(&self.campaign, &t.id)?;
                if info.interactive_ready
                    && info.blocked.is_none()
                    && !info.active
                    && info.owned_job.is_none()
                {
                    return Ok(Decision::RouteGapFill(t.id.clone()));
                }
            }
        }
        for t in &wave.tasks {
            let r = &self.state.tasks[&t.id];
            if !r.accepted {
                return Ok(Decision::Hold(format!(
                    "wave {} is held: {} is not accepted",
                    self.state.current_wave, t.id
                )));
            }
            if r.gap.as_ref().is_some_and(|g| !g.resolved) {
                return Ok(Decision::Hold(format!(
                    "wave {} is held: {} gap fill is unresolved",
                    self.state.current_wave, t.id
                )));
            }
            let info = self.deps.agents.info(&self.campaign, &t.id)?;
            if info.active || info.owned_job.is_some() {
                return Ok(Decision::Hold(format!(
                    "{} still has live owned work",
                    t.id
                )));
            }
            for row in &t.rows {
                if !self.state.closing_faults.contains(row)
                    && (!r.checked_rows.contains(row) || !self.deps.rows.checked(row)?)
                {
                    return Ok(Decision::Hold(format!(
                        "wave {} is held: row {row} is unchecked",
                        self.state.current_wave
                    )));
                }
            }
        }
        let last = self
            .state
            .close
            .as_ref()
            .ok_or_else(|| Error::Denied("declared close missing".into()))?
            .last_wave;
        if self.state.current_wave == last {
            Ok(Decision::Finished)
        } else {
            Ok(Decision::ReleaseWave(self.state.current_wave + 1))
        }
    }
    fn release(&mut self, n: u32) -> Result<()> {
        self.request_release(n)?;
        let (wave, hash, bytes) = self.wave(n)?;
        let width = self.deps.agents.capacity()?.min(wave.parallel);
        if wave.tasks.len() > width {
            return Err(Error::Denied(format!(
                "wave {n} needs all {} tasks together; capacity is {width}",
                wave.tasks.len()
            )));
        }
        for task in &wave.tasks {
            if self.state.tasks.contains_key(&task.id) {
                return Err(Error::Denied(format!(
                    "task {} was already released",
                    task.id
                )));
            }
            self.deps.agents.admit(&self.campaign, task)?;
            for row in &task.rows {
                let status = self.deps.rows.status(row)?;
                if ![
                    "said",
                    "proposed",
                    "Planning",
                    "planned",
                    "built wrong",
                    "in build",
                    "implemented",
                    "verified",
                ]
                .contains(&status.as_str())
                {
                    return Err(Error::Denied(format!(
                        "row {row} cannot start from {status}"
                    )));
                }
            }
        }
        if self.stopped() {
            return Err(Error::Denied("STOP is present: no starts".into()));
        }
        let fingerprint = self
            .campaign
            .root
            .join(format!("controller/wave{n}.fingerprint"));
        if fingerprint.exists() && read(&fingerprint)? != hash.as_bytes() {
            return Err(Error::Denied(format!(
                "wave {n} is pinned at controller/wave{n}.staged.json; edited inputs need a separate prepared wave"
            )));
        }
        self.deps.agents.start(
            &format!("{}:release:{n}:{hash}", self.campaign.run),
            &self.campaign,
            &wave.tasks,
        )?;
        // A successful native start is the first effect. A refusal above leaves every
        // controller file and assignment exactly as it was.
        self.state.current_wave = n;
        write(
            &self
                .campaign
                .root
                .join(format!("controller/wave{n}.staged.json")),
            &bytes,
        )?;
        write(
            &self
                .campaign
                .root
                .join(format!("controller/wave{n}.fingerprint")),
            hash.as_bytes(),
        )?;
        self.state.pins.insert(n, hash);
        self.state.last_progress = self.deps.clock.now();
        for task in &wave.tasks {
            self.state
                .tasks
                .insert(task.id.clone(), TaskRecord::default());
        }
        for task in &wave.tasks {
            for row in &task.rows {
                if !["implemented", "verified"].contains(&self.deps.rows.status(row)?.as_str()) {
                    self.deps.rows.set_status(
                        &format!("{}:start:{n}:{row}", self.campaign.run),
                        row,
                        "in build",
                    )?;
                }
            }
        }
        self.commit(
            None,
            "held",
            "running",
            "qream-controller",
            &format!(
                "wave {n} batch started: {}",
                wave.tasks
                    .iter()
                    .map(|t| t.id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        )?;
        for task in &wave.tasks {
            self.commit(
                Some(&task.id),
                "prepared",
                "working",
                "qream-controller",
                &format!("wave {n} batch start"),
            )?;
        }
        write(
            &self
                .campaign
                .root
                .join(format!("controller/release-wave{n}.json")),
            &serde_json::to_vec(&wave)?,
        )
    }
    fn drive(&mut self) -> Result<Vec<Decision>> {
        let mut decisions = Vec::new();
        loop {
            let decision = self.decide()?;
            decisions.push(decision.clone());
            match decision {
                Decision::Hold(_) => break,
                Decision::Finished => {
                    if !self.state.finished {
                        self.state.finished = true;
                        let at = self.deps.clock.timestamp();
                        if let Some(close) = self.state.close.as_mut() {
                            close.actual_close = Some(at);
                        }
                        self.commit(
                            None,
                            "running",
                            "finished",
                            "qream-controller",
                            "declared last wave cleared",
                        )?;
                    }
                    if let Some(close) = &self.state.close {
                        write(
                            &self.campaign.root.join("CLOSE.json"),
                            &serde_json::to_vec_pretty(close)?,
                        )?;
                    }
                    break;
                }
                Decision::ReleaseWave(n) => self.release(n)?,
                Decision::DeliverJudgment(items) => {
                    let messages = items.iter().map(|(task, seal)| JudgmentMail {
                        key: format!("{}:judgment:{task}:{seal}", self.campaign.run), task: task.clone(), seal: seal.clone(),
                        question: format!("Review {task}, seal revision {seal}, against each served row's definition of done. Return checked lines or batched blocking gaps with now/after and a reason. Routine unblocks belong to the chief."),
                    }).collect::<Vec<_>>();
                    self.deps.mail.judgments(&self.campaign, &messages)?;
                    for (t, s) in &items {
                        self.state.delivered.insert(format!("{t}:{s}"));
                    }
                    for (t, _) in items {
                        self.commit(
                            Some(&t),
                            "awaiting_review",
                            "awaiting_review",
                            "qream-controller",
                            "judgment delivered once",
                        )?;
                    }
                }
                Decision::RouteGapFill(id) => {
                    let r = &self.state.tasks[&id];
                    let gap = r
                        .gap
                        .as_ref()
                        .ok_or_else(|| Error::Denied("no gap to route".into()))?;
                    let text = format!(
                        "Gap fill {:?}: {}\nBLOCKING:\n{}",
                        gap.timing,
                        gap.reason,
                        gap.findings.join("\n")
                    );
                    self.deps.agents.deliver(
                        &format!("{}:gap:{id}:{}", self.campaign.run, r.budget.rounds),
                        &self.campaign,
                        &id,
                        &text,
                        false,
                    )?;
                    let r = self.record(&id)?;
                    r.budget.followups += 1;
                    if let Some(gap) = r.gap.as_mut() {
                        gap.delivered = true;
                    }
                    self.commit(Some(&id), "gap_fill", "gap_fill", "qream-controller", &text)?;
                }
            }
        }
        Ok(decisions)
    }
    pub fn on_event(&mut self, event: Event) -> Result<Vec<Decision>> {
        match event {
            Event::Seals { seals } => {
                for (task, hash) in &seals {
                    self.task(task)?;
                    if hash.is_empty() {
                        return Err(Error::Denied("a seal needs its revision hash".into()));
                    }
                }
                for (task, hash) in seals {
                    if !self
                        .state
                        .seals
                        .entry(task.clone())
                        .or_default()
                        .insert(hash.clone())
                    {
                        continue;
                    }
                    let record = self.record(&task)?;
                    record.seal = Some(hash.clone());
                    record.accepted = false;
                    record.checked_rows.clear();
                    self.state.unsubmitted_blockers.remove(&task);
                    self.state.last_progress = self.deps.clock.now();
                    self.commit(Some(&task), "working", "awaiting_review", "owner", &hash)?;
                }
            }
            Event::Stop => {
                self.state.paused = true;
                // Persist the stop before calling the native stop adapter.
                self.commit(None, "running", "paused", "operator", "STOP")?;
                self.deps.agents.stop(&self.campaign)?;
            }
            Event::Resume => {
                if self.campaign.root.join("STOP").exists() {
                    return Err(Error::Denied("STOP remains present; resume refused".into()));
                }
                // An explicit operator retry after refused admission follows the same
                // Trigger path; it cannot rely on a refused trigger's phantom Running state.
                if !self.state.triggered {
                    return self.trigger();
                }
                self.state.paused = false;
                self.commit(None, "paused", "running", "operator", "explicit release")?;
            }
            Event::Seal { task, hash } => {
                self.task(&task)?;
                if hash.is_empty() {
                    return Err(Error::Denied("a seal needs its revision hash".into()));
                }
                if !self
                    .state
                    .seals
                    .entry(task.clone())
                    .or_default()
                    .insert(hash.clone())
                {
                    return self.drive();
                }
                let record = self.record(&task)?;
                record.seal = Some(hash.clone());
                record.accepted = false;
                record.checked_rows.clear();
                self.state.unsubmitted_blockers.remove(&task);
                self.state.last_progress = self.deps.clock.now();
                self.commit(Some(&task), "working", "awaiting_review", "owner", &hash)?;
            }
            Event::Judgment {
                task,
                hash,
                review,
                timing,
                reason,
            } => {
                let definition = self.task(&task)?;
                let key = format!("{task}:{hash}");
                if self.state.judged.contains(&key) {
                    return self.drive();
                }
                if self.state.tasks[&task].seal.as_ref() != Some(&hash)
                    || !self.state.delivered.contains(&key)
                {
                    return Err(Error::Denied(
                        "judgment must name the delivered current seal revision".into(),
                    ));
                }
                let info = self.deps.agents.info(&self.campaign, &task)?;
                if info.active || info.owned_job.is_some() {
                    return Err(Error::Denied(
                        "cannot judge acceptance while the owner has live work".into(),
                    ));
                }
                let mut checked = BTreeSet::new();
                let mut findings = Vec::new();
                let mut seen = BTreeSet::new();
                for line in review.lines().filter(|l| !l.trim().is_empty()) {
                    let (row, verdict) = line.split_once(": ").ok_or_else(|| {
                        Error::Denied(
                            "review lines must say row: checked or row: not checked, gap".into(),
                        )
                    })?;
                    if !definition.rows.iter().any(|r| r == row) || !seen.insert(row.to_owned()) {
                        return Err(Error::Denied(
                            "review must cover each served row once; no added criteria".into(),
                        ));
                    }
                    if verdict == "checked" {
                        checked.insert(row.to_owned());
                    } else if verdict
                        .strip_prefix("not checked, ")
                        .is_some_and(|gap| !gap.trim().is_empty())
                    {
                        findings.push(line.to_owned());
                    } else {
                        return Err(Error::Denied(
                            "review needs checked or not checked with a concrete gap".into(),
                        ));
                    }
                }
                if seen.len() != definition.rows.len() {
                    return Err(Error::Denied("review must cover every served row".into()));
                }
                if !findings.is_empty() && reason.trim().is_empty() {
                    return Err(Error::Denied("gap fill needs its timing reason".into()));
                }
                let partial = self.deps.rows.partial(&self.campaign, &task)?;
                for row in &checked {
                    if !partial.contains(row)
                        && !["implemented", "verified"]
                            .contains(&self.deps.rows.status(row)?.as_str())
                    {
                        self.deps.rows.set_status(
                            &format!("{key}:{row}:implemented"),
                            row,
                            "implemented",
                        )?;
                    }
                    self.deps
                        .rows
                        .check(&format!("{key}:{row}:check"), row, &review)?;
                }
                let record = self.record(&task)?;
                record.checked_rows = checked;
                let to;
                if findings.is_empty() {
                    record.accepted = true;
                    if let Some(gap) = record.gap.as_mut() {
                        gap.resolved = true;
                    }
                    to = "accepted";
                } else if record.budget.rounds >= 2 || record.budget.followups >= 6 {
                    record.escalated = true;
                    record.accepted = false;
                    to = "needs_attention";
                } else {
                    record.budget.rounds += 1;
                    record.accepted = false;
                    record.gap = Some(Gap {
                        timing,
                        reason: reason.clone(),
                        findings: findings.clone(),
                        delivered: false,
                        resolved: false,
                    });
                    to = "gap_fill";
                }
                let escalated = record.escalated;
                if escalated {
                    self.deps.mail.send(&format!("{}:escalation:{key}", self.campaign.run), &self.campaign, &format!("Judgment escalation for {task}: correction budget exhausted. Diagnosis: {reason}; {}. Chief supplies a changed bounded plan to the same owner; no routine unblock to the operator.", findings.join("; ")))?;
                }
                self.state.judged.insert(key);
                self.state.last_progress = self.deps.clock.now();
                self.commit(Some(&task), "awaiting_review", to, "chief", &review)?;
                let mut gaps = Vec::new();
                for (id, r) in &self.state.tasks {
                    if let Some(gap) = &r.gap {
                        gaps.extend(serde_json::to_vec(
                            &serde_json::json!({"task": id, "gap": gap}),
                        )?);
                        gaps.push(b'\n');
                    }
                }
                write(&self.campaign.root.join("controller/gapfill.jsonl"), &gaps)?;
                let mut judgments = Vec::new();
                for revision in &self.state.judged {
                    judgments.extend(serde_json::to_vec(
                        &serde_json::json!({"revision": revision}),
                    )?);
                    judgments.push(b'\n');
                }
                write(
                    &self.campaign.root.join("controller/judgments.jsonl"),
                    &judgments,
                )?;
            }
            Event::Accept { task, evidence, by } => {
                self.task(&task)?;
                if !["chief", "operator"].contains(&by.as_str())
                    || evidence.trim().is_empty()
                    || self.state.tasks[&task].seal.is_none()
                {
                    return Err(Error::Denied("acceptance requires a seal, evidence and an explicit chief or operator act".into()));
                }
                let info = self.deps.agents.info(&self.campaign, &task)?;
                if info.active || info.owned_job.is_some() {
                    return Err(Error::Denied(
                        "acceptance refused: owner has live work".into(),
                    ));
                }
                self.record(&task)?.accepted = true;
                self.commit(Some(&task), "awaiting_review", "accepted", &by, &evidence)?;
            }
            Event::RowChecked { row, evidence } => {
                let wave = self.wave(self.state.current_wave)?.0;
                if !wave.tasks.iter().any(|t| t.rows.contains(&row)) || evidence.trim().is_empty() {
                    return Err(Error::Denied(
                        "row check needs a served row and review evidence".into(),
                    ));
                }
                self.deps.rows.check(
                    &format!("{}:row:{row}:{evidence}", self.campaign.run),
                    &row,
                    &evidence,
                )?;
                for task in &wave.tasks {
                    if task.rows.contains(&row) {
                        self.record(&task.id)?.checked_rows.insert(row.clone());
                    }
                }
                self.commit(None, "unchecked", "checked", "chief", &evidence)?;
            }
            Event::GapResolved { task, evidence } => {
                if evidence.trim().is_empty()
                    || !self.state.tasks.get(&task).is_some_and(|r| r.accepted)
                {
                    return Err(Error::Denied(
                        "gap resolution requires acceptance and evidence".into(),
                    ));
                }
                let gap = self
                    .record(&task)?
                    .gap
                    .as_mut()
                    .ok_or_else(|| Error::Denied("task has no gap fill".into()))?;
                gap.resolved = true;
                self.commit(Some(&task), "gap_fill", "accepted", "chief", &evidence)?;
            }
            Event::JobEnd { task } | Event::AgentChange { task } => {
                self.task(&task)?;
                let info = self.deps.agents.info(&self.campaign, &task)?;
                if info.ended && info.blocked.is_some() && self.state.tasks[&task].seal.is_none() {
                    self.state.unsubmitted_blockers.insert(task.clone());
                } else if self.state.tasks[&task].seal.is_some() {
                    self.state.unsubmitted_blockers.remove(&task);
                }
                let to = self.task_state(&task)?;
                self.state.last_progress = self.deps.clock.now();
                self.commit(
                    Some(&task),
                    "observed",
                    &to,
                    "qream-controller",
                    "native agent/job event",
                )?;
            }
            Event::Redeliver {
                task,
                request,
                text,
                refresh,
            } => {
                self.task(&task)?;
                let key = format!("{}:continuation:{task}:{request}", self.campaign.run);
                if request.is_empty() || text.is_empty() {
                    return Err(Error::Denied(
                        "continuation needs a request identity and bounded prompt".into(),
                    ));
                }
                if self.state.delivered.contains(&key) {
                    return self.drive();
                }
                if self.stopped() {
                    return Err(Error::Denied("STOP holds re-delivery and refresh".into()));
                }
                let info = self.deps.agents.info(&self.campaign, &task)?;
                if info.blocked.is_some()
                    || !info.interactive_ready
                    || info.active
                    || info.owned_job.is_some()
                {
                    return Err(Error::Denied(
                        "native account, approval, quota or busy state holds continuation".into(),
                    ));
                }
                let budget = &self.state.tasks[&task].budget;
                if budget.followups >= 6 || budget.installs > 3 {
                    self.record(&task)?.escalated = true;
                    self.commit(
                        Some(&task),
                        "halted",
                        "needs_attention",
                        "qream-controller",
                        "continuation budget exhausted",
                    )?;
                } else {
                    self.deps
                        .agents
                        .deliver(&key, &self.campaign, &task, &text, refresh)?;
                    self.record(&task)?.budget.followups += 1;
                    self.state.delivered.insert(key);
                    self.commit(Some(&task), "halted", "working", "chief", &text)?;
                }
            }
            Event::Install { task } => {
                self.task(&task)?;
                let r = self.record(&task)?;
                r.budget.installs += 1;
                if r.budget.installs > 3 {
                    r.escalated = true;
                }
                self.commit(
                    Some(&task),
                    "observed",
                    "observed",
                    "qream-controller",
                    "install budget recorded",
                )?;
            }
            Event::ClosingVerdict {
                faults,
                next_campaign,
                close,
            } => {
                if self.state.current_wave != self.state.close.as_ref().map_or(0, |c| c.last_wave)
                    || faults.is_empty()
                    || next_campaign == self.campaign.name
                    || close.campaign != next_campaign
                    || close.last_wave == 0
                {
                    return Err(Error::Denied("closing faults require the declared last wave and a new campaign with its own close".into()));
                }
                let wave = self.wave(self.state.current_wave)?.0;
                for (row, verdict) in &faults {
                    if !wave.tasks.iter().any(|t| t.rows.contains(row)) || verdict.trim().is_empty()
                    {
                        return Err(Error::Denied(
                            "closing verdict needs served rows and evidence".into(),
                        ));
                    }
                    self.deps.rows.closing_fault(
                        &format!("{}:closing:{row}", self.campaign.run),
                        row,
                        verdict,
                    )?;
                    self.state.closing_faults.insert(row.clone());
                }
                write(
                    &self.campaign.root.join("controller/next-campaign.json"),
                    &serde_json::to_vec_pretty(
                        &serde_json::json!({"campaign": next_campaign, "state": "prepared", "close": close, "faults": faults}),
                    )?,
                )?;
                self.commit(
                    None,
                    "closing_check",
                    "verdict",
                    "non-author",
                    "faults recorded; repair is a new prepared campaign",
                )?;
            }
        }
        let decisions = self.drive()?;
        let state = self.campaign_state()?;
        if state.starts_with("stalled") && !self.stopped() {
            let key = format!(
                "{}:stall-judgment:{}",
                self.campaign.run,
                self.state.idle_since.unwrap_or(self.deps.clock.now())
            );
            if !self.state.delivered.contains(&key) {
                self.deps.mail.send(&key, &self.campaign, &format!("Judgment needed: {state}. Chief resolves routine stalls with a bounded continuation in each owner's history; do not route routine unblocks to the operator."))?;
                self.state.delivered.insert(key);
                self.commit(
                    None,
                    "stalled",
                    "stalled",
                    "qream-controller",
                    "stall judgment delivered once to chief",
                )?;
            }
        }
        Ok(decisions)
    }
    pub fn task_state(&self, task: &str) -> Result<String> {
        let record = self
            .state
            .tasks
            .get(task)
            .ok_or_else(|| Error::Denied("task has not started".into()))?;
        let info = self.deps.agents.info(&self.campaign, task)?;
        Ok(if record.accepted {
            "accepted".into()
        } else if self.stopped() {
            "stopped".into()
        } else if record
            .seal
            .as_ref()
            .is_some_and(|hash| !self.state.judged.contains(&format!("{task}:{hash}")))
        {
            "awaiting_review".into()
        } else if self.state.unsubmitted_blockers.contains(task) {
            "needs_attention".into()
        } else if let Some(reason) = info.blocked {
            format!("blocked ({reason})")
        } else if record.escalated {
            "needs_attention".into()
        } else if record.gap.as_ref().is_some_and(|g| !g.resolved) {
            "gap_fill".into()
        } else if info.active {
            "working".into()
        } else if let Some(job) = info.owned_job {
            format!("waiting_on_job ({job})")
        } else {
            "needs_attention".into()
        })
    }
    /// Computed from fresh owned work; callers provide native events, never model ticks.
    pub fn campaign_state(&self) -> Result<String> {
        if self.state.finished {
            return Ok("finished".into());
        }
        if self.stopped() {
            return Ok("paused".into());
        }
        if !self.state.triggered {
            return Ok("prepared".into());
        }
        let now = self.deps.clock.now();
        let mut idle = Vec::new();
        let mut fresh_work = false;
        for task in self.state.tasks.keys() {
            let info = self.deps.agents.info(&self.campaign, task)?;
            if now.saturating_sub(info.observed_at) <= 15
                && (info.active || info.owned_job.is_some())
            {
                fresh_work = true;
            } else {
                idle.push(task.clone());
            }
        }
        if now.saturating_sub(self.state.observed_at) > 15 {
            return Ok(format!(
                "stale ({} seconds since controller observation)",
                now.saturating_sub(self.state.observed_at)
            ));
        }
        let idle_seconds = self
            .state
            .idle_since
            .map_or(0, |since| now.saturating_sub(since));
        if !fresh_work && idle_seconds >= 600 {
            Ok(format!(
                "stalled ({} minutes; no worker working: {})",
                idle_seconds / 60,
                idle.join(", ")
            ))
        } else {
            Ok("running".into())
        }
    }
}
