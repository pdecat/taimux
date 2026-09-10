//! What a session reports about itself.
//!
//! A port of taimux's `cmd_hook`, `_hook_agent_pid` and `_json_str`. This is the
//! single most repeated fork on the machine: Claude Code runs it at five turn
//! boundaries per turn, per session, and there are about 28 sessions. Replacing
//! `bash` plus `awk` plus its subshells with one static binary is the whole point.
//!
//! **It deliberately does not talk to the daemon.** The design note assumed it
//! would, but the work here is local and stateless: parse a small payload, walk a
//! few `/proc` entries, write one line. A socket round trip would add latency and
//! a second failure mode and buy nothing, and the hook must keep working when no
//! daemon is running. The win was never centralisation, it was not being bash.
//!
//! The logic is split so the decision is a pure function of its inputs: `decide`
//! takes what was read and returns what to do, and only `run` touches the disk.

use std::path::{Path, PathBuf};

/// Agents whose `comm` names them. A self-updated claude is the exception: it
/// runs from a binary named after its version, so `comm` reads `2.1.239`.
const AGENTS: [&str; 7] = [
    "claude",
    "codex",
    "opencode",
    "gemini",
    "antigravity",
    "agy",
    "pi",
];

fn is_agent_comm(comm: &str) -> bool {
    if AGENTS.contains(&comm) {
        return true;
    }
    // the bash glob is [0-9].[0-9]*: a digit, a dot, then a digit
    let b = comm.as_bytes();
    b.len() >= 3 && b[0].is_ascii_digit() && b[1] == b'.' && b[2].is_ascii_digit()
}

/// The value of a flat top-level string field, without a JSON parser.
///
/// The same approximation the bash version makes, and it holds for the same
/// reason: these payloads are flat objects of string fields, and jq is
/// deliberately not a dependency of a tool whose others are awk, bash and ps.
pub fn json_str(payload: &str, key: &str) -> Option<String> {
    crate::json::scan(payload, key)
}

/// The agent process this hook was spawned by, walking up from our parent.
///
/// Exactly one agent may appear in the chain. A second one above it means this is
/// a nested `claude -p`, which inherits `$TMUX_PANE` from the session that
/// launched it: letting that write would hand the pane's line to a throwaway
/// conversation, and letting it DELETE would wipe a good line on its way out.
pub fn agent_pid(
    start_ppid: i32,
    comm_of: &dyn Fn(i32) -> Option<String>,
    ppid_of: &dyn Fn(i32) -> Option<i32>,
) -> Option<i32> {
    let mut pid = start_ppid;
    let mut first: Option<i32> = None;
    for _ in 0..10 {
        if pid <= 1 {
            break;
        }
        if let Some(comm) = comm_of(pid) {
            if is_agent_comm(comm.trim()) {
                if first.is_some() {
                    return None; // nested: a second agent above the first
                }
                first = Some(pid);
            }
        }
        pid = ppid_of(pid)?;
    }
    first
}

/// What a hook event does to the pane's line.
#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    Write(String),
    Remove,
    Nothing,
}

/// The decision, as a pure function of what was read.
///
/// `prev` is the line already on disk, as (pid, state, mode).
pub fn decide(
    event: &str,
    payload_mode: Option<&str>,
    pid: i32,
    prev: Option<(i32, &str, &str)>,
) -> Action {
    match event {
        // Ending removes only OUR OWN line. The guard sits before every write,
        // removal included: a nested session ending would otherwise delete the
        // line of the session that launched it, which is how the first live test
        // of this wiped a perfectly good entry.
        "SessionEnd" => match prev {
            Some((ppid, _, _)) if ppid == pid => Action::Remove,
            _ => Action::Nothing,
        },
        "UserPromptSubmit" | "Stop" | "SessionStart" | "PermissionRequest" => {
            let state = match event {
                "UserPromptSubmit" => "run",
                "PermissionRequest" => "input",
                _ => "idle", // Stop and SessionStart both land at the prompt
            };
            // An event that carries no mode keeps whatever an earlier one knew,
            // but only if the line is about the same process.
            let mode = match payload_mode.filter(|m| !m.is_empty()) {
                Some(m) => m.to_string(),
                None => match prev {
                    Some((ppid, _, pmode)) if ppid == pid && !pmode.is_empty() => pmode.to_string(),
                    _ => "-".to_string(),
                },
            };
            Action::Write(format!("{}\t{}\t{}\n", pid, state, mode))
        }
        // An event nobody subscribed to changes nothing.
        _ => Action::Nothing,
    }
}

