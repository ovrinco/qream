//! Rows from the tracker store crate, linked in-process: no tool is spawned per row.
//! Mutations go through an explicit assignment sink, logged by the store with its author.
use crate::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeMap, path::PathBuf};
use tracker_store::Store;
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Row {
    pub id: String,
    #[serde(default)]
    pub purpose: String,
    #[serde(default)]
    pub intent: String,
    #[serde(default)]
    pub dod: String,
    #[serde(default)]
    pub exemplar: String,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub outcome_id: String,
}
impl Row {
    pub fn from_value(v: &Value) -> Self {
        let field = |k: &str| v[k].as_str().unwrap_or("").to_owned();
        Self {
            id: field("id"),
            purpose: field("purpose"),
            intent: field("intent"),
            dod: field("dod"),
            exemplar: field("exemplar"),
            source: field("source"),
            status: field("status"),
            outcome_id: field("outcome_id"),
        }
    }
    pub fn fields(&self) -> [(&str, &str); 5] {
        [
            ("Purpose", &self.purpose),
            ("Intent", &self.intent),
            ("Definition of done", &self.dod),
            ("Exemplar", &self.exemplar),
            ("Source", &self.source),
        ]
    }
    pub fn thin(&self) -> bool {
        norm(&self.purpose).chars().count() < 40 || norm(&self.dod).is_empty()
    }
}
pub fn norm(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Group {
    pub id: String,
    pub outcome: String,
    pub standing: String,
    pub proof: String,
}
pub trait RowSource {
    fn get(&self, id: &str) -> Result<Option<Row>>;
    fn members(&self, group: &str) -> Result<Vec<String>>;
    fn group(&self, id: &str) -> Result<Option<Group>>;
}
#[derive(Default)]
pub struct MemoryRows {
    pub rows: BTreeMap<String, Row>,
    pub groups: BTreeMap<String, Group>,
}
impl RowSource for MemoryRows {
    fn get(&self, id: &str) -> Result<Option<Row>> {
        Ok(self.rows.get(id).cloned())
    }
    fn members(&self, group: &str) -> Result<Vec<String>> {
        Ok(self
            .rows
            .values()
            .filter(|r| r.outcome_id == group)
            .map(|r| r.id.clone())
            .collect())
    }
    fn group(&self, id: &str) -> Result<Option<Group>> {
        Ok(self.groups.get(id).cloned())
    }
}
impl RowSource for Store {
    fn get(&self, id: &str) -> Result<Option<Row>> {
        if !self.exists(id)? {
            return Ok(None);
        }
        let row = Store::get(self, id)?;
        Ok(Some(Row::from_value(&Value::Object(row))))
    }
    fn members(&self, group: &str) -> Result<Vec<String>> {
        Ok(Store::members(self, group)?)
    }
    fn group(&self, id: &str) -> Result<Option<Group>> {
        if !self.exists(id)? {
            return Ok(None);
        }
        let Ok(row) = Store::group(self, id) else {
            return Ok(None);
        };
        let s = |k: &str| row.get(k).and_then(Value::as_str).unwrap_or("").to_owned();
        Ok(Some(Group {
            id: id.into(),
            outcome: s("outcome"),
            standing: s("standing"),
            proof: s("acceptance_proof"),
        }))
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Assignment {
    pub row: String,
    pub campaign: String,
    pub task: String,
    pub wave: u32,
    pub directive: PathBuf,
    pub part: bool,
}
pub trait AssignmentSink {
    fn assign(&self, a: &Assignment) -> Result<()>;
}
/// Assignments are written by the store crate, one logged event each, under the
/// store's author; the daemon opens the store it was given (a copy in tests).
impl AssignmentSink for Store {
    fn assign(&self, a: &Assignment) -> Result<()> {
        Store::assign(
            self,
            &a.row,
            &tracker_store::Assignment {
                seq: 0,
                campaign: a.campaign.clone(),
                wave: i64::from(a.wave),
                task: a.task.clone(),
                directive: a.directive.display().to_string(),
                row_id: a.row.clone(),
                part: i64::from(a.part),
                state: "planned".into(),
                at: String::new(),
                author: String::new(),
            },
        )?;
        Ok(())
    }
}
