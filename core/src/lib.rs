//! Everything both the daemon and the picker need, and nothing either of them
//! draws with.
//!
//! The rule for this crate is that it has no opinion about terminals, and as
//! close to no dependencies as the job allows. That is enforced rather than
//! asserted: `cli` links ratatui, this does not, and nothing here can reach
//! `cli` to borrow it. The one crate it does take is a pure-Rust SQLite, behind
//! a default feature, because two agents keep their conversations in one.

pub mod agents;
pub mod conv;
pub mod env;
pub mod hook;
pub mod index;
pub mod json;
pub mod log;
pub mod panes;
pub mod paths;
pub mod proc;
pub mod sha256;
pub mod sqlite;
pub mod stat;
pub mod state;
pub mod tmux;
pub mod transcript;
pub mod version;
