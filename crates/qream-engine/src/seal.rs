//! A seal is the whole receipt. Snapshots are reflink copies, never handcrafted archives.
use crate::{
    Error, Result,
    mail::Mail,
    storage::{self, io},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::Command,
};
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Entry {
    pub path: String,
    pub sha256: String,
    pub bytes: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Receipt {
    pub task: String,
    pub version: u64,
    pub sealed_at: String,
    pub candidate: Option<PathBuf>,
    pub report: PathBuf,
    pub report_sha256: String,
    pub files: usize,
    pub candidate_sha256: String,
    pub entries: Vec<Entry>,
}
pub fn digest(path: &Path) -> Result<String> {
    let mut file = io(path, fs::File::open(path))?;
    let mut sha = Sha256::new();
    let mut buf = [0u8; 1024 * 1024];
    loop {
        let n = io(path, file.read(&mut buf))?;
        if n == 0 {
            break;
        }
        sha.update(&buf[..n]);
    }
    Ok(hex(&sha.finalize()))
}
fn entries(root: &Path, dir: &Path, out: &mut Vec<Entry>) -> Result<()> {
    let mut children = io(dir, fs::read_dir(dir))?
        .map(|e| io(dir, e).map(|e| e.path()))
        .collect::<Result<Vec<_>>>()?;
    children.sort();
    // Python os.walk yields all files in this directory before descending into sorted subdirectories.
    for p in &children {
        let meta = io(p, fs::symlink_metadata(p))?;
        if meta.is_file() {
            out.push(Entry {
                path: p
                    .strip_prefix(root)
                    .map_err(|_| Error::Denied("candidate path escaped".into()))?
                    .to_string_lossy()
                    .into(),
                sha256: digest(p)?,
                bytes: meta.len(),
            });
        }
    }
    for p in children {
        if io(&p, fs::symlink_metadata(&p))?.is_dir()
            && ![
                ".git",
                "target",
                "node_modules",
                "__pycache__",
                ".cache",
                "scratch",
            ]
            .contains(&p.file_name().and_then(|s| s.to_str()).unwrap_or(""))
        {
            entries(root, &p, out)?;
        }
    }
    Ok(())
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn python_string(s: &str) -> Result<String> {
    let json = serde_json::to_string(s)?;
    let mut out = String::new();
    for c in json.chars() {
        if c.is_ascii() {
            out.push(c);
        } else {
            let mut buf = [0; 2];
            for u in c.encode_utf16(&mut buf) {
                out.push_str(&format!("\\u{u:04x}"));
            }
        }
    }
    Ok(out)
}
fn python_json_digest(entries: &[Entry]) -> Result<String> {
    let mut pieces = vec![];
    for e in entries {
        pieces.push(format!(
            "{{\"bytes\": {}, \"path\": {}, \"sha256\": {}}}",
            e.bytes,
            python_string(&e.path)?,
            serde_json::to_string(&e.sha256)?
        ));
    }
    Ok(hex(&Sha256::digest(
        format!("[{}]", pieces.join(", ")).as_bytes(),
    )))
}
fn copy(from: &Path, to: &Path) -> Result<()> {
    let output = io(
        from,
        Command::new("cp")
            .args(["-a", "--reflink=auto"])
            .arg(from)
            .arg(to)
            .output(),
    )?;
    if !output.status.success() {
        return Err(Error::Denied(format!(
            "reflink snapshot failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(())
}
pub fn seal(root: &Path, task: &str, candidate: Option<&Path>, report: &Path) -> Result<Receipt> {
    storage::token(task)?;
    let report = io(report, fs::canonicalize(report))?;
    let candidate = candidate.map(|p| io(p, fs::canonicalize(p))).transpose()?;
    if candidate.as_ref().is_some_and(|p| !p.is_dir()) {
        return Err(Error::Denied("candidate must be a directory".into()));
    }
    let art = root.join("artifacts").join(task);
    let _guard = storage::lock(&art.join(".seal.lock"))?;
    let freeze = art.join("freeze.json");
    let version = if freeze.exists() {
        let old: Receipt = storage::read(&freeze)?;
        let snap = art.join(format!("versions/v{}", old.version));
        io(&snap, fs::create_dir_all(&snap))?;
        copy(&freeze, &snap.join("freeze.json"))?;
        if !snap.join("candidate").exists()
            && let Some(c) = &old.candidate
        {
            copy(c, &snap.join("candidate"))?;
        }
        old.version + 1
    } else {
        1
    };
    let mut list = vec![];
    if let Some(c) = &candidate {
        entries(c, c, &mut list)?;
    }
    let rec = Receipt {
        task: task.into(),
        version,
        sealed_at: chrono::Local::now()
            .format("%Y-%m-%dT%H:%M:%S%z")
            .to_string(),
        candidate,
        report_sha256: digest(&report)?,
        report,
        files: list.len(),
        candidate_sha256: python_json_digest(&list)?,
        entries: list,
    };
    storage::write(&freeze, &rec)?;
    // Keep bytes at sealing time: resealing after edits must not snapshot the already changed directory.
    let snap = art.join(format!("versions/v{version}"));
    io(&snap, fs::create_dir_all(&snap))?;
    copy(&freeze, &snap.join("freeze.json"))?;
    if let Some(c) = &rec.candidate {
        copy(c, &snap.join("candidate"))?;
    }
    Ok(rec)
}
pub fn validate_report(report: &Path) -> Result<()> {
    let text = io(report, fs::read_to_string(report))?;
    let lines: Vec<_> = text.lines().collect();
    let prefixes = [
        "Surprise:",
        "Friction:",
        "Directive fix:",
        "Look hardest at:",
    ];
    let missing: Vec<_> = prefixes
        .iter()
        .filter(|prefix| !lines.iter().any(|line| line.starts_with(**prefix)))
        .copied()
        .collect();
    if !missing.is_empty() {
        return Err(Error::Denied(format!(
            "report is missing closing lines: {}",
            missing.join(", ")
        )));
    }
    if lines.len() > 150 || lines.len() < 4 {
        return Err(Error::Denied("report must have 4 to 150 lines".into()));
    }
    if lines[lines.len() - 4..]
        .iter()
        .zip(prefixes)
        .any(|(line, prefix)| !line.starts_with(prefix))
    {
        return Err(Error::Denied(format!(
            "report must end with these closing lines in order: {}",
            prefixes.join(", ")
        )));
    }
    Ok(())
}
/// Controller's durable seal event is mail; one hash/version can wake judgment only once.
pub fn submit(root: &Path, campaign: &str, task: &str, report: &Path) -> Result<String> {
    validate_report(report)?;
    storage::token(campaign)?;
    storage::token(task)?;
    let receipt: Receipt = storage::read(&root.join(format!("artifacts/{task}/freeze.json")))?;
    if receipt.report != io(report, fs::canonicalize(report))?
        || receipt.report_sha256 != digest(report)?
    {
        return Err(Error::Denied("report does not match its seal".into()));
    }
    if let Some(candidate) = &receipt.candidate {
        let mut current = vec![];
        entries(candidate, candidate, &mut current)?;
        if python_json_digest(&current)? != receipt.candidate_sha256 {
            return Err(Error::Denied("candidate changed after sealing".into()));
        }
    }
    let id = format!(
        "{task}-sealed-{}-{}",
        receipt.version,
        &receipt.report_sha256[..12]
    );
    let mail = Mail::new(root);
    let to = format!("{campaign}/controller");
    if mail.get(&to, &id).is_err() {
        mail.send(
            &id,
            &format!("{campaign}/worker/{task}"),
            &to,
            &serde_json::to_string(&receipt)?,
            false,
        )?;
    }
    Ok(id)
}
