//! Shared command boundary, build identity and release provenance for QREAM's thin fronts.

pub mod cli;
pub mod error;
#[path = "../tools/release_record.rs"]
pub mod release_record;
pub mod verbs;

pub use error::{Error, Result};
pub use qream_engine as engine;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct BuildIdentity {
    pub version: String,
    pub git_describe: Option<String>,
    pub build_time: String,
}

pub fn build_identity() -> BuildIdentity {
    BuildIdentity {
        version: env!("CARGO_PKG_VERSION").to_owned(),
        git_describe: option_env!("QREAM_GIT_DESCRIBE").map(str::to_owned),
        build_time: env!("QREAM_BUILD_TIME").to_owned(),
    }
}
