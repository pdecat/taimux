//! Rows held through a restart.
//!
//! A restart takes a session away for about a second: the old claude exits, the
//! shell has the pane, then the new one starts. Measured on 2.1.291 to 2.1.292,
//! a scan every 100ms found no agent in the pane for 0.8 to 1.6s, and a list
//! refreshed once a second lands in that gap more often than not. The row
//! dropped out and came back, and every row under it moved up and back down.
//!
//! The picker used to paper over that from what it had on screen, for the one
//! row `Ctrl-x` was pressed on. Nothing held a row through an `F8` sweep, a
//! `taimux restart` run from a shell, a picker opened after the key, or another
//! host's picker, and a sweep blinks out every row it touches, one after the
//! other.
//!
//! So the restart says so itself. Before its first keystroke it writes the
//! pane's row, exactly as the list shows it, under `restarting/`, and removes it
//! once the new session is up or the attempt is over. The scan lists that row,
//! in the pane's own place, for as long as the file is there and whatever the
//! pane is running meanwhile. It keeps the version and the state it had, which
//! is what keeps it in whichever list it was being watched in until the session
//! is back.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// How long a hold can last.
///
/// `restart` waits up to 12s for a session to exit and then polls up to 20s for
/// it to come back, and the hold is removed when it stops either way. This is
/// only the backstop for a restart killed before it could remove it, which
/// would otherwise list that pane as it was for ever.
pub const HOLD: Duration = Duration::from_secs(40);

fn dir() -> PathBuf {
    crate::paths::runtime_dir().join("restarting")
}

/// A pane id, and nothing that could name a path.
fn is_pane(id: &str) -> bool {
    id.len() > 1 && id.starts_with('%') && id[1..].bytes().all(|b| b.is_ascii_digit())
}

/// A restart in flight, for as long as this is in scope.
pub struct Held(Option<PathBuf>);

impl Held {
    /// Hold `pane` at the row it has now, until this is dropped.
    ///
    /// Taken before the first keystroke reaches the pane, so the row is the one
    /// the list was showing rather than whatever the screen says as the old
    /// session goes away. A pane whose row cannot be read is still marked, so
    /// the picker can say a restart is in flight, but it is listed as it is.
    pub fn begin(pane: &str) -> Held {
        let row = crate::panes::list_row(pane).unwrap_or_default();
        Held(write(&dir(), pane, &row))
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        if let Some(p) = &self.0 {
            let _ = std::fs::remove_file(p);
        }
    }
}

/// Written aside and renamed into place, because a scan may read it at any
/// moment and half a row is a malformed one.
fn write(dir: &Path, pane: &str, row: &str) -> Option<PathBuf> {
    if !is_pane(pane) {
        return None;
    }
    std::fs::create_dir_all(dir).ok()?;
    let path = dir.join(pane);
    let tmp = dir.join(format!(".{}.{}", pane, std::process::id()));
    std::fs::write(&tmp, row).ok()?;
    if std::fs::rename(&tmp, &path).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return None;
    }
    Some(path)
}

/// Every pane with a restart in flight, and the row it is held at: empty where
/// none could be read, in which case the pane is listed as it is.
pub fn held() -> HashMap<String, String> {
    held_in(&dir(), SystemTime::now())
}

fn held_in(dir: &Path, now: SystemTime) -> HashMap<String, String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return HashMap::new();
    };
    entries
        .flatten()
        .filter_map(|e| {
            let id = e.file_name().into_string().ok()?;
            if !is_pane(&id) {
                return None;
            }
            let at = e.metadata().ok()?.modified().ok()?;
            if now.duration_since(at).unwrap_or_default() >= HOLD {
                return None;
            }
            let text = std::fs::read_to_string(e.path()).unwrap_or_default();
            let row = text.trim_end_matches('\n');
            // Only ever this pane's own row: listing anything else in its place
            // would put a session where it is not.
            let row = if row.split('\t').next() == Some(id.as_str()) {
                row.to_string()
            } else {
                String::new()
            };
            Some((id, row))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("taimux-held-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    const ROW: &str = "%7\tw:1.1\t/h\tclaude\t2.1.291\tidle\t-\tapple pie\t1700000000000";

    #[test]
    fn a_held_row_reads_back_as_it_was_written() {
        let d = scratch("rw");
        write(&d, "%7", ROW).unwrap();
        let h = held_in(&d, SystemTime::now());
        assert_eq!(h.get("%7").map(String::as_str), Some(ROW));
        assert_eq!(h.len(), 1, "and the file it was written aside to is gone");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn dropping_the_hold_releases_the_row() {
        let d = scratch("drop");
        let h = Held(write(&d, "%7", ROW));
        assert!(held_in(&d, SystemTime::now()).contains_key("%7"));
        drop(h);
        assert!(held_in(&d, SystemTime::now()).is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A restart killed before it could clean up must not hold a row for ever.
    #[test]
    fn a_hold_older_than_a_restart_can_take_is_ignored() {
        let d = scratch("old");
        write(&d, "%7", ROW).unwrap();
        let later = SystemTime::now() + HOLD + Duration::from_secs(1);
        assert!(held_in(&d, later).is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Marked, so the picker can say so, but with no row to stand in for the
    /// pane's: a row naming another pane, or none at all, is not one.
    #[test]
    fn a_row_that_is_not_the_panes_own_is_not_held() {
        let d = scratch("other");
        write(&d, "%8", ROW).unwrap();
        write(&d, "%9", "").unwrap();
        let h = held_in(&d, SystemTime::now());
        assert_eq!(h.get("%8").map(String::as_str), Some(""));
        assert_eq!(h.get("%9").map(String::as_str), Some(""));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn only_a_pane_id_is_ever_a_file_name() {
        let d = scratch("names");
        assert!(write(&d, "../x", ROW).is_none());
        assert!(write(&d, "%", ROW).is_none());
        assert!(write(&d, "host:%7", ROW).is_none());
        assert!(held_in(&d, SystemTime::now()).is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn nothing_held_is_an_empty_answer_not_an_error() {
        assert!(held_in(&scratch("none"), SystemTime::now()).is_empty());
    }
}
