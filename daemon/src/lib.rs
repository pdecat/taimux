//! The collection daemon: the socket, the protocol both ends speak, and the
//! background indexer.
//!
//! Nothing here draws. It depends on `taimux-core` and on nothing else, which
//! is the whole point of the crate existing separately: "the daemon does not
//! link the terminal stack" used to be a claim in a comment, and is now
//! something the compiler will not let anyone break.

pub mod indexer;
pub mod protocol;
