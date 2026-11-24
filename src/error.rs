use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("operation is not implemented")]
    Unimplemented,
    #[error("I/O at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Arguments(#[from] clap::Error),
    #[error("invalid source path: {0}")]
    InvalidPath(PathBuf),
    #[error("{0}")]
    Refused(String),
    #[error(transparent)]
    Engine(#[from] qream_engine::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
