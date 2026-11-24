//! Stage a release: the binaries, `ui/`, the tree's own release record, the check it passed and `SHA256SUMS`.

use crate::check::CheckRecord;
use crate::error::{At, Error, Result};
use crate::{files, run, Install};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// The binaries a release carries; the first three are one package and must report one build.
pub const SAME_BUILD: &[&str] = &["qream", "qreamd", "qream-install"];
/// Shipped beside the daemon, which starts every named job through it (QF-2026-10-11-ROUND-01).
pub const HELPERS: &[&str] = &["qream-job-monitor"];
pub const OPTIONAL: &[&str] = &["tracker"];

#[derive(Debug, Serialize, Deserialize)]
pub struct StageRecord {
    pub release_id: String,
    pub tree: PathBuf,
    pub tree_sha256: String,
    pub build: String,
    pub source_commit: String,
    pub binaries: Vec<String>,
    pub check: CheckRecord,
    pub at: String,
}

impl Install {
    /// The folder the build line leaves the binaries in.
    pub fn built(&self) -> PathBuf {
        self.target_dir.join("debug")
    }

    pub fn stage(&self, id: &str) -> Result<StageRecord> {
        crate::valid_id(id)?;
        let source_commit = clean_commit(&self.tree)?;
        let check: CheckRecord =
            serde_json::from_slice(&std::fs::read(self.check_file()).map_err(|_| {
                Error::Refused("no full check on record; run `qream-install check` first".into())
            })?)?;
        let tree_sha256 = files::tree_hash(&self.tree)?;
        if !check.ok || check.tree_sha256 != tree_sha256 {
            return Err(Error::Refused(format!(
                "the full check on record is {} for tree {}, the tree is now {}",
                if check.ok { "green" } else { "red" },
                check.tree_sha256,
                tree_sha256
            )));
        }
        let dir = self.release(id);
        if dir.exists() {
            return Err(Error::Refused(format!(
                "release {id} already exists; a release is never overwritten"
            )));
        }
        for name in ["docs/PATTERN_MINE_herdr.md", "NOTICES.md"] {
            if !self.tree.join(name).is_file() {
                return Err(Error::Refused(format!("source tree lacks {name}")));
            }
        }
        let bin = dir.join("bin");
        std::fs::create_dir_all(&bin).at(&bin)?;
        let mut binaries = Vec::new();
        for name in SAME_BUILD.iter().chain(HELPERS).chain(OPTIONAL) {
            let from = self.built().join(name);
            if !from.is_file() {
                if !OPTIONAL.contains(name) {
                    return Err(Error::Refused(format!("{} was not built", from.display())));
                }
                continue;
            }
            std::fs::copy(&from, bin.join(name)).at(&from)?;
            binaries.push((*name).to_owned());
        }
        let ui = self.tree.join("ui");
        if ui.is_dir() {
            files::copy_dir(&ui, &dir.join("ui"))?;
        }
        for (from, to) in [
            ("docs/PATTERN_MINE_herdr.md", "PATTERN_MINE_herdr.md"),
            ("NOTICES.md", "NOTICES.md"),
        ] {
            let dest = dir.join(to);
            std::fs::copy(self.tree.join(from), &dest).at(&dest)?;
        }
        let deadline = Instant::now() + Duration::from_secs(60);
        let mut build = None;
        for name in SAME_BUILD {
            let argv = vec![
                bin.join(name).to_string_lossy().into_owned(),
                "--version".into(),
            ];
            let out = run::one(&format!("{name} --version"), &argv, &dir, deadline, 60)?;
            let text = String::from_utf8_lossy(&out.stdout).trim().to_owned();
            match &build {
                None => build = Some(text),
                Some(first) if *first != text => {
                    return Err(Error::Refused(format!(
                        "{name} reports build {text}, qream reports {first}"
                    )))
                }
                Some(_) => {}
            }
        }
        // The tree's own release-record module writes the record (tree and lock hashes, toolchain, commands).
        let mut argv = vec![
            bin.join("qream-install").to_string_lossy().into_owned(),
            "release-record".into(),
            "--source".into(),
            self.tree.to_string_lossy().into_owned(),
            "--out".into(),
            dir.to_string_lossy().into_owned(),
            "--release-id".into(),
            id.into(),
        ];
        for c in self.commands() {
            argv.push("--command".into());
            argv.push(c);
        }
        run::one("release-record", &argv, &dir, deadline, 60)?;
        let record_path = dir.join("release_record.json");
        let mut record: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&record_path).at(&record_path)?)?;
        for key in [
            "tree_sha256",
            "lock_sha256",
            "toolchain",
            "commands",
            "manifest",
        ] {
            if record.get(key).is_none() {
                return Err(Error::Refused(format!("release_record.json lacks {key}")));
            }
        }
        if clean_commit(&self.tree)? != source_commit
            || files::tree_hash(&self.tree)? != tree_sha256
        {
            return Err(Error::Refused("source changed during staging".into()));
        }
        record["source_commit"] = source_commit.clone().into();
        record["source_clean"] = true.into();
        std::fs::write(&record_path, serde_json::to_vec_pretty(&record)?).at(&record_path)?;
        let stage = StageRecord {
            release_id: id.into(),
            tree: self.tree.clone(),
            tree_sha256,
            build: build.unwrap_or_default(),
            source_commit,
            binaries,
            check,
            at: crate::now(),
        };
        let path = dir.join("STAGE.json");
        std::fs::write(&path, serde_json::to_vec_pretty(&stage)?).at(&path)?;
        let sums = dir.join("SHA256SUMS");
        std::fs::write(&sums, files::sums(&dir)?).at(&sums)?;
        Ok(stage)
    }

    /// The commands that made the release, as the record lists them.
    pub fn commands(&self) -> Vec<String> {
        [
            "qream-install build: cargo +1.94.0 build --workspace --offline --locked -j4",
            "qream-install check: cargo fmt --all -- --check; cargo clippy --workspace --all-targets -- -D warnings; cargo test --workspace --no-run then every test binary at once; qmllint ui/",
            "qream-install stage",
        ]
        .iter()
        .map(|s| (*s).to_owned())
        .collect()
    }
}

/// Refuse dirty or non-Git sources before creating a release, retaining Git's file names.
pub fn clean_commit(tree: &std::path::Path) -> Result<String> {
    let deadline = Instant::now() + Duration::from_secs(10);
    let git = |args: &[&str]| {
        let mut argv = vec!["git".to_owned()];
        argv.extend(args.iter().map(|s| (*s).to_owned()));
        run::one("source Git check", &argv, tree, deadline, 10)
    };
    let status = git(&["status", "--porcelain", "--untracked-files=all"])?;
    if !status.stdout.is_empty() {
        return Err(Error::Refused(format!(
            "source tree has uncommitted changes; commit these files before staging:\n{}",
            String::from_utf8_lossy(&status.stdout).trim_end()
        )));
    }
    let commit = git(&["rev-parse", "--verify", "HEAD"])?;
    Ok(String::from_utf8_lossy(&commit.stdout).trim().to_owned())
}
