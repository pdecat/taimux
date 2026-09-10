//! Everything both the daemon and the picker need, and nothing either of them
//! draws with.
//!
//! The rule for this crate is that it has no external dependencies and no
//! opinion about terminals. That is enforced rather than asserted: `cli` links
//! ratatui, this does not, and nothing here can reach `cli` to borrow it.

pub mod conv;
pub mod env;
pub mod hook;
pub mod index;
pub mod json;
pub mod log;
pub mod panes;
pub mod paths;
pub mod proc;
pub mod stat;
pub mod state;
pub mod tmux;
pub mod transcript;
pub mod version;
