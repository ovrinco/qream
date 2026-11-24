//! Reads a campaign's control ledger, `state/control.jsonl`, into its timeline.
use super::json_lines;
use crate::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Transition {
    pub at: String,
    #[serde(default)]
    pub campaign: String,
    #[serde(default)]
    pub run: String,
    #[serde(default)]
    pub task: String,
    #[serde(default)]
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub by: String,
    #[serde(default)]
    pub evidence: serde_json::Value,
    /// Physical line in the ledger, so a reader can cite it.
    #[serde(default)]
    pub line: usize,
}

/// The campaign's transitions in time order (ledger order among equal times), optionally one task's.
pub fn timeline(root: &Path, task: Option<&str>) -> Result<Vec<Transition>> {
    let mut out = Vec::new();
    for (line, v) in json_lines(&root.join("state/control.jsonl"))? {
        let mut t: Transition = serde_json::from_value(v)?;
        t.line = line;
        if task.is_none_or(|k| t.task == k) {
            out.push(t);
        }
    }
    out.sort_by(|a, b| {
        let key = |t: &Transition| chrono::DateTime::parse_from_rfc3339(&t.at).ok();
        key(a).cmp(&key(b))
    });
    Ok(out)
}
