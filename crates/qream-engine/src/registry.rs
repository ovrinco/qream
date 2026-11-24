use crate::{Error, Result, storage};
use chrono::{DateTime, FixedOffset, Local};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Campaign {
    pub name: String,
    pub root: PathBuf,
    pub state: PathBuf,
    pub session: String,
    pub mode: String,
    /// Absent from the previous tool's registry files.
    #[serde(default)]
    pub title: String,
    pub status: String,
    /// Citium, OVRIN or Zavent when the root does not say it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub division: Option<String>,
    pub created: DateTime<FixedOffset>,
    pub updated: DateTime<FixedOffset>,
}
pub struct Registry {
    pub home: PathBuf,
    changed: tokio::sync::watch::Sender<()>,
}
impl Registry {
    pub fn new(home: PathBuf) -> Self {
        let (changed, _) = tokio::sync::watch::channel(());
        Self { home, changed }
    }
    pub(crate) fn changes(&self) -> tokio::sync::watch::Receiver<()> {
        self.changed.subscribe()
    }
    fn path(&self, name: &str) -> Result<PathBuf> {
        if name.is_empty()
            || name.len() > 64
            || !name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
        {
            return Err(Error::Denied(
                "campaign name needs letters, numbers, dashes or underscores".into(),
            ));
        }
        Ok(self.home.join("campaigns").join(format!("{name}.json")))
    }
    pub fn get(&self, name: &str) -> Result<Campaign> {
        storage::read(&self.path(name)?)
    }
    pub fn up(&self, mut campaign: Campaign) -> Result<Campaign> {
        let path = self.path(&campaign.name)?;
        if path.exists() {
            let old: Campaign = storage::read(&path)?;
            campaign.created = old.created;
            campaign.session = old.session;
        }
        campaign.updated = Local::now().fixed_offset();
        storage::write(&path, &campaign)?;
        self.changed.send_replace(());
        Ok(campaign)
    }
    /// Publish a newly prepared campaign only after its close is on disk.
    /// `up` also persists later lifecycle changes and restores existing records.
    pub fn prepared(&self, campaign: Campaign) -> Result<Campaign> {
        crate::close::require_declared(&campaign.root)?;
        self.up(campaign)
    }
    pub fn down(&self, name: &str) -> Result<Campaign> {
        let mut campaign = self.get(name)?;
        campaign.status = "down".into();
        self.up(campaign)
    }
    pub fn list(&self) -> Result<Vec<Campaign>> {
        let dir = self.home.join("campaigns");
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
            Err(e) => return Err(Error::io(&dir, e)),
        };
        let mut result = vec![];
        for entry in entries {
            let path = entry.map_err(|e| Error::io(&dir, e))?.path();
            if path.extension().is_some_and(|e| e == "json") {
                result.push(storage::read::<Campaign>(&path)?);
            }
        }
        result.sort_by(|a, b| a.created.cmp(&b.created).then_with(|| a.name.cmp(&b.name)));
        Ok(result)
    }
}