fn read_comm(pid: i32) -> Option<String> {
    std::fs::read_to_string(format!("/proc/{}/comm", pid)).ok()
}

fn read_ppid(pid: i32) -> Option<i32> {
    let status = std::fs::read_to_string(format!("/proc/{}/status", pid)).ok()?;
    status
        .lines()
        .find(|l| l.starts_with("PPid:"))
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|v| v.parse().ok())
}

/// Read the line already on disk as (pid, state, mode).
fn read_prev(f: &Path) -> Option<(i32, String, String)> {
    let text = std::fs::read_to_string(f).ok()?;
    let line = text.lines().next()?;
    let mut it = line.split('\t');
    let pid = it.next()?.parse().ok()?;
    let state = it.next().unwrap_or("").to_string();
    let mode = it.next().unwrap_or("").to_string();
    Some((pid, state, mode))
}

/// The whole hook. Returns 0 always: a hook that fails must never be visible to
/// the session it is reporting on.
pub fn run(runtime_dir: PathBuf) -> i32 {
    let Ok(pane) = std::env::var("TMUX_PANE") else {
        return 0; // not in tmux: nothing to key on
    };
    if pane.is_empty() {
        return 0;
    }
    let mut payload = String::new();
    {
        use std::io::Read;
        let _ = std::io::stdin().read_to_string(&mut payload);
    }
    let Some(event) = json_str(&payload, "hook_event_name") else {
        return 0;
    };

    let ppid = std::os::unix::process::parent_id() as i32;
    let Some(pid) = agent_pid(ppid, &read_comm, &read_ppid) else {
        return 0; // nested agent, or an unreadable chain
    };

    let f = runtime_dir.join(pane.trim_start_matches('%'));
    let prev = read_prev(&f);
    let prev_ref = prev.as_ref().map(|(p, s, m)| (*p, s.as_str(), m.as_str()));

    match decide(
        &event,
        json_str(&payload, "permission_mode").as_deref(),
        pid,
        prev_ref,
    ) {
        Action::Nothing => {}
        Action::Remove => {
            let _ = std::fs::remove_file(&f);
        }
        Action::Write(line) => {
            if std::fs::create_dir_all(&runtime_dir).is_ok() {
                let _ = std::fs::write(&f, line);
            }
        }
    }
    0
}

// ---- moved out of main.rs: reading back what the hook above wrote.
/// The hook line a session writes about itself: `<agent pid> \t <state> \t <mode>`.
///
/// The pid is what makes it worth reading. It names the process the line is
/// about, so a line left by an earlier session in that pane, or written by a
/// nested `claude -p` that inherited $TMUX_PANE, does not match and is ignored.
pub fn hook_entry(pane_id: &str, pid: i32) -> Option<(String, String)> {
    let f = crate::paths::runtime_dir().join(pane_id.trim_start_matches('%'));
    let text = std::fs::read_to_string(f).ok()?;
    let line = text.lines().next()?;
    let mut it = line.split('\t');
    let (hpid, hstate) = (it.next()?, it.next()?);
    if hpid.parse::<i32>().ok()? != pid || pid == 0 {
        return None;
    }
    let mode = it.next().filter(|m| !m.is_empty()).unwrap_or("-");
    Some((hstate.to_string(), mode.to_string()))
}

