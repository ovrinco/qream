//! Where the daemon lives and how it runs: its home, socket, store and user unit, read
//! from the environment once, and the server's main loop (`qreamd` calls `server_main`).
use crate::{Error, Result, api::Api};
use std::path::PathBuf;

/// Resolve the account's real home independently of a fixture's HOME override.
/// Linux is the supported runtime platform.
pub fn operator_home() -> Result<PathBuf> {
    let status = std::fs::read_to_string("/proc/self/status")
        .map_err(|e| Error::io("/proc/self/status", e))?;
    let uid = status
        .lines()
        .find_map(|line| {
            line.strip_prefix("Uid:")
                .and_then(|v| v.split_whitespace().next())
        })
        .ok_or_else(|| Error::Denied("The process user identity is unavailable".into()))?;
    let accounts =
        std::fs::read_to_string("/etc/passwd").map_err(|e| Error::io("/etc/passwd", e))?;
    accounts
        .lines()
        .find_map(|line| {
            let fields: Vec<_> = line.split(':').collect();
            (fields.len() >= 7 && fields[2] == uid && fields[5].starts_with('/'))
                .then(|| PathBuf::from(fields[5]))
        })
        .ok_or_else(|| Error::Denied("The process user has no local account home".into()))
}

pub fn default_store() -> Result<PathBuf> {
    let base = std::env::var_os("XDG_DATA_HOME")
        .filter(|v| !v.is_empty() && PathBuf::from(v).is_absolute())
        .map(PathBuf::from)
        .unwrap_or(operator_home()?.join(".local/share"));
    Ok(base.join("qream/tracker.sqlite"))
}

/// `QREAM_HOME`, `QREAM_SOCKET`, `QREAM_STORE` and `QREAM_UNIT` override the defaults
/// (`~/.local/share/qream`, `$XDG_RUNTIME_DIR/qream/engine.sock`, the live store,
/// `qream-daemon`); tests point them at temporary copies.
#[derive(Clone, Debug)]
pub struct Paths {
    pub home: PathBuf,
    pub socket: PathBuf,
    pub store: PathBuf,
    pub unit: String,
    /// `QREAM_READ_ONLY=1`: registry and store read-only, every write refused.
    pub read_only: bool,
}
impl Paths {
    pub fn from_env() -> Result<Self> {
        let var = |key: &str| std::env::var_os(key).filter(|v| !v.is_empty());
        let home = match var("QREAM_HOME") {
            Some(home) => PathBuf::from(home),
            None => var("HOME")
                .map(|p| PathBuf::from(p).join(".local/share/qream"))
                .ok_or_else(|| Error::Denied("The user home is unavailable".into()))?,
        };
        let socket = match var("QREAM_SOCKET") {
            Some(socket) => PathBuf::from(socket),
            None => var("XDG_RUNTIME_DIR")
                .map(|p| PathBuf::from(p).join("qream/engine.sock"))
                .ok_or_else(|| Error::Denied("The user runtime directory is unavailable".into()))?,
        };
        let paths = Self {
            home,
            socket,
            store: match var("QREAM_STORE") {
                Some(path) => PathBuf::from(path),
                None => default_store()?,
            },
            unit: var("QREAM_UNIT")
                .and_then(|u| u.into_string().ok())
                .unwrap_or_else(|| "qream-daemon".into()),
            read_only: var("QREAM_READ_ONLY").is_some_and(|v| v == "1"),
        };
        if let Err(error) = crate::live_guard::check(&paths) {
            eprintln!("{error}");
            std::process::exit(64);
        }
        Ok(paths)
    }
    /// The environment a daemon started on demand must carry to find the same places.
    pub fn exported(&self) -> Vec<(String, String)> {
        // Test overrides of the private folder and the harness session folders travel too.
        let forwarded = [
            "QREAM_TEST",
            "QREAM_TEST_SCRATCH",
            "QREAM_PRIVATE_DIR",
            "QREAM_CODEX_SESSIONS",
            "QREAM_CLAUDE_PROJECTS",
        ]
        .into_iter()
        .filter_map(|key| Some((key.to_owned(), std::env::var(key).ok()?)));
        let mut exported = vec![
            ("QREAM_HOME".into(), self.home.display().to_string()),
            ("QREAM_SOCKET".into(), self.socket.display().to_string()),
            ("QREAM_STORE".into(), self.store.display().to_string()),
            ("QREAM_UNIT".into(), self.unit.clone()),
            (
                "QREAM_READ_ONLY".into(),
                if self.read_only { "1" } else { "0" }.into(),
            ),
        ];
        // How long a launch waits before it lands; tests widen the window a stop may land in.
        if let Some(ms) =
            std::env::var_os("QREAM_LAUNCH_SETTLE_MS").and_then(|v| v.into_string().ok())
        {
            exported.push(("QREAM_LAUNCH_SETTLE_MS".into(), ms));
        }
        exported.extend(forwarded);
        exported
    }
}

/// Run the daemon until `daemon.stop` or the idle window: restore sessions, then serve.
pub fn server_main(build: String) -> Result<()> {
    let paths = Paths::from_env()?;
    let runtime = tokio::runtime::Runtime::new().map_err(|e| Error::io("tokio runtime", e))?;
    runtime.block_on(async {
        let mut api = Api::open(paths.home.clone(), Some(paths.store.clone()), build, 0)?;
        if paths.read_only {
            api = api.into_read_only();
        }
        api.restore()?;
        *api.worker_env.lock() = paths
            .exported()
            .into_iter()
            .filter(|(key, _)| key != "QREAM_READ_ONLY")
            .chain(std::env::current_exe().ok().map(|exe| {
                (
                    "QREAM_BIN".into(),
                    exe.with_file_name("qream").display().to_string(),
                )
            }))
            .collect();
        api.restore_controllers()?;
        api.serve(&paths.socket).await
    })
}
