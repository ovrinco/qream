//! The fault ledger: a JSON-lines file mirrored to the store's Faults tab. Ids are `QF-<n>`,
//! allocated under an exclusive lock on the file from the highest id in the file and the tab,
//! so two writers never share one. An amendment is a new record that names the original's id
//! and physical line; nothing is rewritten in place.
use super::{json_lines, now};
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use tracker_store::{Fields, Store};

pub const TAB: &str = "Faults";

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Fault {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub seen: String,
    pub area: String,
    pub fault: String,
    pub effect: String,
    pub source: String,
    pub status: String,
    pub fix: String,
    /// The original record this one amends or sights again: `{id, line}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amends: Option<Reference>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Reference {
    pub id: String,
    pub line: usize,
}

/// A record as read back, with the physical line it sits on.
#[derive(Clone, Debug, Serialize)]
pub struct Entry {
    pub line: usize,
    pub record: Value,
}

pub struct Ledger {
    file: PathBuf,
    store: PathBuf,
    author: String,
}

/// The default ledger file, `~/.local/share/qream/faults.jsonl`.
pub fn default_file() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").ok_or_else(|| Error::Denied("HOME is not set".into()))?;
    Ok(PathBuf::from(home).join(".local/share/qream/faults.jsonl"))
}

fn number(id: &str) -> Option<u64> {
    id.strip_prefix("QF-")?.parse().ok()
}

impl Ledger {
    pub fn new(file: &Path, store: &Path, author: &str) -> Self {
        Self {
            file: file.into(),
            store: store.into(),
            author: author.into(),
        }
    }

    /// Records a new fault and returns its id. File and store are written under one lock.
    pub fn add(&self, fault: &Fault) -> Result<String> {
        for (name, v) in [
            ("area", &fault.area),
            ("fault", &fault.fault),
            ("effect", &fault.effect),
            ("source", &fault.source),
            ("status", &fault.status),
        ] {
            if v.trim().is_empty() {
                return Err(Error::Denied(format!("a fault needs {name}")));
            }
        }
        if let Some(parent) = self.file.parent() {
            std::fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .read(true)
            .open(&self.file)
            .map_err(|e| Error::io(&self.file, e))?;
        // Released when `file` drops, after both writes.
        file.lock().map_err(|e| Error::io(&self.file, e))?;
        let store = Store::open(&self.store, &self.author)?;
        let entries = json_lines(&self.file)?;
        let in_file = entries
            .iter()
            .filter_map(|(_, v)| v.get("id").and_then(Value::as_str).and_then(number))
            .max();
        let in_tab = store_ids(&store)?.iter().filter_map(|s| number(s)).max();
        let n = in_file.max(in_tab).unwrap_or(0) + 1;
        let mut record = fault.clone();
        record.id = format!("QF-{n}");
        if record.seen.is_empty() {
            record.seen = now();
        }
        if let Some(r) = &record.amends {
            let found = entries.iter().any(|(line, v)| {
                *line == r.line && v.get("id").and_then(Value::as_str) == Some(&r.id)
            });
            if !found {
                return Err(Error::Denied(format!(
                    "{} is not on line {} of the ledger",
                    r.id, r.line
                )));
            }
        }
        let mut line = serde_json::to_string(&record)?;
        line.push('\n');
        file.write_all(line.as_bytes())
            .map_err(|e| Error::io(&self.file, e))?;
        file.sync_data().map_err(|e| Error::io(&self.file, e))?;
        store.add(TAB, &record.id, &tab_fields(&record))?;
        Ok(record.id)
    }

    /// A new record that changes the status and fix of `id`, referencing its physical line.
    pub fn amend(&self, id: &str, status: &str, fix: &str) -> Result<String> {
        let (line, original) = self
            .list()?
            .into_iter()
            .rev()
            .find(|e| e.record.get("id").and_then(Value::as_str) == Some(id))
            .map(|e| (e.line, e.record))
            .ok_or_else(|| Error::Denied(format!("no fault {id} in the ledger")))?;
        let text = |k: &str| match original.get(k) {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Null) | None => String::new(),
            Some(v) => v.to_string(),
        };
        self.add(&Fault {
            area: text("area"),
            fault: text("fault"),
            effect: text("effect"),
            source: format!("amends {id} at line {line}"),
            status: status.into(),
            fix: fix.into(),
            amends: Some(Reference {
                id: id.into(),
                line,
            }),
            ..Fault::default()
        })
    }

    /// Every record in the file, in file order, with its physical line.
    pub fn list(&self) -> Result<Vec<Entry>> {
        Ok(json_lines(&self.file)?
            .into_iter()
            .map(|(line, record)| Entry { line, record })
            .collect())
    }

    /// The Faults tab as the store holds it.
    pub fn tab(&self) -> Result<Vec<Fields>> {
        let store = Store::open(&self.store, &self.author)?;
        Ok(store.list(Some(TAB), None, None, 1_000_000, 0)?.rows)
    }
}

fn store_ids(store: &Store) -> Result<Vec<String>> {
    Ok(store
        .list(Some(TAB), None, None, 1_000_000, 0)?
        .rows
        .iter()
        .filter_map(|r| r.get("id").and_then(Value::as_str).map(String::from))
        .collect())
}

fn tab_fields(f: &Fault) -> Fields {
    let source = match &f.amends {
        Some(r) if !f.source.contains(&r.id) => {
            format!("amends {} at line {}; {}", r.id, r.line, f.source)
        }
        _ => f.source.clone(),
    };
    let mut m = Fields::new();
    for (k, v) in [
        ("date", &f.seen),
        ("area", &f.area),
        ("fault", &f.fault),
        ("effect", &f.effect),
        ("status", &f.status),
        ("fix", &f.fix),
        ("source", &source),
    ] {
        m.insert(k.into(), json!(v));
    }
    m
}
