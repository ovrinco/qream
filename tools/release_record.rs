//! Deterministic source manifest and build inputs for the future unified installer.

use crate::{BuildIdentity, Error, Result};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct SourceHash {
    pub path: String,
    pub sha256: String,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct ReleaseRecord {
    pub release_id: String,
    pub build: BuildIdentity,
    pub source_commit: String,
    pub source_clean: bool,
    pub tree_sha256: String,
    pub manifest: Vec<SourceHash>,
    pub lock_sha256: String,
    pub toolchain: String,
    pub toolchain_sha256: String,
    pub commands: Vec<String>,
}

fn io<T>(path: &Path, value: std::io::Result<T>) -> Result<T> {
    value.map_err(|source| Error::Io {
        path: path.to_owned(),
        source,
    })
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn hash(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

fn files(root: &Path, dir: &Path, output: &Path, found: &mut Vec<PathBuf>) -> Result<()> {
    for entry in io(dir, std::fs::read_dir(dir))? {
        let entry = io(dir, entry)?;
        let path = entry.path();
        let kind = io(&path, entry.file_type())?;
        if path == output
            || entry.file_name() == ".git"
            || entry.file_name() == "target"
            || entry.file_name() == "evidence"
        {
            continue;
        }
        if kind.is_symlink() {
            return Err(Error::InvalidPath(path));
        }
        if kind.is_dir() {
            files(root, &path, output, found)?;
        } else if kind.is_file() {
            found.push(
                path.strip_prefix(root)
                    .map_err(|_| Error::InvalidPath(path.clone()))?
                    .to_owned(),
            );
        }
    }
    Ok(())
}

pub fn write(
    source: &Path,
    out: &Path,
    release_id: &str,
    commands: Vec<String>,
) -> Result<ReleaseRecord> {
    let root = io(source, source.canonicalize())?;
    let source_commit =
        qream_install::stage::clean_commit(&root).map_err(|e| Error::Refused(e.to_string()))?;
    let out = io(out, out.canonicalize())?;
    let output = out.join("release_record.json");
    let mut paths = Vec::new();
    files(&root, &root, &output, &mut paths)?;
    paths.sort();
    let mut manifest = Vec::new();
    let mut tree = Sha256::new();
    for relative in paths {
        let path = root.join(&relative);
        let digest = hash(&io(&path, std::fs::read(&path))?);
        let name = relative
            .to_str()
            .ok_or_else(|| Error::InvalidPath(relative.clone()))?;
        tree.update(name.as_bytes());
        tree.update([0]);
        tree.update(digest.as_bytes());
        tree.update(b"\n");
        manifest.push(SourceHash {
            path: name.to_owned(),
            sha256: digest,
        });
    }
    let lock = root.join("Cargo.lock");
    let toolchain = root.join("rust-toolchain.toml");
    let toolchain_bytes = io(&toolchain, std::fs::read(&toolchain))?;
    let record = ReleaseRecord {
        release_id: release_id.to_owned(),
        build: crate::build_identity(),
        source_commit,
        source_clean: true,
        tree_sha256: hex(&tree.finalize()),
        manifest,
        lock_sha256: hash(&io(&lock, std::fs::read(&lock))?),
        toolchain: "1.94.0".to_owned(),
        toolchain_sha256: hash(&toolchain_bytes),
        commands,
    };
    io(
        &output,
        std::fs::write(&output, serde_json::to_vec_pretty(&record)?),
    )?;
    Ok(record)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_content_changes_and_excludes_its_own_record() -> Result<()> {
        let scratch = Path::new(env!("CARGO_MANIFEST_DIR")).join("crates/qream-install/scratch");
        io(&scratch, std::fs::create_dir_all(&scratch))?;
        let holder = io(&scratch, tempfile::tempdir_in(&scratch))?;
        let root = holder.path().to_owned();
        let git = |args: &[&str]| -> Result<()> {
            let output = io(
                &root,
                std::process::Command::new("git")
                    .current_dir(&root)
                    .args(args)
                    .output(),
            )?;
            if output.status.success() {
                Ok(())
            } else {
                Err(Error::Refused(
                    String::from_utf8_lossy(&output.stderr).into_owned(),
                ))
            }
        };
        git(&["init", "-q"])?;
        let exclude = root.join(".git/info/exclude");
        io(&exclude, std::fs::write(&exclude, "release_record.json\n"))?;
        for (file, bytes) in [
            ("Cargo.lock", "lock"),
            ("rust-toolchain.toml", "toolchain"),
            ("source.rs", "one"),
        ] {
            io(&root, std::fs::write(root.join(file), bytes))?;
        }
        let commit = || -> Result<()> {
            git(&["add", "."])?;
            git(&[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@localhost",
                "commit",
                "-qm",
                "fixture",
            ])
        };
        commit()?;
        let first = write(&root, &root, "probe", vec!["check".into()])?;
        assert!(first.source_clean);
        let second = write(&root, &root, "probe", vec!["check".into()])?;
        assert_eq!(first.tree_sha256, second.tree_sha256);
        assert_eq!(first.manifest.len(), 3);
        io(&root, std::fs::write(root.join("source.rs"), "two"))?;
        let dirty =
            write(&root, &root, "probe", vec!["check".into()]).expect_err("dirty source refused");
        assert!(dirty.to_string().contains("source.rs"));
        commit()?;
        let changed = write(&root, &root, "probe", vec!["check".into()])?;
        assert_ne!(first.source_commit, changed.source_commit);
        assert_ne!(first.tree_sha256, changed.tree_sha256);
        assert_eq!(first.lock_sha256, changed.lock_sha256);
        Ok(())
    }
}
