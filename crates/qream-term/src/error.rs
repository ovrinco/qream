use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{operation} failed at {path}: {source}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    // portable-pty exposes an anyhow error. Preserve it as an opaque source once
    // at the crate boundary; anyhow does not enter this library's signatures.
    #[error("PTY {operation} failed in {cwd}: {source}")]
    Pty {
        operation: &'static str,
        cwd: PathBuf,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    #[error("invalid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid rules: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("invalid marker: {0}")]
    Regex(#[from] regex::Error),
    #[error("invalid argument: {0}")]
    Invalid(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("busy: pane is not idle and accepting input")]
    Busy,
    #[error("pane lost: {0}")]
    Lost(String),
    #[error("pane operation failed: {0}")]
    Pane(String),
}
pub type Result<T> = std::result::Result<T, Error>;
impl Error {
    pub(crate) fn io(
        operation: &'static str,
        path: impl Into<PathBuf>,
        source: std::io::Error,
    ) -> Self {
        Self::Io {
            operation,
            path: path.into(),
            source,
        }
    }
    /// Stable boundary class for daemon/command replies.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Io { .. } | Self::Pty { .. } | Self::Pane(_) => "fault",
            Self::Json(_) | Self::Toml(_) | Self::Regex(_) | Self::Invalid(_) => "invalid",
            Self::NotFound(_) => "not_found",
            Self::Busy => "busy",
            Self::Lost(_) => "lost",
        }
    }
}
pub(crate) fn io(path: &Path, source: std::io::Error) -> Error {
    Error::io("access", path, source)
}
