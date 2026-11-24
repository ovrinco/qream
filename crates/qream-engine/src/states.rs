use chrono::{DateTime, FixedOffset};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskWord {
    Accepted,
    Stopped,
    AwaitingReview,
    Blocked,
    GapFill,
    Working,
    WaitingOnJob,
    NeedsAttention,
    /// Under a held wave and never launched: outside the attention count.
    NotStarted,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CampaignWord {
    Prepared,
    Running,
    Stalled,
    Paused,
    Finished,
    Stopped,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockKind {
    Question,
    Quota,
    Account,
    Launch,
    Dialog,
    E9,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Blocker {
    pub kind: BlockKind,
    pub reason: String,
}
#[derive(Clone, Debug)]
pub struct AgentObservation {
    pub live_turn: bool,
    pub launched: bool,
    pub dead: bool,
    pub blocker: Option<Blocker>,
    pub observed_at: DateTime<FixedOffset>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Job {
    pub name: String,
    pub owner: String,
    pub pid: u32,
    pub live: bool,
}
pub trait AgentSource {
    fn agent(&self, task: &str) -> Option<AgentObservation>;
}
pub trait JobSource {
    fn jobs(&self, task: &str) -> Vec<Job>;
}
pub trait LedgerSource {
    fn transition(&self, task: &str) -> Option<TaskWord>;
    /// An interrupt or pause takes effect as a stopped word once the agent is gone.
    fn stopped_when_gone(&self, _task: &str) -> bool {
        false
    }
    /// A source that tracks submission reports its refusal, or the missing record.
    /// Older sources already vet their seals and need no additional submission check.
    fn submission_blocker(&self, _task: &str) -> Option<Blocker> {
        None
    }
    /// When the task's last gap fill was recorded.
    fn gap_fill_at(&self, _task: &str) -> Option<DateTime<FixedOffset>> {
        None
    }
}
pub trait SealSource {
    fn sealed(&self, task: &str) -> bool;
    /// When the seal was written; a seal older than the last gap fill is stale.
    fn sealed_at(&self, _task: &str) -> Option<DateTime<FixedOffset>> {
        None
    }
}
pub struct StateInputs<'a> {
    pub campaign: String,
    pub tasks: Vec<String>,
    pub now: DateTime<FixedOffset>,
    pub observed_at: DateTime<FixedOffset>,
    pub launched_at: Option<DateTime<FixedOffset>>,
    pub inactive_since: DateTime<FixedOffset>,
    pub current_wave: u32,
    pub finished: bool,
    pub stopped: bool,
    pub paused: bool,
    pub agents: &'a dyn AgentSource,
    pub jobs: &'a dyn JobSource,
    pub ledger: &'a dyn LedgerSource,
    pub seals: &'a dyn SealSource,
    /// Tasks under a wave not yet released.
    pub unreleased: std::collections::BTreeSet<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TaskState {
    pub task: String,
    pub word: TaskWord,
    pub blocker: Option<Blocker>,
    pub job: Option<String>,
    pub observed_at: DateTime<FixedOffset>,
    pub age_seconds: i64,
    pub stale: bool,
    pub processes: Vec<Job>,
    /// The directive's first heading without the task id.
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub wave: u32,
    #[serde(default)]
    pub rows: Vec<Words>,
    #[serde(default)]
    pub group: Option<Words>,
    /// What the task serves: its rows, their outcome group and the campaign.
    #[serde(default)]
    pub trace: Trace,
    /// Why the word holds: the ledger's last evidence, the seal, the job or the screen.
    #[serde(default)]
    pub evidence: String,
    /// The version of the task's latest seal (`artifacts/TNN/freeze.json`).
    #[serde(default)]
    pub seal: Option<u64>,
    /// The worker's pane, as the controller started it.
    #[serde(default)]
    pub pane: Option<String>,
    /// The directive's path; `task.directive` serves its markdown.
    #[serde(default)]
    pub directive: Option<std::path::PathBuf>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Trace {
    pub rows: Vec<String>,
    pub group: Option<String>,
    pub campaign: String,
}
/// A row or group named in a few words for the window.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Words {
    pub id: String,
    pub words: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WaveWord {
    Open,
    Held,
    Released,
    Closed,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WaveState {
    pub n: u32,
    pub state: WaveWord,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub remaining: usize,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CloseLine {
    pub expected_close: DateTime<FixedOffset>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub declared_close: Option<DateTime<FixedOffset>>,
    /// The estimate before the latest change, shown as "(was HH:MM)".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous: Option<DateTime<FixedOffset>>,
    /// The estimate before the last move of more than five minutes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_close: Option<DateTime<FixedOffset>>,
}
/// One process group or unit QREAM started for the campaign, with its owner.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessView {
    pub owner: String,
    pub role: String,
    pub purpose: String,
    pub what: String,
    pub pids: Vec<u32>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CampaignState {
    pub campaign: String,
    pub word: CampaignWord,
    pub current_wave: u32,
    pub tasks: Vec<TaskState>,
    pub counts: BTreeMap<TaskWord, usize>,
    pub observed_at: DateTime<FixedOffset>,
    pub age_seconds: i64,
    pub stale: bool,
    pub stalled_minutes: Option<i64>,
    pub reason: Option<String>,
    #[serde(default)]
    pub waves: Vec<WaveState>,
    #[serde(default)]
    pub last_wave: u32,
    #[serde(default)]
    pub waves_left: usize,
    #[serde(default)]
    pub tasks_left: usize,
    #[serde(default)]
    pub close: Option<CloseLine>,
    #[serde(default)]
    pub finished: bool,
    #[serde(default)]
    pub stalled: bool,
    /// "Closes after wave N · k waves, m tasks left · expected HH:MM (was HH:MM)".
    #[serde(default)]
    pub close_line: String,
    /// Every live process the campaign owns, with its owner.
    #[serde(default)]
    pub processes: Vec<ProcessView>,
}
pub fn derive(inputs: StateInputs<'_>) -> CampaignState {
    let mut tasks = vec![];
    let mut counts = BTreeMap::new();
    let mut oldest = inputs.observed_at;
    let mut ids = inputs.tasks.clone();
    ids.sort();
    ids.dedup();
    for task in ids {
        let agent = inputs.agents.agent(&task);
        let jobs: Vec<_> = inputs
            .jobs
            .jobs(&task)
            .into_iter()
            .filter(|j| j.live && j.owner == task)
            .collect();
        let transition = inputs.ledger.transition(&task);
        let observed_at = agent.as_ref().map_or(inputs.observed_at, |a| a.observed_at);
        oldest = oldest.min(observed_at);
        let mut blocker = agent.as_ref().and_then(|a| a.blocker.clone());
        if agent.as_ref().is_some_and(|a| a.dead) && blocker.is_none() {
            blocker = Some(Blocker {
                kind: BlockKind::E9,
                reason: "The agent exited without a seal".into(),
            });
        }
        let live_turn = agent.as_ref().is_some_and(|a| a.launched && a.live_turn);
        // A seal counts only when newer than the last gap fill and the agent is idle.
        let fresh_seal = inputs.seals.sealed(&task)
            && match (
                inputs.seals.sealed_at(&task),
                inputs.ledger.gap_fill_at(&task),
            ) {
                (Some(seal), Some(gap)) => seal > gap,
                (None, Some(_)) => false,
                (_, None) => true,
            };
        let submission_blocker = fresh_seal
            .then(|| inputs.ledger.submission_blocker(&task))
            .flatten();
        if let Some(refusal) = &submission_blocker {
            blocker = Some(refusal.clone());
        }
        let agent_gone = agent.as_ref().is_none_or(|a| a.dead) && jobs.is_empty();
        let word = if transition == Some(TaskWord::Accepted) {
            TaskWord::Accepted
        } else if inputs.stopped || transition == Some(TaskWord::Stopped) {
            TaskWord::Stopped
        } else if agent.is_none() && jobs.is_empty() && inputs.unreleased.contains(&task) {
            // A task that never started is not stopped by a pause: it stays queued.
            TaskWord::NotStarted
        } else if agent_gone && (inputs.paused || inputs.ledger.stopped_when_gone(&task)) {
            TaskWord::Stopped
        } else if fresh_seal && !live_turn && jobs.is_empty() {
            if let Some(refusal) = submission_blocker {
                blocker = Some(refusal);
                TaskWord::Blocked
            } else {
                TaskWord::AwaitingReview
            }
        } else if blocker.is_some() {
            TaskWord::Blocked
        } else if transition == Some(TaskWord::GapFill) && !live_turn && jobs.is_empty() {
            TaskWord::GapFill
        } else if live_turn {
            TaskWord::Working
        } else if !jobs.is_empty() {
            TaskWord::WaitingOnJob
        } else {
            TaskWord::NeedsAttention
        };
        let job = if word == TaskWord::WaitingOnJob {
            jobs.first().map(|j| j.name.clone())
        } else {
            None
        };
        if word != TaskWord::Blocked {
            blocker = None;
        }
        let age = (inputs.now - observed_at).num_seconds().max(0);
        *counts.entry(word.clone()).or_insert(0) += 1;
        tasks.push(TaskState {
            task,
            word,
            blocker,
            job,
            observed_at,
            age_seconds: age,
            stale: age > 15,
            processes: jobs,
            title: String::new(),
            wave: 0,
            rows: Vec::new(),
            group: None,
            trace: Trace::default(),
            evidence: String::new(),
            seal: None,
            pane: None,
            directive: None,
        });
    }
    let active = tasks
        .iter()
        .any(|t| matches!(t.word, TaskWord::Working | TaskWord::WaitingOnJob));
    let inactive = inputs.launched_at.map_or(0, |at| {
        (inputs.now - inputs.inactive_since.max(at))
            .num_seconds()
            .max(0)
    });
    let word = if inputs.finished {
        CampaignWord::Finished
    } else if inputs.stopped {
        CampaignWord::Stopped
    } else if inputs.paused {
        CampaignWord::Paused
    } else if inputs.launched_at.is_none() {
        CampaignWord::Prepared
    } else if !active && inactive > 600 {
        CampaignWord::Stalled
    } else {
        CampaignWord::Running
    };
    let stalled = word == CampaignWord::Stalled;
    let reason = if stalled {
        Some(
            tasks
                .iter()
                .find_map(|t| t.blocker.as_ref().map(|b| b.reason.clone()))
                .unwrap_or_else(|| "No worker or owned job is running".into()),
        )
    } else {
        None
    };
    let age = (inputs.now - oldest).num_seconds().max(0);
    CampaignState {
        campaign: inputs.campaign,
        word,
        current_wave: inputs.current_wave,
        tasks,
        counts,
        observed_at: oldest,
        age_seconds: age,
        stale: age > 15,
        stalled_minutes: stalled.then_some(inactive / 60),
        reason,
        waves: Vec::new(),
        last_wave: inputs.current_wave,
        waves_left: 0,
        tasks_left: 0,
        close: None,
        finished: inputs.finished,
        stalled,
        close_line: String::new(),
        processes: Vec::new(),
    }
}
