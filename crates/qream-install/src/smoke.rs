//! Smoke the staged release against the live home, read-only: the registry must parse, the staged daemon must
//! answer on a private socket with the staged build, list campaigns and read one campaign's states, and the home
//! must be byte-for-byte the same afterwards. Any error refuses the switch.

use crate::error::{At, Error, Result};
use crate::{files, Install};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Debug, Serialize, Deserialize)]
pub struct SmokeRecord {
    pub release_id: String,
    pub home: PathBuf,
    pub ok: bool,
    pub campaigns: usize,
    pub states_of: Option<String>,
    pub sums_sha256: String,
    pub error: Option<String>,
    pub seconds: f64,
    pub at: String,
}

/// Every path of the home with its length and modified time, so any write by the smoke is caught.
fn snapshot(home: &Path) -> Result<String> {
    if !home.is_dir() {
        return Ok(String::new());
    }
    let mut lines = Vec::new();
    for rel in files::walk(home)? {
        let path = home.join(&rel);
        let meta = std::fs::symlink_metadata(&path).at(&path)?;
        let modified = meta
            .modified()
            .ok()
            .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_nanos());
        lines.push(format!("{}\t{}\t{modified}", rel.display(), meta.len()));
    }
    Ok(files::sha256_text(&lines.join("\n")))
}

fn registry(home: &Path) -> Result<usize> {
    let dir = home.join("campaigns");
    if !dir.is_dir() {
        return Ok(0);
    }
    let mut n = 0;
    for rel in files::walk(&dir)? {
        if rel.extension().is_some_and(|e| e == "json") {
            let path = dir.join(&rel);
            serde_json::from_slice::<Value>(&std::fs::read(&path).at(&path)?).map_err(|e| {
                Error::Refused(format!(
                    "registry file {} does not parse: {e}",
                    path.display()
                ))
            })?;
            n += 1;
        }
    }
    Ok(n)
}

struct Client {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
    next: u64,
}

impl Client {
    fn line(&mut self) -> Result<Value> {
        let mut text = String::new();
        let read = self
            .reader
            .read_line(&mut text)
            .at(Path::new("daemon socket"))?;
        if read == 0 {
            return Err(Error::Refused("the staged daemon closed the socket".into()));
        }
        Ok(serde_json::from_str(&text)?)
    }

    fn call(&mut self, cmd: &str, args: Value) -> Result<Value> {
        self.next += 1;
        let id = format!("smoke-{}", self.next);
        let mut text = json!({"v": 1, "id": id, "cmd": cmd, "args": args}).to_string();
        text.push('\n');
        self.writer
            .write_all(text.as_bytes())
            .at(Path::new("daemon socket"))?;
        loop {
            let reply = self.line()?;
            if reply.get("id").and_then(Value::as_str) != Some(id.as_str()) {
                continue; // an event, not the reply
            }
            if reply.get("ok") == Some(&Value::Bool(true)) {
                return Ok(reply.get("result").cloned().unwrap_or(Value::Null));
            }
            return Err(Error::Refused(format!(
                "{cmd} answered {}",
                reply.get("error").cloned().unwrap_or(Value::Null)
            )));
        }
    }
}

