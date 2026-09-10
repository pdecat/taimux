//! Where this tool puts things at runtime.
//!
//! One module because there were two copies of the answer: `main::runtime_dir`
//! and `index::hook_dir` were byte-identical functions under different names,
//! each with its own callers, and neither knew about the other. Two spellings of
//! one directory is the kind of duplicate that survives indefinitely, because
//! nothing ever disagrees until somebody changes one of them.
//!
//! Nothing here is configurable except the socket, and that only so the test
//! suite can point a client at a path nothing is listening on.

use std::path::PathBuf;

use crate::env;

/// The per-user runtime directory for this tool.
///
/// `XDG_RUNTIME_DIR` first, which on a normal login is a tmpfs the session owns
/// and the system clears on logout: exactly right for hook state and a socket,
/// neither of which should outlive the session that made them. `TMPDIR` then
/// `/tmp` are the fallbacks for a context that has no session, such as a cron
/// job or a container.
pub fn runtime_dir() -> PathBuf {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .or_else(|| std::env::var_os("TMPDIR"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    base.join("taimux")
}

/// The daemon's socket.
///
/// Overridable because the suite needs to point a client at a path nothing is
/// listening on, which is the state on every host that has never run a daemon
/// and the case the fallback-to-in-process path exists for.
pub fn socket_path() -> PathBuf {
    env::var("TAIMUX_SOCKET")
        .map(PathBuf::from)
        .unwrap_or_else(|| runtime_dir().join("sock"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_runtime_dir_prefers_xdg_then_tmpdir_then_tmp() {
        let _g = env::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (xdg, tmp) = (
            std::env::var_os("XDG_RUNTIME_DIR"),
            std::env::var_os("TMPDIR"),
        );

        std::env::set_var("XDG_RUNTIME_DIR", "/run/user/9");
        std::env::set_var("TMPDIR", "/var/tmp");
        assert_eq!(runtime_dir(), PathBuf::from("/run/user/9/taimux"));

        std::env::remove_var("XDG_RUNTIME_DIR");
        assert_eq!(runtime_dir(), PathBuf::from("/var/tmp/taimux"));

        std::env::remove_var("TMPDIR");
        assert_eq!(runtime_dir(), PathBuf::from("/tmp/taimux"));

        match xdg {
            Some(v) => std::env::set_var("XDG_RUNTIME_DIR", v),
            None => std::env::remove_var("XDG_RUNTIME_DIR"),
        }
        match tmp {
            Some(v) => std::env::set_var("TMPDIR", v),
            None => std::env::remove_var("TMPDIR"),
        }
    }

    #[test]
    fn the_socket_sits_in_the_runtime_dir_unless_overridden() {
        let _g = env::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let had = std::env::var_os("TAIMUX_SOCKET");

        std::env::remove_var("TAIMUX_SOCKET");
        assert_eq!(socket_path(), runtime_dir().join("sock"));

        std::env::set_var("TAIMUX_SOCKET", "/nowhere/sock");
        assert_eq!(socket_path(), PathBuf::from("/nowhere/sock"));

        match had {
            Some(v) => std::env::set_var("TAIMUX_SOCKET", v),
            None => std::env::remove_var("TAIMUX_SOCKET"),
        }
    }
}
