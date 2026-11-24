//! Every build, check, stage and smoke writes a duration row with its outcome through the store crate.

use crate::Result;
use std::path::Path;
use tracker_store::{Duration, Store};

pub struct Timed {
    pub kind: &'static str,
    pub verb: &'static str,
    pub started_at: String,
    pub begun: std::time::Instant,
}

pub fn start(kind: &'static str, verb: &'static str) -> Timed {
    Timed {
        kind,
        verb,
        started_at: crate::now(),
        begun: std::time::Instant::now(),
    }
}

/// Write the row; `kind` is one of the store's build|test|check|install.
pub fn finish(
    store: &Path,
    campaign: &str,
    task: &str,
    timed: Timed,
    outcome: &str,
    note: &str,
) -> Result<f64> {
    let seconds = timed.begun.elapsed().as_secs_f64();
    Store::open(store, "qream-install")?.duration(&Duration {
        campaign: campaign.into(),
        task: task.into(),
        kind: timed.kind.into(),
        started_at: timed.started_at,
        ended_at: crate::now(),
        seconds,
        outcome: outcome.into(),
        note: format!("qream-install {}: {note}", timed.verb),
    })?;
    Ok(seconds)
}
