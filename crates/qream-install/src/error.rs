use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("refused: {0}")]
    Refused(String),
    #[error("{step} failed: {detail}")]
    Step { step: String, detail: String },
    #[error("the check passed its wall clock of {limit_secs} s during {step}")]
    Timeout { step: String, limit_secs: u64 },
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Store(#[from] tracker_store::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Attach the path to an I/O result.
pub trait At<T> {
    fn at(self, path: &std::path::Path) -> Result<T>;
}

impl<T> At<T> for std::io::Result<T> {
    fn at(self, path: &std::path::Path) -> Result<T> {
        self.map_err(|source| Error::Io {
            path: path.to_owned(),
            source,
        })
    }
}
