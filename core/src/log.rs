//! The log files this tool appends to, and keeping them bounded.
//!
//! Two functions that were stranded in `main.rs` with no relation to dispatch:
//! `act` and `tui` both reached UP into the binary root for `stamp`, which is
//! the one shape a library cannot have. They are here together because a
//! timestamp is only ever wanted for a log line, and a log that is appended to
//! forever is only safe if something trims it.

use std::process::Command;

/// An ISO 8601 local timestamp, which is what `date -Is` printed into this log.
pub fn stamp() -> String {
    Command::new("date")
        .arg("-Is")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim_end().to_string())
        .unwrap_or_default()
}

/// Keep the log bounded: it is appended to on every save, forever.
pub fn trim_log(path: &std::path::Path, over: usize, keep: usize) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() <= over {
        return;
    }
    let from = lines.len() - keep;
    let _ = std::fs::write(path, format!("{}\n", lines[from..].join("\n")));
}
