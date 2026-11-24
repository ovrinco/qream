use crate::{Error, Result};
use chrono::{DateTime, FixedOffset};
use serde::{Deserialize, Serialize};
use std::{
    io::{BufRead, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Transition {
    pub at: DateTime<FixedOffset>,
    pub campaign: String,
    pub run: String,
    /// Empty for a campaign-level transition (the controller writes `null`).
    #[serde(default, deserialize_with = "null_empty")]
    pub task: String,
    pub from: String,
    pub to: String,
    pub by: String,
    #[serde(default)]
    pub evidence: serde_json::Value,
}
fn null_empty<'d, D: serde::Deserializer<'d>>(d: D) -> std::result::Result<String, D::Error> {
    Ok(Option::<String>::deserialize(d)?.unwrap_or_default())
}
pub struct Ledger {
    journal: Journal,
}
impl Ledger {
    pub fn new(root: &Path) -> Self {
        Self {
            journal: Journal::new(root),
        }
    }
    pub fn append(&self, transition: Transition) -> Result<()> {
        self.journal.append(&transition).map(|_| ())
    }
    pub fn read(&self) -> Result<Vec<Transition>> {
        self.journal
            .read()?
            .into_iter()
            .map(|value| Ok(serde_json::from_value(value)?))
            .collect()
    }
}

/// The sole append implementation for a campaign's control record. Each call opens its
/// own descriptor: the OS lock serializes both threads and independent processes.
pub struct Journal {
    path: PathBuf,
}
impl Journal {
    pub fn new(root: &Path) -> Self {
        Self {
            path: root.join("state/control.jsonl"),
        }
    }

    /// Create an empty journal without truncating any existing history.
    pub fn initialize(&self) -> Result<()> {
        let parent = self.path.parent().expect("journal state parent");
        std::fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .read(true)
            .open(&self.path)
            .map_err(|e| Error::io(&self.path, e))?;
        file.lock()
            .and_then(|()| file.sync_data())
            .map_err(|e| Error::io(&self.path, e))
    }

    /// Allocate under the lock, then acknowledge only after the complete line is durable.
    pub fn append(&self, transition: &impl Serialize) -> Result<u64> {
        let mut value = serde_json::to_value(transition)?;
        let object = value
            .as_object_mut()
            .ok_or_else(|| Error::Denied("a journal entry must be an object".into()))?;
        let parent = self.path.parent().expect("journal state parent");
        std::fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .read(true)
            .open(&self.path)
            .map_err(|e| Error::io(&self.path, e))?;
        // No cached sequence: another process may have appended since our last call.
        file.lock().map_err(|e| Error::io(&self.path, e))?;
        let seq = self
            .last_sequence(&mut file)?
            .checked_add(1)
            .ok_or_else(|| Error::Denied("journal sequence exhausted".into()))?;
        object.insert("seq".into(), seq.into());
        let mut line = serde_json::to_vec(&value)?;
        line.push(b'\n');
        file.write_all(&line)
            .and_then(|()| file.sync_data())
            .map_err(|e| Error::io(&self.path, e))?;
        Ok(seq)
    }

    /// Values preserve writer-owned fields without changing legacy reader structs.
    pub fn read(&self) -> Result<Vec<serde_json::Value>> {
        read_values(&self.path)
    }

    fn last_sequence(&self, file: &mut std::fs::File) -> Result<u64> {
        let result = (|| -> std::io::Result<Vec<u8>> {
            let end = file.seek(SeekFrom::End(0))?;
            if end == 0 {
                return Ok(Vec::new());
            }
            let mut position = end;
            let mut tail = Vec::new();
            // Read only the last line, even when a controller snapshot spans blocks.
            while position > 0 {
                let size = position.min(4096);
                position -= size;
                file.seek(SeekFrom::Start(position))?;
                let mut block = vec![0; size as usize];
                file.read_exact(&mut block)?;
                block.extend_from_slice(&tail);
                tail = block;
                if tail[..tail.len().saturating_sub(1)].contains(&b'\n') {
                    break;
                }
            }
            Ok(tail)
        })()
        .map_err(|e| Error::io(&self.path, e))?;
        if result.is_empty() {
            return Ok(0);
        }
        if result.last() != Some(&b'\n') {
            return Err(Error::Denied(
                "control journal has an incomplete final line".into(),
            ));
        }
        let body = &result[..result.len() - 1];
        let start = body.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
        let value: serde_json::Value = serde_json::from_slice(&body[start..])?;
        if let Some(seq) = value.get("seq") {
            return seq
                .as_u64()
                .ok_or_else(|| Error::Denied("invalid journal sequence".into()));
        }
        // Legacy files have no sequence. Count physical lines once on migration.
        file.rewind().map_err(|e| Error::io(&self.path, e))?;
        let mut count = 0u64;
        for line in std::io::BufReader::new(file).lines() {
            let line = line.map_err(|e| Error::io(&self.path, e))?;
            let _: serde_json::Value = serde_json::from_str(&line)?;
            count = count
                .checked_add(1)
                .ok_or_else(|| Error::Denied("journal sequence exhausted".into()))?;
        }
        Ok(count)
    }
}

fn read_values(path: &Path) -> Result<Vec<serde_json::Value>> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(e) => return Err(Error::io(path, e)),
    };
    file.lock_shared().map_err(|e| Error::io(path, e))?;
    std::io::BufReader::new(file)
        .lines()
        .map(|line| {
            Ok(serde_json::from_str(
                &line.map_err(|e| Error::io(path, e))?,
            )?)
        })
        .collect()
}

