//! The pane scan, without forking.
//!
//! The bash version pays `ps -eo tty=,pid=,pgid=,tpgid=,args=` plus an awk pass
//! for every list build, and a list build happens on a timer for every open
//! picker. Reading `/proc` directly costs no process at all, which is most of
//! the point of having a daemon.

use std::fs;
use std::path::Path;

/// One process that owns its terminal: it is in the foreground process group of
/// some tty, which is what makes it "the thing in front" of a tmux pane.
#[derive(Debug, Clone, PartialEq)]
pub struct Foreground {
    pub tty: String,
    pub pid: i32,
    pub argv: String,
}

/// `pts/N` for a tty, or None for anything that is not a pseudo-terminal.
///
/// `tty_nr` in `/proc/<pid>/stat` is the kernel's `new_encode_dev`, not a device
/// number you can print: the minor is SPLIT, its low byte at bits 0-7 and the
/// rest at bits 20+, with the major wedged between them at bits 8-19. Reading it
/// as a plain number, or assuming the minor is contiguous, gives the wrong tty
/// for any pts above 255, which is a machine with a lot of panes open. Only major
/// 136 (UNIX98 pts) matters here, since a tmux pane is always a pts, and 0 means
/// the process has no controlling terminal at all.
pub fn tty_name(tty_nr: i64) -> Option<String> {
    if tty_nr == 0 {
        return None;
    }
    let major = (tty_nr >> 8) & 0xfff;
    let minor = (tty_nr & 0xff) | ((tty_nr >> 12) & 0xfff00);
    if major == 136 {
        Some(format!("pts/{}", minor))
    } else {
        None
    }
}

/// The fields of `/proc/<pid>/stat` this needs: (pgrp, tty_nr, tpgid).
///
/// Field 2 is the executable name in parentheses and it may contain both spaces
/// and parentheses, so the only safe split is at the LAST `)`. Splitting on
/// whitespace from the left is the classic way to misparse this file.
pub fn parse_stat(stat: &str) -> Option<(i64, i64, i64)> {
    let close = stat.rfind(')')?;
    let rest = stat.get(close + 1..)?;
    let f: Vec<&str> = rest.split_whitespace().collect();
    // After the comm field `rest` starts at field 3 (state), so the fields wanted
    // (pgrp 5, tty_nr 7, tpgid 8) sit at offsets 2, 4 and 5.
    let pgrp = f.get(2)?.parse().ok()?;
    let tty_nr = f.get(4)?.parse().ok()?;
    let tpgid = f.get(5)?.parse().ok()?;
    Some((pgrp, tty_nr, tpgid))
}

/// argv as one space-joined line, tabs flattened so it survives a TSV row.
pub fn read_argv(pid: i32) -> Option<String> {
    let raw = fs::read(format!("/proc/{}/cmdline", pid)).ok()?;
    if raw.is_empty() {
        return None;
    }
    let joined: Vec<String> = raw
        .split(|b| *b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).replace('\t', " "))
        .collect();
    if joined.is_empty() {
        None
    } else {
        Some(joined.join(" "))
    }
}

