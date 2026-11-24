use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{path}:{line}: {message}")]
    Parse {
        path: PathBuf,
        line: usize,
        message: String,
    },
    #[error("JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("rule table: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("rule pattern: {0}")]
    Regex(#[from] regex::Error),
    /// A row rule's refusal, in the words the window shows.
    #[error("{rule} refuses {row} ({words}): {reason}")]
    Refused {
        rule: String,
        row: String,
        words: String,
        reason: String,
    },
    /// Any other refusal, as one plain sentence.
    #[error("{0}")]
    Denied(String),
    #[error("tracker store: {0}")]
    Store(#[from] tracker_store::Error),
    #[error("terminal: {0}")]
    Term(#[from] qream_term::Error),
    #[error("control ledger: {0}")]
    Ledger(#[from] rusqlite::Error),
    #[error("not built yet: {0}")]
    Unimplemented(&'static str),
    #[error("This daemon is read-only")]
    ReadOnly,
    #[error("{0}")]
    Invalid(String),
    #[error("not found: {0}")]
    NotFound(String),
    /// A pause or kill fences this unit until the operator releases it.
    #[error("held: {0}")]
    Held(String),
    #[error("the campaign's STOP marker is present at {0}")]
    Stop(PathBuf),
    #[error("stopped: {0}")]
    Stopped(String),
    #[error("task {task} still has a live process it started: {purpose} (pids {pids:?})")]
    LiveProcess {
        task: String,
        purpose: String,
        pids: Vec<u32>,
    },
    /// The process or unit is not one QREAM started; it is never signalled.
    #[error("not owned: {0}")]
    NotOwned(String),
    #[error("signal failed: {0}")]
    Signal(String),
    /// The daemon's own refusal or fault, carried with its class to the front.
    #[error("{message}")]
    Remote { code: String, message: String },
}
pub type Result<T> = std::result::Result<T, Error>;
impl Error {
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
    /// Stable class for socket replies and the command line's exit code.
    pub fn code(&self) -> &str {
        match self {
            Self::Refused { .. } | Self::Denied(_) => "refused",
            Self::Io { .. } => "io",
            Self::Json(_) => "invalid",
            Self::Parse { .. } | Self::Toml(_) | Self::Regex(_) => "parse",
            Self::Store(e) => e.code(),
            Self::Term(e) => e.code(),
            Self::Ledger(_) => "unreadable",
            Self::Unimplemented(_) => "unimplemented",
            Self::Remote { code, .. } => code,
            Self::ReadOnly => "read_only",
            Self::Invalid(_) => "invalid",
            Self::NotFound(_) => "not_found",
            Self::Held(_) | Self::Stop(_) => "held",
            Self::Stopped(_) => "stopped",
            Self::LiveProcess { .. } => "live_process",
            Self::NotOwned(_) => "not_owned",
            Self::Signal(_) => "fault",
        }
    }
    /// A fault stops the act it happened in; any other error is a refusal reported per unit.
    pub fn is_fault(&self) -> bool {
        matches!(
            self,
            Self::Io { .. } | Self::Json(_) | Self::Signal(_) | Self::Ledger(_)
        )
    }
}
pub fn io(path: &Path, source: std::io::Error) -> Error {
    Error::io(path, source)
}
pub fn read(path: &Path) -> Result<String> {
    std::fs::read_to_string(path).map_err(|e| io(path, e))
}
pub fn write(path: &Path, text: impl AsRef<[u8]>) -> Result<()> {
    std::fs::write(path, text).map_err(|e| io(path, e))
}
pub fn mkdir(path: &Path) -> Result<()> {
    std::fs::create_dir_all(path).map_err(|e| io(path, e))
}
pub fn refuse(rule: &str, row: &str, purpose: &str, reason: &str) -> Error {
    Error::Refused {
        rule: rule.into(),
        row: row.into(),
        words: purpose
            .split_whitespace()
            .take(6)
            .collect::<Vec<_>>()
            .join(" "),
        reason: reason.into(),
    }
}