/// A single read of the ledger, scoped to a campaign and run before derivation.
pub struct LedgerSnapshot {
    latest: std::collections::BTreeMap<String, crate::states::TaskWord>,
}
impl LedgerSnapshot {
    pub fn read(ledger: &Ledger, campaign: &str, run: &str) -> Result<Self> {
        let mut latest = std::collections::BTreeMap::new();
        for transition in ledger.read()? {
            if transition.campaign == campaign
                && transition.run == run
                && !transition.task.is_empty()
            {
                let word = match transition.to.as_str() {
                    "accepted" => Some(crate::states::TaskWord::Accepted),
                    "stopped" | "cancelled" | "archived" => Some(crate::states::TaskWord::Stopped),
                    "gap_fill" => Some(crate::states::TaskWord::GapFill),
                    _ => None,
                };
                if let Some(word) = word {
                    latest.insert(transition.task, word);
                } else {
                    latest.remove(&transition.task);
                }
            }
        }
        Ok(Self { latest })
    }
}
impl crate::states::LedgerSource for LedgerSnapshot {
    fn transition(&self, task: &str) -> Option<crate::states::TaskWord> {
        self.latest.get(task).cloned()
    }
}

/// Every task's latest ledger word across all runs of one campaign, with the first and
/// last transition times. Reads the contract's `state/control.jsonl` and, for campaigns
/// run by the previous tool, its `state/control.sqlite` event table, opened immutable.
pub struct CampaignLedger {
    /// Each task's last `to` word as written, whatever it maps to.
    pub raw: std::collections::BTreeMap<String, String>,
    /// Each task's last transition evidence, in words.
    pub evidence: std::collections::BTreeMap<String, String>,
    pub latest: std::collections::BTreeMap<String, crate::states::TaskWord>,
    pub first: Option<DateTime<FixedOffset>>,
    pub last: Option<DateTime<FixedOffset>>,
    pub transitions: usize,
    /// Each task's first transition: when its work began.
    pub started: std::collections::BTreeMap<String, DateTime<FixedOffset>>,
    pub accepted_at: std::collections::BTreeMap<String, DateTime<FixedOffset>>,
    pub gap_fill: std::collections::BTreeMap<String, DateTime<FixedOffset>>,
    /// Tasks whose latest word is cancelled or archived: a closed run's, never current.
    pub cancelled: std::collections::BTreeSet<String>,
    /// Settled scoped holds, supplied by the observation adapter.
    pub held: std::collections::BTreeSet<String>,
}
impl CampaignLedger {
    pub fn read(root: &Path, campaign: &str) -> Result<Self> {
        let mut ledger = Self {
            raw: std::collections::BTreeMap::new(),
            evidence: std::collections::BTreeMap::new(),
            latest: std::collections::BTreeMap::new(),
            first: None,
            last: None,
            transitions: 0,
            started: Default::default(),
            accepted_at: Default::default(),
            gap_fill: Default::default(),
            cancelled: Default::default(),
            held: Default::default(),
        };
        let legacy = root.join("state/control.sqlite");
        if legacy.is_file() {
            ledger.read_legacy(&legacy, campaign)?;
        }
        for transition in Ledger::new(root).read()? {
            if transition.campaign == campaign {
                if transition.task.is_empty() {
                    ledger.touch(transition.at);
                    continue;
                }
                let at = transition.at;
                ledger.started.entry(transition.task.clone()).or_insert(at);
                match transition.to.as_str() {
                    "accepted" => {
                        ledger.accepted_at.insert(transition.task.clone(), at);
                    }
                    "gap_fill" => {
                        ledger.gap_fill.insert(transition.task.clone(), at);
                    }
                    _ => {}
                }
                if matches!(transition.to.as_str(), "cancelled" | "archived") {
                    ledger.cancelled.insert(transition.task.clone());
                } else {
                    ledger.cancelled.remove(&transition.task);
                }
                let word = word(&transition.to);
                ledger
                    .raw
                    .insert(transition.task.clone(), transition.to.clone());
                let evidence = match &transition.evidence {
                    serde_json::Value::String(text) => text.clone(),
                    serde_json::Value::Null => String::new(),
                    other => other.to_string(),
                };
                ledger.evidence.insert(transition.task.clone(), evidence);
                ledger.note(transition.task, word, Some(transition.at), true);
            }
        }
        Ok(ledger)
    }
    /// A campaign-level transition counts for the campaign's first and last times only.
    fn touch(&mut self, at: DateTime<FixedOffset>) {
        self.transitions += 1;
        self.first = Some(self.first.map_or(at, |f| f.min(at)));
        self.last = Some(self.last.map_or(at, |l| l.max(at)));
    }
    fn note(
        &mut self,
        task: String,
        word: Option<crate::states::TaskWord>,
        at: Option<DateTime<FixedOffset>>,
        clears: bool,
    ) {
        self.transitions += 1;
        if let Some(at) = at {
            self.first = Some(self.first.map_or(at, |f| f.min(at)));
            self.last = Some(self.last.map_or(at, |l| l.max(at)));
        }
        match word {
            Some(word) => {
                self.latest.insert(task, word);
            }
            None if clears => {
                self.latest.remove(&task);
            }
            None => {}
        }
    }
    fn read_legacy(&mut self, path: &Path, campaign: &str) -> Result<()> {
        use rusqlite::{Connection, OpenFlags};
        let uri = format!("file:{}?mode=ro&immutable=1", path.display());
        let db = Connection::open_with_flags(
            uri,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
        )?;
        let mut statement = db.prepare(
            "SELECT e.task, e.kind, e.at FROM swarm_events e JOIN swarm_runs r ON r.id = e.run \
             WHERE r.campaign = ?1 AND e.task IS NOT NULL ORDER BY e.seq",
        )?;
        let rows = statement.query_map([campaign], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
            ))
        })?;
        for row in rows {
            let (task, kind, at) = row?;
            // The previous tool's per-task acceptance and stop events; its other events
            // (dispatch, prompts, retirement) never change a ledger word.
            let word = match kind.as_str() {
                "operator_accepted" => Some(crate::states::TaskWord::Accepted),
                "task_stopped" | "task_cancelled" => Some(crate::states::TaskWord::Stopped),
                _ => None,
            };
            let at = at.as_deref().and_then(legacy_time);
            self.note(task, word, at, false);
        }
        Ok(())
    }
}
impl crate::states::LedgerSource for CampaignLedger {
    fn transition(&self, task: &str) -> Option<crate::states::TaskWord> {
        self.latest.get(task).cloned()
    }
    fn gap_fill_at(&self, task: &str) -> Option<DateTime<FixedOffset>> {
        self.gap_fill.get(task).copied()
    }
    fn stopped_when_gone(&self, task: &str) -> bool {
        self.held.contains(task)
            || self
                .raw
                .get(task)
                .is_some_and(|word| matches!(word.as_str(), "interrupted" | "paused"))
    }
    fn submission_blocker(&self, task: &str) -> Option<crate::states::Blocker> {
        if self
            .raw
            .get(task)
            .is_some_and(|word| word == "awaiting_review")
        {
            return None;
        }
        let refusal = self
            .raw
            .get(task)
            .is_some_and(|word| matches!(word.as_str(), "submit_refused" | "submission_refused"));
        let reason = if refusal {
            self.evidence.get(task).cloned().filter(|s| !s.is_empty())
        } else {
            None
        };
        Some(crate::states::Blocker {
            kind: crate::states::BlockKind::E9,
            reason: reason.unwrap_or_else(|| "The seal has no recorded submission".into()),
        })
    }
}
fn word(to: &str) -> Option<crate::states::TaskWord> {
    match to {
        "accepted" => Some(crate::states::TaskWord::Accepted),
        "stopped" | "cancelled" | "archived" => Some(crate::states::TaskWord::Stopped),
        "gap_fill" => Some(crate::states::TaskWord::GapFill),
        _ => None,
    }
}
/// SQLite's CURRENT_TIMESTAMP: UTC without an offset.
fn legacy_time(text: &str) -> Option<DateTime<FixedOffset>> {
    chrono::NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M:%S")
        .ok()
        .map(|t| t.and_utc().fixed_offset())
}
