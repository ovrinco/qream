use crate::{Error, Result, handle::PaneHandle};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf};
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentKind {
    Claude,
    Codex,
    Opencode,
    /// A plain program in a pane (probes and scripted tasks); its argv names the program.
    Shell,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSpec {
    pub kind: AgentKind,
    pub argv: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub prompt_file: Option<PathBuf>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentState {
    Working,
    Idle,
    Blocked,
    Done,
    Unknown,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockedKind {
    Question,
    Approval,
    Quota,
    Account,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evidence {
    pub rule: String,
    pub text: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentInfo {
    pub state: AgentState,
    pub blocked_kind: Option<BlockedKind>,
    pub interactive_ready: bool,
    pub evidence: Evidence,
    pub pid: Option<u32>,
    pub started_at: String,
    pub observed_at: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchRecord {
    pub spec: AgentSpec,
    pub pid: Option<u32>,
    pub started_at: String,
    /// Durable native conversation identity is independent of pane names.
    pub conversation_id: Option<String>,
    pub pending_prompt: Option<String>,
}
/// The pause between a prompt's text and its submit key.
pub const SUBMIT_PAUSE: std::time::Duration = std::time::Duration::from_millis(300);
/// The first prompt names the directive file instead of typing it: a typed body larger than the pane's
/// input buffer blocked the write while the reader waited on the same lock (QF-2026-10-11-REPAIR-05).
pub fn directive_prompt(path: &std::path::Path) -> std::io::Result<String> {
    std::fs::metadata(path)?;
    Ok(format!(
        "Your directive is the file {}. Read the whole file now, then carry it out exactly as written; it is your complete instruction for this task.",
        path.display()
    ))
}
pub fn prompt(handle: &mut dyn PaneHandle, info: &AgentInfo, text: &str) -> Result<()> {
    if info.state != AgentState::Idle || !info.interactive_ready {
        return Err(Error::Busy);
    }
    // Literal writes, never shell evaluation or an approval response. The submit key goes on its
    // own after a pause: a harness reads one burst as a paste and keeps its Enter as a newline.
    handle.enqueue_prompt(text)
}
