//! The QREAM installer: build, full check, stage, smoke, switch, rollback and the first-install move-aside.
//! Nothing here deletes: what is replaced is re-pointed or moved aside, and every move is written down.

pub mod adopt;
pub mod check;
pub mod daemon;
pub mod error;
pub mod files;
pub mod record;
pub mod run;
pub mod smoke;
pub mod stage;
pub mod switch;

pub use error::{Error, Result};

use std::path::PathBuf;

/// Where the installer reads its tree and writes its releases and links.
#[derive(Debug, Clone)]
pub struct Install {
    /// The workspace to build, check and stage (T01's root).
    pub tree: PathBuf,
    /// The releases home: `releases/<id>`, `current`, `ROLLBACK.tsv`, `CHECK.json`, `kept/`.
    pub home: PathBuf,
    /// Where the command links live (`~/.local/bin`).
    pub bin_dir: PathBuf,
    /// The shared warm target the build line uses.
    pub target_dir: PathBuf,
    /// The build environment script and the build lock of the build line.
    pub build_env: PathBuf,
    pub build_lock: PathBuf,
}

impl Install {
    pub fn releases(&self) -> PathBuf {
        self.home.join("releases")
    }
    pub fn release(&self, id: &str) -> PathBuf {
        self.releases().join(id)
    }
    pub fn current(&self) -> PathBuf {
        self.home.join("current")
    }
    pub fn rollback_file(&self) -> PathBuf {
        self.home.join("ROLLBACK.tsv")
    }
    pub fn kept(&self) -> PathBuf {
        self.home.join("kept")
    }
    pub fn check_file(&self) -> PathBuf {
        self.home.join("CHECK.json")
    }
}

pub fn now() -> String {
    chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, false)
}

/// A release id is one plain path segment.
pub fn valid_id(id: &str) -> Result<()> {
    let ok = !id.is_empty()
        && id != "."
        && id != ".."
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if ok {
        Ok(())
    } else {
        Err(Error::Refused(format!(
            "release id {id:?} is not one plain name"
        )))
    }
}
