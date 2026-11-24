//! The process ledger: the four closing lines of every report of a campaign, gathered into
//! `state/process.jsonl` so the next campaign's directives and the skill can read them.
use crate::{Error, Result};
use serde::Serialize;
use std::path::Path;

pub const LINES: [&str; 4] = [
    "Surprise:",
    "Friction:",
    "Directive fix:",
    "Look hardest at:",
];

#[derive(Clone, Debug, Serialize)]
pub struct Entry {
    pub report: String,
    pub kind: String,
    pub text: String,
    pub source: String,
}

/// Gathers the closing lines of `reports/*.md` and rewrites `state/process.jsonl` from them.
pub fn gather(root: &Path) -> Result<Vec<Entry>> {
    let dir = root.join("reports");
    let mut names: Vec<_> = match std::fs::read_dir(&dir) {
        Ok(d) => d
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "md"))
            .collect(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(Error::io(&dir, e)),
    };
    names.sort();
    let mut out = Vec::new();
    for path in names {
        let text = std::fs::read_to_string(&path).map_err(|e| Error::io(&path, e))?;
        let report = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        for (i, line) in text.lines().enumerate() {
            let line = line.trim_start_matches(['-', '*', ' ']);
            if let Some(kind) = LINES.iter().find(|k| line.starts_with(**k)) {
                out.push(Entry {
                    report: report.clone(),
                    kind: kind.trim_end_matches(':').into(),
                    text: line[kind.len()..].trim().into(),
                    source: format!("reports/{report}:{}", i + 1),
                });
            }
        }
    }
    let state = root.join("state");
    std::fs::create_dir_all(&state).map_err(|e| Error::io(&state, e))?;
    let mut body = String::new();
    for e in &out {
        body.push_str(&serde_json::to_string(e)?);
        body.push('\n');
    }
    let file = state.join("process.jsonl");
    std::fs::write(&file, body).map_err(|e| Error::io(&file, e))?;
    Ok(out)
}
