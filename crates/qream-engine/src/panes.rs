//! The control engine's view of a real pane: the daemon's session store owns the PTY and its
//! journal, the live-pane table owns the running process; this handle reaches both by the
//! pane's stable id, so stops, interrupts and the recovery act act on the pane the worker runs in.
use crate::{
    control::UnitRecord,
    recover::{Opener, resume_argv},
};
use parking_lot::Mutex;
use qream_term::{
    Error as TermError, Result as TermResult,
    agent::{AgentSpec, LaunchRecord},
    core::{LivePanes, journal_screen},
    handle::{PaneHandle, Screen},
    session::SessionStore,
};
use std::sync::Arc;
use tokio::sync::broadcast;

pub struct SessionPane {
    pub id: String,
    pub sessions: Arc<Mutex<SessionStore>>,
    pub live: LivePanes,
}

impl SessionPane {
    pub fn new(id: &str, sessions: Arc<Mutex<SessionStore>>, live: LivePanes) -> Self {
        Self {
            id: id.into(),
            sessions,
            live,
        }
    }
}

impl PaneHandle for SessionPane {
    fn spawn(&mut self, spec: &AgentSpec) -> TermResult<u32> {
        let record = self.sessions.lock().start(&self.id, spec.clone())?;
        record
            .pid
            .ok_or_else(|| TermError::Lost(format!("{}: no process id", self.id)))
    }
    /// The harness's own resume form over the frozen arguments, in this same pane.
    fn resume(&mut self, record: &LaunchRecord) -> TermResult<u32> {
        let argv = resume_argv(record).map_err(|e| TermError::Invalid(e.to_string()))?;
        let resumed = self.sessions.lock().resume(&self.id, argv)?;
        resumed
            .pid
            .ok_or_else(|| TermError::Lost(format!("{}: no process id", self.id)))
    }
    fn write(&mut self, bytes: &[u8]) -> TermResult<()> {
        self.live.get(&self.id)?.write(bytes)
    }
    fn resize(&mut self, cols: u16, rows: u16) -> TermResult<()> {
        self.sessions.lock().resize(&self.id, cols, rows)
    }
    fn subscribe(&self) -> broadcast::Receiver<Screen> {
        // The session store journals this pane's screens; the engine reads them on demand.
        broadcast::channel(1).1
    }
    fn text(&self, lines: usize) -> TermResult<Screen> {
        let pane = self.live.get(&self.id)?;
        journal_screen(&pane, lines)
    }
    fn child_pids(&self) -> TermResult<Vec<u32>> {
        let Ok(pane) = self.live.get(&self.id) else {
            return Ok(Vec::new());
        };
        let root = pane.pid();
        Ok(pane
            .child_pids()?
            .into_iter()
            .filter(|pid| Some(*pid) != root)
            .collect())
    }
    /// A pane with no live process in this daemon has exited for the engine's purposes.
    fn exited(&self) -> TermResult<bool> {
        Ok(self
            .live
            .get(&self.id)
            .map_or(true, |pane| pane.exited().is_some()))
    }
    /// Ends the pane's process; the journal, its history and the launch record stay.
    fn close(&mut self) -> TermResult<()> {
        if let Ok(pane) = self.live.get(&self.id) {
            pane.kill();
        }
        Ok(())
    }
}

/// Gives the recovery act the task's own pane by the id its unit recorded; never a new pane.
pub struct SessionOpener {
    pub sessions: Arc<Mutex<SessionStore>>,
    pub live: LivePanes,
}

impl Opener for SessionOpener {
    fn open(&mut self, unit: &UnitRecord) -> crate::Result<Box<dyn PaneHandle>> {
        let pane = unit
            .pane
            .as_deref()
            .ok_or_else(|| crate::Error::Invalid(format!("{} has no recorded pane", unit.task)))?;
        self.sessions.lock().journal(pane)?;
        Ok(Box::new(SessionPane::new(
            pane,
            Arc::clone(&self.sessions),
            self.live.clone(),
        )))
    }
}
