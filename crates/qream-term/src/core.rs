//! The PTY pane core behind the session layer's pane trait: `Pane` is the owned
//! process and screen, `CorePane` is its `PaneHandle`, `CoreFactory` builds and
//! rebuilds panes for the session store. `LivePanes` lets the daemon attach viewers
//! and route typed input to exactly one pane.
use crate::{
    Error, Pane, PtySpec, Result,
    agent::AgentSpec,
    handle::{PaneFactory, PaneHandle, PaneSpec, Recovery, Screen},
};
use parking_lot::Mutex;
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::broadcast;

pub const DEFAULT_COLS: u16 = 120;
pub const DEFAULT_ROWS: u16 = 40;
pub const HISTORY_LINES: usize = 10_000;
const WATCH_INTERVAL: Duration = Duration::from_millis(200);

/// Live panes of this daemon by stable pane id.
#[derive(Clone, Default)]
pub struct LivePanes(Arc<Mutex<BTreeMap<String, Arc<Pane>>>>);
impl LivePanes {
    pub fn get(&self, id: &str) -> Result<Arc<Pane>> {
        self.0
            .lock()
            .get(id)
            .cloned()
            .ok_or_else(|| Error::Lost(format!("{id}: no live pane in this daemon")))
    }
    pub fn ids(&self) -> Vec<String> {
        self.0.lock().keys().cloned().collect()
    }
    fn insert(&self, id: &str, pane: Arc<Pane>) -> Option<Arc<Pane>> {
        self.0.lock().insert(id.into(), pane)
    }
    fn remove(&self, id: &str) -> Option<Arc<Pane>> {
        self.0.lock().remove(id)
    }
}

/// The session layer's journal shape for one pane, read from the core.
pub fn journal_screen(pane: &Pane, history: usize) -> Result<Screen> {
    let (scrollback, lines) = pane.lines(history);
    let styled = pane.snapshot();
    Ok(Screen {
        scrollback,
        lines,
        cols: styled.cols,
        rows: styled.rows.len() as u16,
        state: serde_json::to_value(styled)?,
    })
}

pub struct CorePane {
    id: String,
    cwd: PathBuf,
    env: BTreeMap<String, String>,
    size: (u16, u16),
    live: LivePanes,
    pane: Arc<Pane>,
    sender: broadcast::Sender<Screen>,
    stop: Arc<AtomicBool>,
}
impl CorePane {
    fn new(id: &str, spec: &PaneSpec, size: (u16, u16), live: LivePanes, pane: Arc<Pane>) -> Self {
        let (sender, _) = broadcast::channel(16);
        let mut core = Self {
            id: id.into(),
            cwd: spec.cwd.clone(),
            env: spec.env.clone(),
            size,
            live,
            pane,
            sender,
            stop: Arc::new(AtomicBool::new(false)),
        };
        core.watch();
        core
    }
    /// One watcher per owned process publishes a screen whenever output arrives,
    /// so the session store journals with no viewer attached.
    fn watch(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let stop = Arc::new(AtomicBool::new(false));
        self.stop = Arc::clone(&stop);
        let pane: Weak<Pane> = Arc::downgrade(&self.pane);
        let sender = self.sender.clone();
        let _ = std::thread::Builder::new()
            .name("qream-pane-watch".into())
            .spawn(move || {
                let (_wait_sender, wait_receiver) = std::sync::mpsc::channel::<()>();
                let mut last = None;
                while !stop.load(Ordering::Relaxed) {
                    let Some(pane) = pane.upgrade() else { break };
                    let generation = (pane.generation(), pane.exited());
                    if last != Some(generation) {
                        last = Some(generation);
                        if let Ok(screen) = journal_screen(&pane, HISTORY_LINES) {
                            let _ = sender.send(screen);
                        }
                    }
                    drop(pane);
                    let _ = wait_receiver.recv_timeout(WATCH_INTERVAL);
                }
            });
    }
}
impl Drop for CorePane {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}
impl PaneHandle for CorePane {
    /// The harness replaces the pane's shell with its own frozen argv and environment.
    fn spawn(&mut self, spec: &AgentSpec) -> Result<u32> {
        if spec.argv.first().is_none_or(String::is_empty) {
            return Err(Error::Invalid("agent argv must name a program".into()));
        }
        let mut env = self.env.clone();
        env.extend(spec.env.clone());
        let pane = Arc::new(Pane::spawn(PtySpec {
            argv: spec.argv.clone(),
            cwd: self.cwd.clone(),
            env,
            cols: self.size.0,
            rows: self.size.1,
        })?);
        let pid = pane
            .pid()
            .ok_or_else(|| Error::Pane("the harness started without a process id".into()))?;
        if let Some(old) = self.live.insert(&self.id, Arc::clone(&pane)) {
            old.kill();
        }
        self.pane = pane;
        self.watch();
        Ok(pid)
    }
    fn write(&mut self, bytes: &[u8]) -> Result<()> {
        self.pane.write(bytes)
    }
    fn enqueue_prompt(&mut self, text: &str) -> Result<()> {
        self.pane.enqueue_prompt(text)
    }
    fn resize(&mut self, cols: u16, rows: u16) -> Result<()> {
        self.pane.resize(cols, rows)?;
        self.size = (cols, rows);
        Ok(())
    }
    fn subscribe(&self) -> broadcast::Receiver<Screen> {
        self.sender.subscribe()
    }
    fn text(&self, lines: usize) -> Result<Screen> {
        if let Some(error) = self.pane.take_error() {
            return Err(error);
        }
        journal_screen(&self.pane, lines)
    }
    /// Tools running under the pane's root process; the root itself is excluded.
    fn child_pids(&self) -> Result<Vec<u32>> {
        let root = self.pane.pid();
        Ok(self
            .pane
            .child_pids()?
            .into_iter()
            .filter(|pid| Some(*pid) != root)
            .collect())
    }
    fn exited(&self) -> Result<bool> {
        Ok(self.pane.exited().is_some())
    }
    fn close(&mut self) -> Result<()> {
        self.stop.store(true, Ordering::Relaxed);
        self.live.remove(&self.id);
        self.pane.kill();
        Ok(())
    }
}