impl Install {
    /// `home` is the live QREAM home; `scratch` holds the private socket and is the daemon's runtime dir.
    pub fn smoke(&self, id: &str, home: &Path, scratch: &Path) -> Result<SmokeRecord> {
        crate::valid_id(id)?;
        let start = Instant::now();
        let dir = self.release(id);
        for name in ["PATTERN_MINE_herdr.md", "NOTICES.md"] {
            if !dir.join(name).is_file() {
                return Err(Error::Refused(format!("release {id} lacks {name}")));
            }
        }
        let sums_path = dir.join("SHA256SUMS");
        let sums = std::fs::read_to_string(&sums_path).at(&sums_path)?;
        if sums != files::sums(&dir)? {
            return Err(Error::Refused(format!(
                "release {id} differs from its SHA256SUMS"
            )));
        }
        let before = snapshot(home)?;
        let result = self.smoke_daemon(&dir, home, scratch);
        let after = snapshot(home);
        let result = match (result, after) {
            (Ok(v), Ok(a)) if a == before => Ok(v),
            (Ok(_), Ok(_)) => Err(Error::Refused("the smoke changed the live home".into())),
            (Err(e), _) | (_, Err(e)) => Err(e),
        };
        let (campaigns, states_of, error) = match &result {
            Ok((n, s)) => (*n, s.clone(), None),
            Err(e) => (0, None, Some(e.to_string())),
        };
        let record = SmokeRecord {
            release_id: id.into(),
            home: home.to_owned(),
            ok: error.is_none(),
            campaigns,
            states_of,
            sums_sha256: files::sha256_text(&sums),
            error,
            seconds: start.elapsed().as_secs_f64(),
            at: crate::now(),
        };
        let path = dir.join("SMOKE.json");
        std::fs::write(&path, serde_json::to_vec_pretty(&record)?).at(&path)?;
        match result {
            Ok(_) => Ok(record),
            Err(e) => Err(e),
        }
    }

    fn smoke_daemon(
        &self,
        dir: &Path,
        home: &Path,
        scratch: &Path,
    ) -> Result<(usize, Option<String>)> {
        let in_registry = registry(home)?;
        let stamp = chrono::Local::now().format("%Y%m%dT%H%M%S%.f");
        let runtime = scratch.join(format!("qream-smoke-{}-{stamp}", std::process::id()));
        std::fs::create_dir_all(&runtime).at(&runtime)?;
        let socket = runtime.join("qream").join("engine.sock");
        let daemon = dir.join("bin").join("qreamd");
        let mut child = Command::new(&daemon)
            .env("XDG_RUNTIME_DIR", &runtime)
            .env("QREAM_SOCKET", &socket)
            .env_remove("QREAM_UNIT")
            .env("QREAM_HOME", home)
            .env("QREAM_READ_ONLY", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .at(&daemon)?;
        let result = (|| {
            let deadline = Instant::now() + Duration::from_secs(15);
            let stream = loop {
                if let Ok(s) = UnixStream::connect(&socket) {
                    break s;
                }
                if let Ok(Some(status)) = child.try_wait() {
                    return Err(Error::Refused(format!(
                        "the staged daemon exited ({status}) before its socket opened"
                    )));
                }
                if Instant::now() >= deadline {
                    return Err(Error::Refused(
                        "the staged daemon opened no socket in 15 s".into(),
                    ));
                }
                std::thread::sleep(Duration::from_millis(50));
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(10)))
                .at(&socket)?;
            let mut client = Client {
                reader: BufReader::new(stream.try_clone().at(&socket)?),
                writer: stream,
                next: 0,
            };
            let hello = client.line()?;
            let build = hello.pointer("/data/build").and_then(Value::as_str);
            let id = dir.file_name().map(|n| n.to_string_lossy().into_owned());
            if hello.get("event").and_then(Value::as_str) != Some("hello") || build != id.as_deref()
            {
                return Err(Error::Refused(format!(
                    "the staged daemon's hello names build {build:?}, the release is {id:?}"
                )));
            }
            let list = client.call("campaigns.list", json!({}))?;
            let list = list
                .as_array()
                .ok_or_else(|| Error::Refused("campaigns.list is not a list".into()))?;
            if list.len() < in_registry {
                return Err(Error::Refused(format!(
                    "the registry holds {in_registry} campaigns, the staged daemon lists {}",
                    list.len()
                )));
            }
            let first = list
                .first()
                .and_then(|c| c.get("name"))
                .and_then(Value::as_str)
                .map(str::to_owned);
            if let Some(name) = &first {
                client.call("states", json!({"campaign": name}))?;
            }
            Ok((list.len(), first))
        })();
        let _ = child.kill();
        let _ = child.wait();
        result
    }
}
