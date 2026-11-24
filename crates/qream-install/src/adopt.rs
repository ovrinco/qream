//! The first install's move-aside: the old tree to `<old>_legacy`, the new tree into the old name, the old
//! command link's target kept in `ROLLBACK.tsv`, and `MOVE_RECORD.md` saying what moved where. Nothing is deleted.

use crate::error::{At, Error, Result};
use crate::files::{self, Prior};
use crate::Install;
use serde::Serialize;
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, Serialize)]
pub struct MoveRecord {
    pub moves: Vec<(PathBuf, PathBuf)>,
    pub old_link: Option<String>,
    pub record: PathBuf,
    pub at: String,
    pub by: String,
}

impl Install {
    /// `old` moves to `legacy`, then `tree` moves to `old`. Refused unless both sources exist and `legacy` does not.
    pub fn adopt(&self, old: &Path, legacy: &Path, tree: &Path) -> Result<MoveRecord> {
        for source in [old, tree] {
            if std::fs::symlink_metadata(source).is_err() {
                return Err(Error::Refused(format!(
                    "{} does not exist; nothing moved",
                    source.display()
                )));
            }
        }
        if std::fs::symlink_metadata(legacy).is_ok() {
            return Err(Error::Refused(format!(
                "{} already exists; nothing moved",
                legacy.display()
            )));
        }
        if old == tree || legacy == tree {
            return Err(Error::Refused(
                "the tree to adopt is one of the paths it replaces".into(),
            ));
        }
        let link = self.bin_dir.join("qream");
        let old_link = match files::prior(&link)? {
            Prior::Link(t) => Some(format!("link:{}", t.display())),
            Prior::File => Some("file".to_owned()),
            Prior::Absent => None,
        };
        let at = crate::now();
        let by = format!(
            "qream-install adopt (user {}, pid {})",
            std::env::var("USER").unwrap_or_else(|_| "unknown".into()),
            std::process::id()
        );
        std::fs::rename(old, legacy).at(old)?;
        if let Err(e) = std::fs::rename(tree, old).at(tree) {
            // Put the old tree back where it was so a failed adopt leaves the disk as it found it.
            let _ = std::fs::rename(legacy, old);
            return Err(e);
        }
        let moves = vec![
            (old.to_owned(), legacy.to_owned()),
            (tree.to_owned(), old.to_owned()),
        ];
        let seq = self.next_seq()?;
        self.append(&[format!(
            "adopt\t{seq}\t{at}\t-\t{}\t{}\t-",
            link.display(),
            old_link.as_deref().unwrap_or("absent")
        )])?;
        let record = self.home.join("MOVE_RECORD.md");
        let mut text = String::new();
        if !record.exists() {
            text.push_str(
                "# QREAM move record\nNothing was deleted; each line says what moved where.\n",
            );
        }
        text.push_str(&format!("\n## {at}, by {by}\n"));
        for (from, to) in &moves {
            text.push_str(&format!(
                "- moved `{}` to `{}`\n",
                from.display(),
                to.display()
            ));
        }
        text.push_str(&format!(
            "- `{}` pointed at `{}` before the first install (kept in ROLLBACK.tsv)\n",
            link.display(),
            old_link.as_deref().unwrap_or("nothing")
        ));
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&record)
            .at(&record)?;
        f.write_all(text.as_bytes()).at(&record)?;
        Ok(MoveRecord {
            moves,
            old_link,
            record,
            at,
            by,
        })
    }
}
