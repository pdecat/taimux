//! The picker and the commands a person runs: the TUI, the row layout, restart,
//! resurrect, and the ssh federation.
//!
//! The only crate here that links a terminal library. Nothing in `core` or
//! `daemon` can see any of it.

pub mod act;
pub mod ansi;
pub mod install;
pub mod remote;
pub mod restart;
pub mod resurrect;
pub mod rows;
pub mod tui;
