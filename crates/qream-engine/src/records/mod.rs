//! QREAM's own records: faults, usage, durations, the control ledger, replay and the process ledger.
pub mod duration;
pub mod fault;
pub mod ledger;
pub mod process;
pub mod replay;
pub mod usage;

use crate::{Error, Result};
use std::io::{BufRead, BufReader};
use std::path::Path;

/// Each non-empty line of a JSON-lines file with its 1-based physical line number.
pub(crate) fn json_lines(path: &Path) -> Result<Vec<(usize, serde_json::Value)>> {
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(Error::io(path, e)),
    };
    let mut out = Vec::new();
    for (i, line) in BufReader::new(file).lines().enumerate() {
        let line = line.map_err(|e| Error::io(path, e))?;
        if line.trim().is_empty() {
            continue;
        }
        let value = serde_json::from_str(&line).map_err(|source| Error::Parse {
            path: path.into(),
            line: i + 1,
            message: source.to_string(),
        })?;
        out.push((i + 1, value));
    }
    Ok(out)
}

pub(crate) fn now() -> String {
    chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, false)
}
