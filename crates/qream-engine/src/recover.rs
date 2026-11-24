//! The one recovery act: every halted agent resumes in its own history, in its own pane, with its
//! frozen launch arguments, and its pending work is re-delivered once; nothing done is repeated.
use crate::{
    Error, Result,
    control::{Engine, UnitRecord, UnitStatus},
    error::io,
    now,
    owned::Owned,
};
use qream_term::{
    agent::{self, AgentInfo, AgentKind, AgentState, BlockedKind, LaunchRecord},
    detect::Detector,
    handle::PaneHandle,
};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::PathBuf,
    thread,
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Halt {
    /// The harness shows its usage-limit text: a quota blocker, never a finished word.
    Quota,
    /// The harness asks for sign-in after a login change.
    Account,
    DeadProcess,
    /// The daemon restarted and the pane's process did not survive it.
    DaemonRestart,
    /// Stopped by a pause the operator has since released.
    Released,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Restored {
    pub task: String,
    pub conversation_id: String,
    pub halt: Halt,
    /// The delivery re-sent, when one was.
    pub redelivered: Option<String>,
    pub note: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Missed {
    pub task: String,
    pub reason: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Report {
    pub campaign: String,
    pub restored: Vec<Restored>,
    pub missed: Vec<Missed>,
}

/// Gives back a task's own pane when the engine holds no handle for it (after a daemon restart).
/// It reopens the pane journal named by the unit; it never creates a second pane for a task.
pub trait Opener {
    fn open(&mut self, unit: &UnitRecord) -> Result<Box<dyn PaneHandle>>;
}

/// Release uses only the pane already retained by control. Explicit recovery can reopen
/// a durable pane through the daemon's opener after a restart.
pub(crate) struct ExistingPane;
impl Opener for ExistingPane {
    fn open(&mut self, unit: &UnitRecord) -> Result<Box<dyn PaneHandle>> {
        Err(Error::Invalid(format!(
            "{}'s recorded pane is unavailable; use recovery to reopen its own pane",
            unit.task
        )))
    }
}

#[derive(Serialize, Deserialize)]
struct DeliveryLine {
    at: String,
    task: String,
    conversation_id: String,
    delivery: String,
    /// The halted process's start, so one halt re-delivers once however often recovery runs.
    halted: String,
}

/// The harness command that resumes a recorded conversation with its frozen arguments.
/// Refuses where the resume form is not settled rather than guess an argument.
pub fn resume_argv(record: &LaunchRecord) -> Result<Vec<String>> {
    let id = record
        .conversation_id
        .as_deref()
        .filter(|id| !id.is_empty())
        .ok_or_else(|| {
            Error::Invalid("no recorded conversation; a new one is never opened".into())
        })?;
    let program = match record.spec.kind {
        AgentKind::Codex => "codex",
        AgentKind::Claude => "claude",
        AgentKind::Opencode => {
            return Err(Error::Invalid(
                "the OpenCode resume form is not settled; nothing is guessed".into(),
            ));
        }
        AgentKind::Shell => {
            return Err(Error::Invalid(
                "a shell task has no conversation to resume".into(),
            ));
        }
    };
    let mut frozen = record
        .spec
        .argv
        .iter()
        .skip_while(|a| a.as_str() == program)
        .cloned()
        .peekable();
    let mut options = Vec::new();
    while let Some(arg) = frozen.next() {
        let selector = match record.spec.kind {
            AgentKind::Claude => matches!(arg.as_str(), "--session-id" | "--resume"),
            AgentKind::Codex => arg == "resume",
            _ => false,
        };
        if selector {
            if frozen.next().as_deref() != Some(id) {
                return Err(Error::Invalid(
                    "frozen arguments name a different conversation; nothing is started".into(),
                ));
            }
        } else {
            options.push(arg);
        }
    }
    Ok(crate::seat::argv(program, None, None, Some(id), None)
        .into_iter()
        .chain(options)
        .collect())
}

impl Engine {
    /// Resumes every halted agent of the campaign; prints what was restored and what was not, with why.
    pub fn recover(&mut self, opener: &mut dyn Opener) -> Result<Report> {
        let tasks: Vec<String> = self.units.keys().cloned().collect();
        self.recover_tasks(&tasks, opener)
    }

    /// The same act for one task.
    pub fn refresh(&mut self, task: &str, opener: &mut dyn Opener) -> Result<Report> {
        if !self.units.contains_key(task) {
            return Err(Error::NotFound(format!("task {task}")));
        }
        self.recover_tasks(&[task.to_string()], opener)
    }

    pub(crate) fn recover_tasks(
        &mut self,
        tasks: &[String],
        opener: &mut dyn Opener,
    ) -> Result<Report> {
        let mut report = Report {
            campaign: self.campaign.clone(),
            ..Report::default()
        };
        for task in tasks {
            // The operator's stop is honoured between every unit, during recovery too.
            let refusal = if self.stop_marker().exists() {
                Some("the campaign's STOP marker is present".to_string())
            } else {
                let u = &self.units[task];
                self.held(u.role, u.wave, task).map(|h| {
                    let kind = format!("{:?}", h.kind).to_lowercase();
                    format!("held by the operator's {kind} of {}", h.scope)
                })
            };
            if let Some(reason) = refusal {
                report.missed.push(Missed {
                    task: task.clone(),
                    reason,
                });
                continue;
            }
            match self.recover_one(task, opener) {
                Ok(Some(r)) => report.restored.push(r),
                Ok(None) => {}
                Err(e) if e.is_fault() => return Err(e),
                Err(e) => report.missed.push(Missed {
                    task: task.clone(),
                    reason: e.to_string(),
                }),
            }
        }
        Ok(report)
    }

    fn halt_of(&mut self, task: &str) -> Result<Option<Halt>> {
        let unit = self.units[task].clone();
        match unit.status {
            UnitStatus::Launching => Err(Error::Invalid("a launch is in flight".into())),
            UnitStatus::Stopped => Ok(Some(Halt::Released)),
            UnitStatus::Live => {
                let Some(handle) = self.handles.get(task) else {
                    return Ok(Some(Halt::DaemonRestart));
                };
                if handle.exited()? {
                    return Ok(Some(Halt::DeadProcess));
                }
                let info = observe(handle.as_ref(), &unit.record)?;
                Ok(match (info.state, info.blocked_kind) {
                    (AgentState::Blocked, Some(BlockedKind::Quota)) => Some(Halt::Quota),
                    (AgentState::Blocked, Some(BlockedKind::Account)) => Some(Halt::Account),
                    (AgentState::Done, _) => Some(Halt::DeadProcess),
                    _ => None,
                })
            }
        }
    }

    fn recover_one(&mut self, task: &str, opener: &mut dyn Opener) -> Result<Option<Restored>> {
        let Some(halt) = self.halt_of(task)? else {
            return Ok(None);
        };
        let unit = self.units[task].clone();
        let argv = resume_argv(&unit.record)?;
        let conversation = unit.record.conversation_id.clone().unwrap_or_default();
        let halted_key = format!(
            "{}@{}",
            unit.record.pid.unwrap_or(0),
            unit.record.started_at
        );

        // Partial work is read first: a sealed task is never given its work again.
        let sealed = self
            .root
            .join("artifacts")
            .join(task)
            .join("freeze.json")
            .exists();
        let delivered_before = self.delivered(task, &halted_key)?;

        // A harness still alive at its limit or sign-in screen ends first, so the resume reads new credentials.
        if matches!(halt, Halt::Quota | Halt::Account)
            && let Some(handle) = self.handles.get_mut(task)
        {
            handle.close()?;
        }
        self.end_task_processes(task)?;

        let mut handle = match self.handles.remove(task) {
            Some(handle) => handle,
            None => opener.open(&unit)?,
        };
        let pid = match handle.resume(&unit.record) {
            Ok(pid) => pid,
            Err(e) => {
                self.handles.insert(task.into(), handle);
                return Err(Error::Invalid(format!("resume refused: {e}")));
            }
        };
        let owned = Owned::group(&self.owner(&unit, "agent harness (resumed)"), pid)?;
        self.registry.add(owned)?;
        let mut record = unit.record.clone();
        record.pid = Some(pid);
        record.started_at = now();
        if let Some(u) = self.units.get_mut(task) {
            u.record = record.clone();
            u.status = UnitStatus::Live;
            u.reason = None;
        }
        self.save_units()?;
        self.log(
            task,
            &format!("halted:{halt:?}").to_lowercase(),
            "live",
            &format!("resumed in its own history: {}", argv.join(" ")),
        )?;

        let mut redelivered = None;
        let note = match (&unit.pending, sealed, delivered_before) {
            (_, true, _) => "already sealed; nothing re-delivered".to_string(),
            (None, false, _) => "no pending work".to_string(),
            (Some(d), false, true) => format!("{} was already re-delivered for this halt", d.id),
            (Some(d), false, false) => {
                if self.wait_idle(handle.as_mut(), &record)? {
                    let info = observe(handle.as_ref(), &record)?;
                    agent::prompt(handle.as_mut(), &info, &d.text)?;
                    self.record_delivery(task, &conversation, &d.id, &halted_key)?;
                    self.log(task, "live", "live", &format!("re-delivered {}", d.id))?;
                    redelivered = Some(d.id.clone());
                    "work re-delivered once".to_string()
                } else {
                    format!("{} left pending: the prompt never became idle", d.id)
                }
            }
        };
        self.handles.insert(task.into(), handle);
        Ok(Some(Restored {
            task: task.into(),
            conversation_id: conversation,
            halt,
            redelivered,
            note,
        }))
    }

    fn wait_idle(&self, handle: &mut dyn PaneHandle, record: &LaunchRecord) -> Result<bool> {
        let until = Instant::now() + self.deliver_wait;
        loop {
            let info = observe(handle, record)?;
            if info.state == AgentState::Idle && info.interactive_ready {
                return Ok(true);
            }
            if Instant::now() >= until || info.state == AgentState::Done {
                return Ok(false);
            }
            thread::sleep(Duration::from_millis(100));
        }
    }

    fn deliveries_path(&self) -> PathBuf {
        self.root.join("state/deliveries.jsonl")
    }

    fn delivered(&self, task: &str, halted: &str) -> Result<bool> {
        let path = self.deliveries_path();
        let text = match fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(io(&path, e)),
        };
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            let d: DeliveryLine = serde_json::from_str(line)?;
            if d.task == task && d.halted == halted {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn record_delivery(
        &self,
        task: &str,
        conversation: &str,
        delivery: &str,
        halted: &str,
    ) -> Result<()> {
        let path = self.deliveries_path();
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| io(&path, e))?;
        let mut bytes = serde_json::to_vec(&DeliveryLine {
            at: now(),
            task: task.into(),
            conversation_id: conversation.into(),
            delivery: delivery.into(),
            halted: halted.into(),
        })?;
        bytes.push(b'\n');
        file.write_all(&bytes).map_err(|e| io(&path, e))
    }
}

fn observe(handle: &dyn PaneHandle, record: &LaunchRecord) -> Result<AgentInfo> {
    let screen = handle.text(200)?;
    Ok(Detector::for_kind(record.spec.kind)?.observe(
        &screen.lines,
        &handle.child_pids()?,
        handle.exited()?,
        record.pid,
        &record.started_at,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use qream_term::agent::AgentSpec;

    fn record(kind: AgentKind, argv: &[&str], id: Option<&str>) -> LaunchRecord {
        LaunchRecord {
            spec: AgentSpec {
                kind,
                argv: argv.iter().map(|s| s.to_string()).collect(),
                env: Default::default(),
                prompt_file: None,
            },
            pid: None,
            started_at: String::new(),
            conversation_id: id.map(Into::into),
            pending_prompt: None,
        }
    }

    #[test]
    fn resume_keeps_frozen_arguments_and_never_guesses() {
        let codex = record(
            AgentKind::Codex,
            &["codex", "-m", "x", "-c", "a=b"],
            Some("abc"),
        );
        assert_eq!(
            resume_argv(&codex).ok(),
            Some(vec![
                "codex".into(),
                "resume".into(),
                "abc".into(),
                "-m".into(),
                "x".into(),
                "-c".into(),
                "a=b".into()
            ])
        );
        let claude = record(AgentKind::Claude, &["--model", "m"], Some("u1"));
        assert_eq!(
            resume_argv(&claude).ok(),
            Some(vec![
                "claude".into(),
                "--resume".into(),
                "u1".into(),
                "--model".into(),
                "m".into()
            ])
        );
        assert!(resume_argv(&record(AgentKind::Codex, &[], None)).is_err());
        assert!(resume_argv(&record(AgentKind::Opencode, &[], Some("s"))).is_err());
    }
}
