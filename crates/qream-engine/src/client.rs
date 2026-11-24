//! The socket client: connect without starting, or explicitly start a transient user unit.
//! Send one request and read its reply, skipping events.
use crate::{Error, Result, daemon::Paths};
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixStream,
    path::Path,
    time::{Duration, Instant},
};

pub struct Client {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
    pub hello: Value,
    next: u64,
}
impl Client {
    pub fn connect(socket: &Path) -> Result<Self> {
        let writer = UnixStream::connect(socket).map_err(|e| Error::io(socket, e))?;
        writer
            .set_read_timeout(Some(Duration::from_secs(60)))
            .map_err(|e| Error::io(socket, e))?;
        let reader = BufReader::new(writer.try_clone().map_err(|e| Error::io(socket, e))?);
        let mut client = Self {
            reader,
            writer,
            hello: Value::Null,
            next: 1,
        };
        client.hello = client.line(socket)?;
        Ok(client)
    }
    /// Connect without starting anything; explain the explicit start command on failure.
    pub fn connect_or_explain(paths: &Paths) -> Result<Self> {
        Self::connect(&paths.socket).map_err(|error| {
            Error::Denied(format!(
                "The engine is not running or did not answer. Start it with `qream daemon start`. ({error})"
            ))
        })
    }
    /// Explicitly start `qreamd` as the transient unit `paths.unit` and wait for it.
    pub fn start(paths: &Paths, qreamd: &Path) -> Result<(Self, bool)> {
        if let Ok(client) = Self::connect(&paths.socket) {
            return Ok((client, false));
        }
        // A unit of the same name still stopping (a daemon that just exited) refuses a new start
        // for a moment: try again for up to five seconds, unless another client started it.
        let tried = Instant::now();
        loop {
            let mut command = std::process::Command::new("systemd-run");
            command.args(["--user", "--collect", "--quiet", "--unit", &paths.unit]);
            for (key, value) in paths.exported() {
                command.arg(format!("--setenv={key}={value}"));
            }
            // Private test isolation must survive the user service manager's environment.
            if std::env::var("QREAM_TEST").as_deref() == Ok("1") {
                for key in [
                    "QREAM_TEST",
                    "HOME",
                    "XDG_DATA_HOME",
                    "XDG_STATE_HOME",
                    "XDG_CACHE_HOME",
                    "TMPDIR",
                ] {
                    if let Ok(value) = std::env::var(key) {
                        command.arg(format!("--setenv={key}={value}"));
                    }
                }
            }
            let status = command
                .arg(qreamd)
                .stdin(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .map_err(|e| Error::io("systemd-run", e))?;
            if status.success() {
                break;
            }
            if let Ok(client) = Self::connect(&paths.socket) {
                return Ok((client, false));
            }
            if tried.elapsed() >= Duration::from_secs(5) {
                return Err(Error::Denied(
                    "The user service manager refused to start the daemon".into(),
                ));
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(10) {
            if let Ok(client) = Self::connect(&paths.socket) {
                return Ok((client, true));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        Err(Error::Denied(
            "The daemon started but its socket did not answer".into(),
        ))
    }
    fn line(&mut self, socket: &Path) -> Result<Value> {
        let mut text = String::new();
        let n = self
            .reader
            .read_line(&mut text)
            .map_err(|e| Error::io(socket, e))?;
        if n == 0 {
            return Err(Error::Denied("The daemon closed the connection".into()));
        }
        Ok(serde_json::from_str(&text)?)
    }
    /// The whole reply line, `ok` true or false.
    pub fn request(&mut self, cmd: &str, args: Value) -> Result<Value> {
        let id = format!("cli-{}", self.next);
        self.next += 1;
        let mut line = serde_json::to_vec(&json!({"v":1,"id":id,"cmd":cmd,"args":args}))?;
        line.push(b'\n');
        self.writer
            .write_all(&line)
            .map_err(|e| Error::io("daemon socket", e))?;
        loop {
            let reply = self.line(Path::new("daemon socket"))?;
            if reply.get("id").and_then(Value::as_str) == Some(id.as_str()) {
                return Ok(reply);
            }
        }
    }
    /// The next event line (after `subscribe`), blocking on the socket up to `timeout`.
    pub fn event(&mut self, timeout: Duration) -> Result<Value> {
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(Error::Denied("No matching event before the timeout".into()));
            }
            self.writer
                .set_read_timeout(Some(left))
                .map_err(|e| Error::io("daemon socket", e))?;
            let line = match self.line(Path::new("daemon socket")) {
                Ok(line) => line,
                Err(Error::Io { source, .. })
                    if matches!(
                        source.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    return Err(Error::Denied("No matching event before the timeout".into()));
                }
                Err(e) => return Err(e),
            };
            if line.get("event").is_some() {
                return Ok(line);
            }
        }
    }
    /// The result, or the daemon's error as `Error::Remote` with its class.
    pub fn call(&mut self, cmd: &str, args: Value) -> Result<Value> {
        let reply = self.request(cmd, args)?;
        if reply["ok"] == true {
            return Ok(reply["result"].clone());
        }
        let text = |k: &str| reply["error"][k].as_str().unwrap_or("").to_owned();
        Err(Error::Remote {
            code: text("code"),
            message: text("message"),
        })
    }
}

/// The harness session of a campaign: `qream-<name>-harness` with a `chief` workspace.
/// Starts no agent; the orchestrator's seat is a pane opened later.
pub fn harness_up(client: &mut Client, name: &str, cwd: &Path) -> Result<Value> {
    let session = format!("qream-{name}-harness");
    client.call("term.session.up", json!({"name":session,"cwd":cwd}))?;
    let workspaces = client.call("term.workspaces", json!({"session":session}))?;
    let chief = workspaces
        .as_array()
        .and_then(|w| w.iter().find(|w| w["label"] == "chief").cloned());
    let chief = match chief {
        Some(chief) => chief,
        None => client.call(
            "term.workspace.new",
            json!({"session":session,"label":"chief","cwd":cwd,"env":{}}),
        )?,
    };
    Ok(json!({"session":session,"workspace":chief}))
}
pub fn harness_status(client: &mut Client, name: &str) -> Result<Value> {
    let session = format!("qream-{name}-harness");
    let sessions = client.call("term.sessions", json!({}))?;
    let found = sessions
        .as_array()
        .and_then(|s| s.iter().find(|s| s["name"] == session.as_str()).cloned());
    Ok(match found {
        Some(found) => {
            json!({"session":session,"up":found["up"],"workspaces":found["workspaces"],"panes":found["panes"]})
        }
        None => json!({"session":session,"up":false}),
    })
}