/// Every process in the foreground group of a pts, which is the same set the
/// bash version selects with `$3 == $4` on pgid and tpgid.
///
/// The whole group is kept rather than just its leader: a resident launcher
/// (rses, npx, npm, uv) can lead the group while the agent runs as its child, so
/// keying off the leader alone would miss the agent entirely.
pub fn foreground_map() -> Vec<Foreground> {
    let mut out = Vec::new();
    let dir = match fs::read_dir("/proc") {
        Ok(d) => d,
        Err(_) => return out,
    };
    for entry in dir.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let pid: i32 = match name.parse() {
            Ok(p) => p,
            Err(_) => continue,
        };
        let stat = match fs::read_to_string(Path::new("/proc").join(&*name).join("stat")) {
            Ok(s) => s,
            Err(_) => continue, // it exited between readdir and here, which is normal
        };
        let (pgrp, tty_nr, tpgid) = match parse_stat(&stat) {
            Some(v) => v,
            None => continue,
        };
        if pgrp != tpgid {
            continue; // not the foreground group of its terminal
        }
        let tty = match tty_name(tty_nr) {
            Some(t) => t,
            None => continue,
        };
        if let Some(argv) = read_argv(pid) {
            out.push(Foreground { tty, pid, argv });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The kernel's own new_encode_dev, so the tests build what /proc really
    /// prints instead of asserting against my reading of it.
    fn encode(major: i64, minor: i64) -> i64 {
        (minor & 0xff) | (major << 8) | ((minor & !0xff) << 12)
    }

    #[test]
    fn pts_devices_decode_to_their_name() {
        assert_eq!(tty_name(encode(136, 110)), Some("pts/110".into()));
        assert_eq!(tty_name(encode(136, 0)), Some("pts/0".into()));
    }

    #[test]
    fn a_minor_above_255_survives_the_split_encoding() {
        // the case that catches reading tty_nr as a contiguous number
        assert_eq!(tty_name(encode(136, 261)), Some("pts/261".into()));
        assert_eq!(tty_name(encode(136, 4095)), Some("pts/4095".into()));
    }

    #[test]
    fn a_process_with_no_terminal_has_no_tty() {
        assert_eq!(tty_name(0), None);
    }

    #[test]
    fn non_pts_terminals_are_ignored() {
        assert_eq!(tty_name((4 << 8) | 1), None); // tty1, a real console
    }

    #[test]
    fn stat_is_parsed_past_the_comm_field() {
        // pid comm state ppid pgrp session tty_nr tpgid ...
        let s = "1234 (bash) S 1 1234 1234 34816 1234 4194304 0 0";
        assert_eq!(parse_stat(s), Some((1234, 34816, 1234)));
    }

    #[test]
    fn a_comm_containing_spaces_and_parens_does_not_break_it() {
        // This is why the split is at the LAST ')' and not on whitespace.
        let s = "77 (weird ) name) S 1 99 99 34816 42 0";
        assert_eq!(parse_stat(s), Some((99, 34816, 42)));
    }

    #[test]
    fn a_truncated_stat_line_is_refused_rather_than_guessed() {
        assert_eq!(parse_stat("1234 (bash) S 1"), None);
        assert_eq!(parse_stat("no parens here"), None);
    }
}

/// The direct children of a pid.
///
/// A `/proc` walk rather than `pgrep -P`, which is what the bash restart paid per
/// poll while waiting for a session to come back: forty polls, half a second
/// apart, per pane.
pub fn children_of(parent: i32) -> Vec<i32> {
    let mut out = Vec::new();
    if parent <= 0 {
        return out;
    }
    for e in std::fs::read_dir("/proc").into_iter().flatten().flatten() {
        let name = e.file_name();
        let Some(pid) = name.to_str().and_then(|s| s.parse::<i32>().ok()) else {
            continue;
        };
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{}/stat", pid)) else {
            continue;
        };
        // The comm field can hold spaces and parens, so the fields after it are
        // counted from the LAST ')'. Same rule as parse_stat above, and the same
        // reason it exists.
        let Some(close) = stat.rfind(')') else {
            continue;
        };
        let after: Vec<&str> = stat[close + 1..].split_whitespace().collect();
        // state, ppid, ...
        if after.get(1).and_then(|p| p.parse::<i32>().ok()) == Some(parent) {
            out.push(pid);
        }
    }
    out.sort_unstable();
    out
}

#[cfg(test)]
mod children_tests {
    use super::*;

    /// This process's parent has this process as a child, which is the only
    /// assertion available without spawning something.
    #[test]
    fn a_process_is_its_parents_child() {
        let me = std::process::id() as i32;
        let stat = std::fs::read_to_string(format!("/proc/{}/stat", me)).expect("stat");
        let close = stat.rfind(')').expect("comm");
        let ppid: i32 = stat[close + 1..]
            .split_whitespace()
            .nth(1)
            .and_then(|p| p.parse().ok())
            .expect("ppid");
        assert!(children_of(ppid).contains(&me));
    }

    #[test]
    fn nothing_is_a_child_of_a_nonsense_pid() {
        assert!(children_of(0).is_empty());
        assert!(children_of(-1).is_empty());
    }
}
