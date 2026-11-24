//! Durable records: atomic JSON writes, locked appends and safe identifiers.
use crate::{Error, Result};
use serde::{Serialize, de::DeserializeOwned};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
};
pub(crate) fn io<T>(p: &Path, r: std::io::Result<T>) -> Result<T> {
    r.map_err(|source| Error::io(p, source))
}
/// A mail id, task, job or campaign name that is safe as one path component.
pub(crate) fn token(s: &str) -> Result<()> {
    if s.is_empty()
        || s.len() > 128
        || s.starts_with('.')
        || !s
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.:".contains(&b))
    {
        return Err(Error::Denied("unsafe identifier".into()));
    }
    Ok(())
}
pub(crate) fn lock(p: &Path) -> Result<File> {
    if let Some(parent) = p.parent() {
        io(parent, fs::create_dir_all(parent))?;
    }
    let f = io(
        p,
        OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(p),
    )?;
    io(p, f.lock())?;
    Ok(f)
}
pub fn read<T: DeserializeOwned>(path: &Path) -> Result<T> {
    Ok(serde_json::from_slice(&io(path, fs::read(path))?)?)
}
/// Write through a unique temporary file and rename: readers never see half a record.
pub fn write<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    static N: AtomicU64 = AtomicU64::new(0);
    let parent = path
        .parent()
        .ok_or_else(|| Error::Denied("record needs a parent directory".into()))?;
    io(parent, fs::create_dir_all(parent))?;
    let tmp = parent.join(format!(
        ".tmp-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let mut f = io(
        &tmp,
        OpenOptions::new().create_new(true).write(true).open(&tmp),
    )?;
    io(&tmp, f.write_all(&serde_json::to_vec_pretty(value)?))?;
    io(&tmp, f.sync_all())?;
    io(path, fs::rename(&tmp, path))
}
pub(crate) fn append<T: Serialize>(p: &Path, value: &T) -> Result<()> {
    let _guard = lock(&p.with_extension("lock"))?;
    let mut f = io(p, OpenOptions::new().create(true).append(true).open(p))?;
    let mut b = serde_json::to_vec(value)?;
    b.push(b'\n');
    io(p, f.write_all(&b))?;
    io(p, f.sync_all())
}
pub(crate) fn now() -> String {
    chrono::Local::now().to_rfc3339()
}
/// Reads a JSON record, or its default when it does not exist yet.
pub fn read_json<T: DeserializeOwned + Default>(path: &Path) -> Result<T> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(e) => Err(Error::io(path, e)),
    }
}
/// Writes a JSON record whole through a sibling and a rename, so a reader never sees half.
pub fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    write(path, value)
}