/// The state a session reported about itself, or None. Named so `restart` can
/// reach the same reading the list uses: one busy taxonomy in the tool, not two.
pub fn hook_state_of(pane_id: &str, pid: i32) -> Option<String> {
    hook_entry(pane_id, pid).map(|(st, _)| st)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_flat_string_field_is_read_out() {
        assert_eq!(
            json_str(
                r#"{"a":1,"hook_event_name":"Stop","b":"x"}"#,
                "hook_event_name"
            )
            .as_deref(),
            Some("Stop")
        );
        // spaces around the colon are no obstacle, nor are newlines
        assert_eq!(
            json_str(
                "{\"permission_mode\" :\n \"acceptEdits\"}",
                "permission_mode"
            )
            .as_deref(),
            Some("acceptEdits")
        );
        assert_eq!(json_str(r#"{"a":"b"}"#, "permission_mode"), None);
    }

    #[test]
    fn a_key_appearing_as_a_value_does_not_win() {
        // "hook_event_name" as someone else's value, then the real field
        let p = r#"{"note":"hook_event_name","hook_event_name":"Stop"}"#;
        assert_eq!(json_str(p, "hook_event_name").as_deref(), Some("Stop"));
    }

    fn chain<'a>(
        pairs: &'a [(i32, &'a str, i32)],
    ) -> (
        impl Fn(i32) -> Option<String> + 'a,
        impl Fn(i32) -> Option<i32> + 'a,
    ) {
        let comm = move |pid: i32| {
            pairs
                .iter()
                .find(|(p, _, _)| *p == pid)
                .map(|(_, c, _)| c.to_string())
        };
        let ppid = move |pid: i32| {
            pairs
                .iter()
                .find(|(p, _, _)| *p == pid)
                .map(|(_, _, pp)| *pp)
        };
        (comm, ppid)
    }

    #[test]
    fn the_agent_above_the_hook_is_found() {
        let pairs = [(10, "bash", 20), (20, "claude", 30), (30, "bash", 1)];
        let (c, p) = chain(&pairs);
        assert_eq!(agent_pid(10, &c, &p), Some(20));
    }

    #[test]
    fn a_self_updated_claude_is_named_after_its_version() {
        let pairs = [(10, "bash", 20), (20, "2.1.239", 1)];
        let (c, p) = chain(&pairs);
        assert_eq!(agent_pid(10, &c, &p), Some(20));
    }

    #[test]
    fn a_nested_agent_is_refused_rather_than_allowed_to_write() {
        // a `claude -p` inherits $TMUX_PANE from the session that launched it
        let pairs = [(10, "bash", 20), (20, "claude", 30), (30, "claude", 1)];
        let (c, p) = chain(&pairs);
        assert_eq!(agent_pid(10, &c, &p), None);
    }

    #[test]
    fn a_chain_with_no_agent_in_it_yields_nothing() {
        let pairs = [(10, "bash", 20), (20, "sshd", 1)];
        let (c, p) = chain(&pairs);
        assert_eq!(agent_pid(10, &c, &p), None);
    }

    #[test]
    fn each_event_maps_to_the_state_the_picker_shows() {
        assert_eq!(
            decide("UserPromptSubmit", Some("acceptEdits"), 42, None),
            Action::Write("42\trun\tacceptEdits\n".into())
        );
        assert_eq!(
            decide("PermissionRequest", Some("acceptEdits"), 42, None),
            Action::Write("42\tinput\tacceptEdits\n".into())
        );
        assert_eq!(
            decide("Stop", Some("acceptEdits"), 42, None),
            Action::Write("42\tidle\tacceptEdits\n".into())
        );
        assert_eq!(
            decide("SessionStart", None, 42, None),
            Action::Write("42\tidle\t-\n".into())
        );
    }

    #[test]
    fn an_event_nobody_subscribed_to_changes_nothing() {
        assert_eq!(
            decide("PostToolUse", Some("auto"), 42, None),
            Action::Nothing
        );
    }

    #[test]
    fn a_mode_an_earlier_event_knew_is_kept() {
        // no event announces a mode flip, so the last one seen has to persist
        assert_eq!(
            decide("Stop", None, 42, Some((42, "run", "acceptEdits"))),
            Action::Write("42\tidle\tacceptEdits\n".into())
        );
        // …but not from a line about a different process
        assert_eq!(
            decide("Stop", None, 42, Some((99, "run", "acceptEdits"))),
            Action::Write("42\tidle\t-\n".into())
        );
    }

    #[test]
    fn ending_removes_only_our_own_line() {
        assert_eq!(
            decide("SessionEnd", None, 42, Some((42, "idle", "-"))),
            Action::Remove
        );
        // the line belongs to the session that launched us: leave it alone
        assert_eq!(
            decide("SessionEnd", None, 42, Some((99, "idle", "-"))),
            Action::Nothing
        );
        assert_eq!(decide("SessionEnd", None, 42, None), Action::Nothing);
    }
}
