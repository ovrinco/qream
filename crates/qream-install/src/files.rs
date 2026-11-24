//! Hashes, tree walks, copies and atomic link swaps.

use crate::error::{At, Result};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn sha256_file(path: &Path) -> Result<String> {
    Ok(hex(&Sha256::digest(std::fs::read(path).at(path)?)))
}

/// Every regular file under `root` (relative, sorted), skipping `.git`, `target`, `evidence` and `scratch`
/// (a development daemon writes there while the check runs).
pub fn walk(root: &Path) -> Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    walk_into(root, root, &mut found)?;
    found.sort();
    Ok(found)
}

fn walk_into(root: &Path, dir: &Path, found: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir).at(dir)? {
        let entry = entry.at(dir)?;
        let name = entry.file_name();
        if name == ".git" || name == "target" || name == "evidence" || name == "scratch" {
            continue;
        }
        let path = entry.path();
        let kind = entry.file_type().at(&path)?;
        if kind.is_dir() {
            walk_into(root, &path, found)?;
        } else if kind.is_file() {
            if let Ok(rel) = path.strip_prefix(root) {
                found.push(rel.to_owned());
            }
        }
    }
    Ok(())
}

/// One hash over the names and contents of a tree: what the check passed is what is staged.
pub fn tree_hash(root: &Path) -> Result<String> {
    let mut tree = Sha256::new();
    for rel in walk(root)? {
        tree.update(rel.to_string_lossy().as_bytes());
        tree.update([0]);
        tree.update(sha256_file(&root.join(&rel))?.as_bytes());
        tree.update(b"\n");
    }
    Ok(hex(&tree.finalize()))
}

pub fn copy_dir(from: &Path, to: &Path) -> Result<()> {
    std::fs::create_dir_all(to).at(to)?;
    for entry in std::fs::read_dir(from).at(from)? {
        let entry = entry.at(from)?;
        let path = entry.path();
        let dest = to.join(entry.file_name());
        if entry.file_type().at(&path)?.is_dir() {
            copy_dir(&path, &dest)?;
        } else {
            std::fs::copy(&path, &dest).at(&dest)?;
        }
    }
    Ok(())
}

/// `SHA256SUMS` lines for every file of a release except the sums file and the gate records.
pub fn sums(dir: &Path) -> Result<String> {
    let mut out = String::new();
    for rel in walk(dir)? {
        let name = rel.to_string_lossy();
        if matches!(name.as_ref(), "SHA256SUMS" | "SMOKE.json") {
            continue;
        }
        out.push_str(&format!("{}  {name}\n", sha256_file(&dir.join(&rel))?));
    }
    Ok(out)
}

/// Point `link` at `target` atomically: a fresh link beside it, renamed over it.
pub fn swap_link(link: &Path, target: &Path) -> Result<()> {
    let parent = link.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent).at(parent)?;
    let name = link.file_name().map(|n| n.to_string_lossy().into_owned());
    let tmp = parent.join(format!(
        ".{}.qream-install.{}",
        name.unwrap_or_default(),
        std::process::id()
    ));
    std::os::unix::fs::symlink(target, &tmp).at(&tmp)?;
    std::fs::rename(&tmp, link).at(link)
}

/// What stands at a link path before it is re-pointed.
pub enum Prior {
    Absent,
    Link(PathBuf),
    File,
}

pub fn prior(path: &Path) -> Result<Prior> {
    match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Prior::Absent),
        Err(e) => Err(e).at(path),
        Ok(m) if m.file_type().is_symlink() => Ok(Prior::Link(std::fs::read_link(path).at(path)?)),
        Ok(_) => Ok(Prior::File),
    }
}

pub fn sha256_text(text: &str) -> String {
    hex(&Sha256::digest(text.as_bytes()))
}
