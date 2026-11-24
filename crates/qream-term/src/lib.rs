#![cfg_attr(not(test), deny(clippy::disallowed_methods))]
//! QREAM-owned terminal layer: PTY panes, screens, durable sessions, agent detection and local events.
//! Viewers never own the process or the pane's size; the session store outlives every viewer.
pub mod agent;
pub mod core;
pub mod detect;
mod error;
pub mod events;
pub mod handle;
pub mod input;
pub mod pane;
pub mod privacy;
pub mod screen;
pub mod session;
pub mod viewer;
#[allow(clippy::disallowed_methods)]
mod writer;
pub use error::{Error, Result};
pub use pane::{Pane, PtySpec};
pub use screen::{Color, Cursor, Row, Run, Screen, Update};
pub use viewer::Viewer;