/// Builds panes running `shell` (the pane's `SHELL`, else `/bin/sh`) until a harness starts.
#[derive(Clone, Default)]
pub struct CoreFactory {
    pub live: LivePanes,
}
impl CoreFactory {
    pub fn new(live: LivePanes) -> Self {
        Self { live }
    }
}
impl PaneFactory for CoreFactory {
    fn create(&mut self, spec: &PaneSpec, screen: &Screen) -> Result<Box<dyn PaneHandle>> {
        let size = if screen.cols > 0 && screen.rows > 0 {
            (screen.cols, screen.rows)
        } else {
            (DEFAULT_COLS, DEFAULT_ROWS)
        };
        let mut env = spec.env.clone();
        env.entry("TERM".into())
            .or_insert_with(|| "xterm-256color".into());
        let shell = env
            .get("SHELL")
            .cloned()
            .unwrap_or_else(|| "/bin/sh".into());
        // A pane rebuilt from its journal keeps its history ahead of the new shell.
        let mut history = screen.scrollback.clone();
        history.extend(screen.lines.iter().cloned());
        let pane = Arc::new(Pane::spawn_with_history(
            PtySpec {
                argv: vec![shell],
                cwd: spec.cwd.clone(),
                env,
                cols: size.0,
                rows: size.1,
            },
            &history,
        )?);
        if let Some(old) = self.live.insert(&spec.id, Arc::clone(&pane)) {
            old.kill();
        }
        Ok(Box::new(CorePane::new(
            &spec.id,
            spec,
            size,
            self.live.clone(),
            pane,
        )))
    }
    /// A pane still owned by this daemon reattaches. A journal with no live pane here
    /// means its PTY ended with the previous daemon: only then may it be rebuilt.
    fn recover(&mut self, spec: &PaneSpec, screen: &Screen) -> Result<Recovery> {
        match self.live.get(&spec.id) {
            Ok(pane) => {
                let size = (screen.cols.max(1), screen.rows.max(1));
                Ok(Recovery::Attached(Box::new(CorePane::new(
                    &spec.id,
                    spec,
                    size,
                    self.live.clone(),
                    pane,
                ))))
            }
            Err(_) => Ok(Recovery::DiedWithDaemon),
        }
    }
}
