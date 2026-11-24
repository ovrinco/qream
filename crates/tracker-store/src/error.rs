use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Invalid(String),
    #[error("conflict on {id}.{field}: expected {expected}, found {found}")]
    Conflict {
        id: String,
        field: String,
        expected: String,
        found: String,
    },
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("database: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error("JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("CSV: {0}")]
    Csv(#[from] csv::Error),
    #[error("workbook: {0}")]
    Xlsx(#[from] rust_xlsxwriter::XlsxError),
    #[error("unimplemented: {0}")]
    Unimplemented(&'static str),
}
impl Error {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Conflict { .. } => "conflict",
            Self::Invalid(_) => "invalid_request",
            Self::Unimplemented(_) => "unimplemented",
            Self::Io { .. } => "io",
            Self::Sql(_) => "store_unavailable",
            Self::Json(_) | Self::Csv(_) | Self::Xlsx(_) => "encoding",
        }
    }
    pub(crate) fn io(path: &std::path::Path, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
}
pub type Result<T> = std::result::Result<T, Error>;
