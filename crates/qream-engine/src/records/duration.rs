//! Job and accepted-task durations, with their outcome, in the store's `duration` table.
use crate::{Error, Result};
use chrono::{DateTime, Local, SecondsFormat};
use serde_json::Value;
use tracker_store::{Duration, Store};

pub const KINDS: [&str; 5] = ["build", "test", "check", "install", "task"];

/// One measured job.
pub struct Span<'a> {
    pub campaign: &'a str,
    pub task: &'a str,
    pub kind: &'a str,
    pub started: DateTime<Local>,
    pub ended: DateTime<Local>,
    pub outcome: &'a str,
    pub note: &'a str,
}

/// Records one duration measured between `started` and `ended`.
pub fn record(store: &Store, s: &Span) -> Result<()> {
    let Span {
        campaign,
        task,
        kind,
        started,
        ended,
        outcome,
        note,
    } = *s;
    if !KINDS.contains(&kind) {
        return Err(Error::Denied(format!(
            "a duration is one of {}",
            KINDS.join(", ")
        )));
    }
    let seconds = (ended - started).num_milliseconds() as f64 / 1000.0;
    store.duration(&Duration {
        campaign: campaign.into(),
        task: task.into(),
        kind: kind.into(),
        started_at: started.to_rfc3339_opts(SecondsFormat::Millis, false),
        ended_at: ended.to_rfc3339_opts(SecondsFormat::Millis, false),
        seconds,
        outcome: outcome.into(),
        note: note.into(),
    })?;
    Ok(())
}

/// Runs `job`, then records its duration with `ok` or `failed`; returns the job's result.
pub fn timed<T, E>(
    store: &Store,
    campaign: &str,
    task: &str,
    kind: &str,
    job: impl FnOnce() -> std::result::Result<T, E>,
) -> Result<std::result::Result<T, E>> {
    let started = Local::now();
    let out = job();
    let outcome = if out.is_ok() { "ok" } else { "failed" };
    record(
        store,
        &Span {
            campaign,
            task,
            kind,
            started,
            ended: Local::now(),
            outcome,
            note: "",
        },
    )?;
    Ok(out)
}

/// The durations of a campaign, oldest first, as the store holds them.
pub fn list(store: &Store, campaign: &str) -> Result<Vec<Value>> {
    let (cols, rows) = store.query(
        "SELECT id,campaign,task,kind,started_at,ended_at,seconds,outcome,note FROM duration ORDER BY id",
    )?;
    Ok(rows
        .into_iter()
        .map(|r| Value::Object(cols.iter().cloned().zip(r).collect()))
        .filter(|v| v["campaign"] == campaign)
        .collect())
}

/// Persist each explicit acceptance once, measured from the task's first transition in that run.
pub fn accepted(store: &Store, root: &std::path::Path, campaign: &str, task: &str) -> Result<()> {
    let transitions = crate::ledger::Ledger::new(root).read()?;
    let acceptance = transitions
        .iter()
        .rev()
        .find(|t| t.campaign == campaign && t.task == task && t.to == "accepted")
        .ok_or_else(|| Error::Denied(format!("{task} has no recorded acceptance")))?;
    let started = transitions
        .iter()
        .filter(|t| {
            t.campaign == campaign
                && t.task == task
                && t.run == acceptance.run
                && t.at <= acceptance.at
        })
        .map(|t| t.at)
        .min()
        .unwrap_or(acceptance.at);
    let note = format!(
        "acceptance:{}:{}",
        acceptance.run,
        acceptance.at.to_rfc3339()
    );
    if list(store, campaign)?
        .iter()
        .any(|r| r["task"] == task && r["kind"] == "task" && r["note"] == note)
    {
        return Ok(());
    }
    record(
        store,
        &Span {
            campaign,
            task,
            kind: "task",
            started: started.with_timezone(&Local),
            ended: acceptance.at.with_timezone(&Local),
            outcome: "accepted",
            note: &note,
        },
    )
}
