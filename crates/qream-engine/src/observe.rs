//! The state model's inputs read from the real sources, once per observation: the
//! campaign's waves and close from its folder, accepted and stopped words from the
//! control ledger, seals from `artifacts/TNN/freeze.json`, and agents and their child
//! processes from the terminal layer's panes (a worker pane carries `QREAM_TASK`).
use crate::{
    Result,
    close::Close,
    folder::Directive,
    ledger::CampaignLedger,
    rows::RowSource,
    states::{
        AgentObservation, AgentSource, BlockKind, Blocker, CampaignState, CloseLine, Job,
        JobSource, ProcessView, SealSource, StateInputs, TaskWord, WaveState, WaveWord, Words,
        derive,
    },
};
use chrono::{DateTime, FixedOffset, Local};
use qream_term::{
    agent::{AgentInfo, AgentKind, AgentState, BlockedKind},
    detect::Detector,
};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

/// One worker pane as the terminal layer sees it.
#[derive(Clone, Debug)]
pub struct WorkerPane {
    /// The pane's id; empty when unknown.
    pub pane: String,
    pub task: String,
    pub agent: Option<AgentInfo>,
    pub children: Vec<u32>,
}

/// Tasks per wave from `waveN.json`, in wave order.
pub fn waves(root: &Path) -> Result<BTreeMap<u32, Vec<String>>> {
    Ok(manifests(root)?
        .into_iter()
        .map(|(n, tasks)| (n, tasks.into_iter().map(|(id, _, _)| id).collect()))
        .collect())
}
/// A manifest task: its id, its directive and its harness kind.
pub type TaskEntry = (String, Option<PathBuf>, String);
/// Each wave's tasks with their directive paths.
pub fn manifests(root: &Path) -> Result<BTreeMap<u32, Vec<TaskEntry>>> {
    let mut waves = BTreeMap::new();
    let entries = std::fs::read_dir(root).map_err(|e| crate::Error::io(root, e))?;
    for entry in entries {
        let path = entry.map_err(|e| crate::Error::io(root, e))?.path();
        let Some(n) = path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_prefix("wave")?.strip_suffix(".json"))
            .and_then(|n| n.parse::<u32>().ok())
        else {
            continue;
        };
        let manifest: Value = crate::storage::read(&path)?;
        let tasks = manifest["tasks"]
            .as_array()
            .map(|tasks| {
                tasks
                    .iter()
                    .filter_map(|t| {
                        Some((
                            t["id"].as_str()?.to_owned(),
                            t["directive"].as_str().map(PathBuf::from),
                            t["kind"].as_str().unwrap_or("").to_owned(),
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default();
        waves.insert(n, tasks);
    }
    Ok(waves)
}

/// How long after its start a harness may take to draw its first prompt.
pub const START_GRACE_SECONDS: i64 = 30;

/// Uses the terminal layer's resident rules, including its ancestor check.
/// A resident helper is session infrastructure rather than a worker's job.
pub struct WorkerProcesses(Detector);
impl WorkerProcesses {
    pub fn codex() -> Result<Self> {
        Ok(Self(Detector::for_kind(AgentKind::Codex)?))
    }
    pub fn is_resident(&self, pid: u32) -> bool {
        self.0.is_resident(pid)
    }
}

struct Agents(
    BTreeMap<String, WorkerPane>,
    PathBuf,
    BTreeSet<String>,
    WorkerProcesses,
);
impl Agents {
    fn children(&self, task: &str) -> Vec<u32> {
        self.0.get(task).map_or_else(Vec::new, |pane| {
            pane.children
                .iter()
                .copied()
                .filter(|pid| !self.2.contains(task) || !self.3.is_resident(*pid))
                .collect()
        })
    }
}
impl AgentSource for Agents {
    fn agent(&self, task: &str) -> Option<AgentObservation> {
        let pane = self.0.get(task)?;
        let info = pane.agent.as_ref()?;
        let blocker = (info.state == AgentState::Blocked).then(|| Blocker {
            kind: match info.blocked_kind {
                Some(BlockedKind::Question) | None => BlockKind::Question,
                Some(BlockedKind::Approval) => BlockKind::Dialog,
                Some(BlockedKind::Quota) => BlockKind::Quota,
                Some(BlockedKind::Account) => BlockKind::Account,
            },
            reason: info
                .evidence
                .text
                .lines()
                .last()
                .unwrap_or("")
                .trim()
                .into(),
        });
        // A harness younger than the start grace that is neither blocked nor gone is a live
        // launch: Working, never Needs attention before it has drawn its prompt.
        // A named job (`qream job`) the worker already started is its wait, not a launch.
        let named_job = crate::job::list(&self.1, task)
            .unwrap_or_default()
            .iter()
            .any(|j| j.ended_at.is_none());
        let launching = matches!(info.state, AgentState::Idle | AgentState::Unknown)
            && self.children(task).is_empty()
            && !named_job
            && DateTime::parse_from_rfc3339(&info.started_at).is_ok_and(|at| {
                (Local::now().fixed_offset() - at).num_seconds() < START_GRACE_SECONDS
            });
        // The harness's own turn events (`qream hook`) decide whether a turn is live; a reading of
        // the screen decides only for a worker whose harness has reported nothing since its launch.
        let hooked =
            turn_record(&self.1, task).filter(|(_, at)| at.as_str() >= info.started_at.as_str());
        Some(AgentObservation {
            // A child under an idle prompt is the worker's job, not a turn: Waiting on its job.
            live_turn: match hooked {
                Some((active, _)) => active,
                None => {
                    launching
                        || (info.state == AgentState::Working
                            && info.evidence.rule != "process.tool")
                }
            },
            launched: true,
            dead: info.state == AgentState::Done,
            blocker,
            observed_at: DateTime::parse_from_rfc3339(&info.observed_at)
                .unwrap_or_else(|_| Local::now().fixed_offset()),
        })
    }
}
/// A task's last turn event from its harness's hook: whether a turn is live, and when it was recorded.
fn turn_record(root: &Path, task: &str) -> Option<(bool, String)> {
    let bytes = std::fs::read(root.join("state/turns").join(format!("{task}.json"))).ok()?;
    let record: Value = serde_json::from_slice(&bytes).ok()?;
    Some((
        record["active"].as_bool()?,
        record["at"].as_str()?.to_owned(),
    ))
}
impl JobSource for Agents {
    /// A worker's own child processes are its jobs while its harness is not mid-turn.
    /// A task's named jobs (`qream job`) come first, then its pane's own child processes.
    fn jobs(&self, task: &str) -> Vec<Job> {
        let mut named: Vec<Job> = crate::job::list(&self.1, task)
            .unwrap_or_default()
            .into_iter()
            .filter(|j| j.ended_at.is_none())
            .map(|j| Job {
                name: j.name,
                owner: task.into(),
                pid: 0,
                live: true,
            })
            .collect();
        named.extend(self.children(task).into_iter().map(|pid| {
            Job {
                name: std::fs::read_to_string(format!("/proc/{pid}/comm"))
                    .map(|c| c.trim().to_owned())
                    .unwrap_or_else(|_| format!("process {pid}")),
                owner: task.into(),
                pid,
                live: true,
            }
        }));
        named
    }
}
/// A receipt is a fact; derive checks its submission, stop and gap-fill ledger facts.
struct Seals(PathBuf);
impl SealSource for Seals {
    fn sealed(&self, task: &str) -> bool {
        self.0
            .join("artifacts")
            .join(task)
            .join("freeze.json")
            .is_file()
    }
    fn sealed_at(&self, task: &str) -> Option<DateTime<FixedOffset>> {
        let path = self.0.join("artifacts").join(task).join("freeze.json");
        let modified = std::fs::metadata(path).ok()?.modified().ok()?;
        Some(DateTime::<Local>::from(modified).fixed_offset())
    }
}

/// What the stop and recovery engine knows beside the folder: holds, the owned processes,
/// when a worker last worked, and whether the close estimate may be written.
#[derive(Clone, Debug, Default)]
pub struct ControlView {
    /// A campaign-wide pause has settled: nothing of the campaign runs.
    pub paused: bool,
    /// A campaign-wide kill holds the campaign.
    pub killed: bool,
    /// When a task of the campaign was last Working or Waiting on its job in this daemon.
    pub last_active: Option<DateTime<FixedOffset>>,
    pub processes: Vec<ProcessView>,
    /// False on a read-only daemon: the estimate is computed but not kept.
    pub persist_close: bool,
}

/// The first clause of a text, at most sixty characters.
pub fn words(text: &str) -> String {
    let clause = text
        .split([',', ';', ':', '.'])
        .next()
        .unwrap_or("")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if clause.chars().count() <= 60 {
        return clause;
    }
    let cut: String = clause.chars().take(60).collect();
    cut.rsplit_once(' ')
        .map_or(cut.clone(), |(head, _)| head.to_owned())
}

/// Derive the one state of a campaign from its folder, ledger, worker panes and rows.
/// Rows are named in words when a store is given; without one, ids stand alone.
pub fn campaign_state(
    name: &str,
    root: &Path,
    panes: Vec<WorkerPane>,
    rows: Option<&dyn RowSource>,
    now: DateTime<FixedOffset>,
    control: &ControlView,
) -> Result<CampaignState> {
    let manifests = manifests(root)?;
    let waves = waves(root)?;
    let mut ledger = CampaignLedger::read(root, name)?;
    for hold in crate::control::load_holds(root)?
        .into_iter()
        .filter(|h| !h.pending)
    {
        for (wave, tasks) in &waves {
            for task in tasks {
                if hold
                    .scope
                    .covers(crate::owned::Role::Worker, Some(*wave), Some(task))
                {
                    ledger.held.insert(task.clone());
                }
            }
        }
    }
    let close = Close::read(root).ok();
    let last_wave = close
        .as_ref()
        .map(|c| c.last_wave)
        .or_else(|| waves.keys().max().copied())
        .unwrap_or(0);
    let tasks: Vec<String> = waves
        .iter()
        .filter(|(n, _)| **n <= last_wave)
        .flat_map(|(_, t)| t.iter().cloned())
        .collect();
    // A cancelled task of a closed run is done for every purpose but acceptance: never current.
    let accepted = |t: &String| {
        ledger.latest.get(t) == Some(&crate::states::TaskWord::Accepted)
            || ledger.cancelled.contains(t)
    };
    let finished = close.as_ref().is_some_and(|c| c.actual_close.is_some())
        || (!tasks.is_empty() && tasks.iter().all(accepted));
    let current_wave = waves
        .iter()
        .filter(|(n, _)| **n <= last_wave)
        .find(|(_, t)| !t.iter().all(accepted))
        .map_or(last_wave, |(n, _)| *n);
    let unreleased = waves
        .iter()
        .filter(|(n, _)| **n > current_wave)
        .flat_map(|(_, t)| t.iter().cloned())
        .collect();
    let seals = Seals(root.to_owned());
    let pace_tasks: Vec<crate::close::PaceTask> = manifests
        .iter()
        .filter(|(n, _)| **n <= last_wave)
        .flat_map(|(n, tasks)| {
            tasks.iter().map(|(id, _, kind)| crate::close::PaceTask {
                wave: *n,
                kind: kind.clone(),
                started: ledger.started.get(id).copied(),
                sealed: seals.sealed_at(id),
                accepted: ledger.accepted_at.get(id).copied(),
                cancelled: ledger.cancelled.contains(id),
            })
        })
        .collect();
    let agents = Agents(
        panes.into_iter().map(|p| (p.task.clone(), p)).collect(),
        root.to_owned(),
        manifests
            .values()
            .flatten()
            .filter(|(_, _, kind)| kind == "codex")
            .map(|(task, _, _)| task.clone())
            .collect(),
        WorkerProcesses::codex()?,
    );
    let mut state = derive(StateInputs {
        campaign: name.into(),
        tasks,
        now,
        observed_at: now,
        launched_at: ledger.first,
        // Activity is the latest of a ledger transition and a worker seen working.
        inactive_since: ledger
            .last
            .into_iter()
            .chain(control.last_active)
            .max()
            .unwrap_or(now),
        current_wave,
        finished,
        stopped: root.join("STOP").exists() || control.killed,
        paused: control.paused,
        agents: &agents,
        jobs: &agents,
        ledger: &ledger,
        seals: &seals,
        unreleased,
    });
    let word_of = |id: &str| -> Words {
        let text = rows
            .and_then(|r| r.get(id).ok().flatten())
            .map(|row| words(&row.purpose))
            .unwrap_or_default();
        Words {
            id: id.into(),
            words: text,
        }
    };
    let mut directives = BTreeMap::new();
    for (n, tasks) in &manifests {
        for (id, path, _) in tasks {
            directives.insert(id.clone(), (*n, path.clone()));
        }
    }
    for task in &mut state.tasks {
        task.pane = agents
            .0
            .get(&task.task)
            .map(|p| p.pane.clone())
            .filter(|p| !p.is_empty());
        let Some((wave, path)) = directives.get(&task.task) else {
            continue;
        };
        task.wave = *wave;
        task.directive = path.as_ref().map(|p| {
            if p.is_absolute() {
                p.clone()
            } else {
                root.join(p)
            }
        });
        let Some(directive) = path.as_deref().and_then(|p| Directive::open(p).ok()) else {
            continue;
        };
        let title = directive.title.trim();
        task.title = title
            .strip_prefix(task.task.as_str())
            .map_or(title, |t| t.trim_start_matches([' ', '-', '—', ':', '·']))
            .to_owned();
        task.rows = directive.served().keys().map(|id| word_of(id)).collect();
        let group = rows.and_then(|r| {
            let first = task.rows.first()?;
            let id = r.get(&first.id).ok().flatten()?.outcome_id;
            let outcome = r.group(&id).ok().flatten()?.outcome;
            Some(Words {
                id,
                words: words(&outcome),
            })
        });
        task.group = group;
    }
    for task in &mut state.tasks {
        task.trace = crate::states::Trace {
            rows: task.rows.iter().map(|r| r.id.clone()).collect(),
            group: task.group.as_ref().map(|g| g.id.clone()),
            campaign: name.into(),
        };
        task.seal = crate::storage::read::<Value>(
            &root.join("artifacts").join(&task.task).join("freeze.json"),
        )
        .ok()
        .and_then(|f| f["version"].as_u64().or(Some(1)));
        task.evidence = match task.word {
            TaskWord::WaitingOnJob => format!("its job {}", task.job.clone().unwrap_or_default()),
            TaskWord::AwaitingReview => format!("seal version {}", task.seal.unwrap_or(1)),
            TaskWord::Blocked => task
                .blocker
                .as_ref()
                .map(|b| b.reason.clone())
                .unwrap_or_default(),
            _ => ledger.evidence.get(&task.task).cloned().unwrap_or_default(),
        };
    }
    let left = |n: &u32| {
        state
            .tasks
            .iter()
            .filter(|t| {
                t.wave == *n && t.word != TaskWord::Accepted && !ledger.cancelled.contains(&t.task)
            })
            .count()
    };
    state.waves = waves
        .keys()
        .filter(|n| **n <= last_wave)
        .map(|n| {
            let remaining = left(n);
            let (word, reason) = if remaining == 0 {
                (WaveWord::Closed, None)
            } else if *n == current_wave {
                (WaveWord::Released, None)
            } else if *n > current_wave {
                (
                    WaveWord::Held,
                    Some(format!("waits for wave {current_wave}")),
                )
            } else {
                (WaveWord::Open, None)
            };
            WaveState {
                n: *n,
                state: word,
                reason,
                remaining,
            }
        })
        .collect();
    state.last_wave = last_wave;
    state.waves_left = state
        .waves
        .iter()
        .filter(|w| w.state != WaveWord::Closed)
        .count();
    state.tasks_left = state
        .tasks
        .iter()
        .filter(|t| t.word != TaskWord::Accepted && !ledger.cancelled.contains(&t.task))
        .count();
    state.close_line = close.as_ref().map_or_else(String::new, |c| {
        let was = c
            .previous_estimate
            .map_or_else(String::new, |p| format!(" (was {})", p.format("%H:%M")));
        format!(
            "Closes after wave {} · {} waves, {} tasks left · expected {}{was}",
            c.last_wave,
            state.waves_left,
            state.tasks_left,
            c.expected_close.format("%H:%M")
        )
    });
    state.close = match close {
        Some(c) => {
            let mut estimate = crate::close::Estimate::read(root)?;
            if estimate.expected.is_none() {
                estimate.expected = Some(c.expected_close);
            }
            if let Some(next) = crate::close::pace_close(&pace_tasks, now)
                && estimate.advance(next)
                && control.persist_close
            {
                estimate.write(root)?;
            }
            Some(CloseLine {
                expected_close: estimate.expected.unwrap_or(c.expected_close),
                declared_close: Some(c.expected_close),
                previous: estimate.previous.or(c.previous_estimate),
                previous_close: estimate.previous,
            })
        }
        None => None,
    };
    state.processes = control
        .processes
        .iter()
        .filter_map(|process| {
            let mut process = process.clone();
            if agents.2.contains(&process.owner) {
                process.pids.retain(|pid| !agents.3.is_resident(*pid));
            }
            (!process.pids.is_empty()).then_some(process)
        })
        .collect();
    Ok(state)
}
