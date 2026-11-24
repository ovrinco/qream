use crate::{Result, agent::AgentSpec};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf};
use tokio::sync::broadcast;
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Screen {
    pub scrollback: Vec<String>,
    /// Full core-owned screen snapshot, including modes/cells for restoration.
    pub state: serde_json::Value,
    pub lines: Vec<String>,
    pub cols: u16,
    pub rows: u16,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Split {
    pub of: String,
    pub dir: Direction,
    pub ratio: f32,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Horizontal,
    Vertical,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PaneSpec {
    pub id: String,
    pub workspace: String,
    pub cwd: PathBuf,
    pub env: BTreeMap<String, String>,
    pub split: Option<Split>,
}
/// Implementations own only the processes they spawn. close must never kill foreign PIDs.
/// child_pids excludes the harness itself and includes only live running tools.
pub trait PaneHandle: Send {
    fn spawn(&mut self, spec: &AgentSpec) -> Result<u32>;
    /// Resume the durable native conversation using frozen options. The default refuses
    /// rather than silently opening a replacement conversation or answering a trust dialog.
    fn resume(&mut self, _record: &crate::agent::LaunchRecord) -> Result<u32> {
        Err(crate::Error::Lost(
            "native conversation resume is not implemented by this core".into(),
        ))
    }
    fn write(&mut self, bytes: &[u8]) -> Result<()>;
    /// Enqueue one atomic text/pause/submit delivery. Real PTYs override this;
    /// the default preserves the byte contract of synchronous fake handles.
    fn enqueue_prompt(&mut self, text: &str) -> Result<()> {
        self.write(text.as_bytes())?;
        self.write(b"\r")
    }
    fn resize(&mut self, cols: u16, rows: u16) -> Result<()>;
    fn subscribe(&self) -> broadcast::Receiver<Screen>;
    fn text(&self, lines: usize) -> Result<Screen>;
    fn child_pids(&self) -> Result<Vec<u32>>;
    fn exited(&self) -> Result<bool>;
    fn close(&mut self) -> Result<()>;
}
/// Identity/ownership must be checked by the core, not guessed from a reused PID.
pub enum Recovery {
    Attached(Box<dyn PaneHandle>),
    DiedWithDaemon,
    Lost(String),
}
pub trait PaneFactory: Send {
    fn create(&mut self, spec: &PaneSpec, screen: &Screen) -> Result<Box<dyn PaneHandle>>;
    fn recover(&mut self, spec: &PaneSpec, screen: &Screen) -> Result<Recovery>;
}
