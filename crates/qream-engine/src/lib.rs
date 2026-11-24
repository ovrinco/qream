//! Campaigns: the folder convention, lint and row rules, setup, the registry, the control
//! ledger, the one state model, the close, and the daemon's socket API over the terminal
//! layer and the store crate. Every front (`qream`, `qreamd`, the window) calls this library.
pub mod api;
pub mod capacity;
pub mod client;
pub mod close;
pub mod control;
pub mod controller;
pub mod conversation;
pub mod daemon;
pub mod error;
pub mod folder;
pub mod job;
pub mod ledger;
pub mod leftovers;
pub mod live;
pub mod live_guard;
pub mod mail;
pub mod mask;
pub mod observe;
pub mod owned;
pub mod panes;
pub mod profile;
pub mod quota;
pub mod records;
pub mod recover;
pub mod registry;
pub mod rows;
pub mod rules;
pub mod seal;
pub mod seat;
pub mod setup;
pub mod states;
mod storage;

pub use control::{Action, Engine, Scope};
pub use error::{Error, Result};
pub use qream_term as terminal;
pub use tracker_store as store;

pub fn now() -> String {
    chrono::Local::now().to_rfc3339()
}
