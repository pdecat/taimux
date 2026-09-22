//! What a session reports about itself.
//!
//! A port of the bash prototype's `cmd_hook`, `_hook_agent_pid` and `_json_str`. This is the
//! single most repeated fork on the machine: Claude Code runs it at the turn
//! boundaries AND after every tool call, per session, and there are about 28
//! sessions. Replacing `bash` plus `awk` plus its subshells with one static
//! binary is the whole point, and it is what makes the per-tool-call events
//! affordable: 685 µs an invocation, so a thirty-call turn spends 20 ms of CPU
//! over the minutes it runs for.
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

/// The fields of a hook payload the line depends on.
#[derive(Debug, Default, Clone, Copy)]
pub struct Payload<'a> {
    pub event: &'a str,
    /// `permission_mode`, which not every event carries.
    pub mode: Option<&'a str>,
    /// `agent_id`, present only when a SUBAGENT raised the event. Subagents run
    /// in the background by default since Claude Code 2.1.198, so their tool calls
    /// go on firing after the main thread's `Stop`.
    pub agent: Option<&'a str>,
    /// `notification_type`, on a `Notification`.
    pub notification: Option<&'a str>,
    /// On `Stop`: whether `background_tasks` listed anything still in flight.
    pub background: bool,
    /// `source` on a `SessionStart`: startup, resume, clear or compact.
    pub source: Option<&'a str>,
}

/// The line already on disk.
#[derive(Debug, Clone, Copy)]
pub struct Prev<'a> {
    pub pid: i32,
    pub state: &'a str,
    pub mode: &'a str,
    /// Who raised the dialog an `input` or `ask` line is about: a subagent's id,
    /// or empty for the main thread.
    pub who: &'a str,
}

/// The decision, as a pure function of what was read.
///
/// The line reads one of five states: `run`, `input` (a permission was asked
/// for, which in auto mode may be answered without anyone seeing it), `ask` (the
/// same, confirmed on screen: see `Notification` below), `idle`, and `bg` (the
/// turn is over but background work is still in flight and will wake the
/// session again).
pub fn decide(p: &Payload, pid: i32, prev: Option<&Prev>) -> Action {
    // Everything read off the old line is only good for the same process.
    let mine = prev.filter(|q| q.pid == pid);
    let mut who = "";
    let state = match p.event {
        // Ending removes only OUR OWN line. The guard sits before every write,
        // removal included: a nested session ending would otherwise delete the
        // line of the session that launched it, which is how the first live test
        // of this wiped a perfectly good entry.
        "SessionEnd" => {
            return match mine {
                Some(_) => Action::Remove,
                None => Action::Nothing,
            }
        }
        // A compaction runs INSIDE a turn as often as between two: auto-compact
        // fires when the context fills, and the turn carries on after it. Idle
        // is only right for the other kind, typed at a prompt, and that one
        // follows a `Stop` that already said so. So a compaction changes nothing,
        // and touches nothing, since the line's mtime is what the transcript is
        // measured against.
        "SessionStart"
            if p.source == Some("compact")
                && mine.is_some_and(|q| matches!(q.state, "run" | "input" | "ask" | "bg")) =>
        {
            return Action::Nothing;
        }
        // Both land at the prompt.
        "SessionStart" => "idle",
        "UserPromptSubmit" => "run",
        // A tool that has just run is a turn in flight, and it is the ONLY event
        // that lands after a permission was granted: granting fires nothing of
        // its own, so without this the line sits at `input` until the turn ends,
        // or forever if that turn is then interrupted. It doubles as the repair
        // for a turn whose opening `UserPromptSubmit` never arrived, which is a
        // thing that happens and has not been explained. `PreToolUse` cannot do
        // either job: it fires BEFORE `PermissionRequest`, so the state it wrote
        // would be overwritten by the one it is meant to clear.
        //
        // A SUBAGENT's tool says nothing about the main thread, which may have
        // ended its turn long ago: a background subagent fires these after the
        // `Stop`, and taking them at their word put a session back in the working
        // list while it sat at an empty prompt. The one thing such a call does
        // settle is a dialog that same subagent raised, since running is how a
        // granted permission shows.
        "PostToolUse" | "PostToolUseFailure" => match p.agent.filter(|a| !a.is_empty()) {
            None => "run",
            Some(a) => match mine {
                Some(q) if matches!(q.state, "input" | "ask") && q.who == a => "run",
                _ => return Action::Nothing,
            },
        },
        "PermissionRequest" => {
            who = p.agent.unwrap_or("");
            "input"
        }
        // `PermissionRequest` fires even when nothing is asked (auto mode answers
        // it unseen), so `input` alone cannot mean "waiting". Claude's own
        // notification can: `permission_prompt` goes out once a prompt has sat on
        // screen for about six seconds unanswered, and never for one auto mode
        // settled. It covers AskUserQuestion too (measured on 2.1.280), and an MCP
        // elicitation form is the same situation in another dialog. Every other
        // notification, `idle_prompt` included, changes nothing: that one fires a
        // minute after a turn ends whether or not background work is still in
        // flight, and never after an interrupt.
        "Notification" => match p.notification {
            Some("permission_prompt" | "elicitation_dialog" | "elicitation_url_dialog") => {
                who = mine
                    .filter(|q| matches!(q.state, "input" | "ask"))
                    .map(|q| q.who)
                    .unwrap_or("");
                "ask"
            }
            _ => return Action::Nothing,
        },
        // `background_tasks` is how Claude tells "done" from "paused until a
        // background shell or subagent reports back". The report arrives as a new
        // turn, with a `UserPromptSubmit` of its own, so `bg` is closed the same
        // way `idle` is.
        "Stop" if p.background => "bg",
        "Stop" => "idle",
        // A turn ended by an API error runs this INSTEAD of `Stop`, and left
        // unregistered it left the line at `run` until the next prompt: 44
        // minutes of "working" on one 529, found in a transcript.
        "StopFailure" => "idle",
        // An event nobody subscribed to changes nothing.
        _ => return Action::Nothing,
    };
    // An event that carries no mode keeps whatever an earlier one knew, but only
    // if the line is about the same process.
    let mode = match p.mode.filter(|m| !m.is_empty()) {
        Some(m) => m.to_string(),
        None => match mine {
            Some(q) if !q.mode.is_empty() => q.mode.to_string(),
            _ => "-".to_string(),
        },
    };
    Action::Write(if who.is_empty() {
        format!("{}\t{}\t{}\n", pid, state, mode)
    } else {
        format!("{}\t{}\t{}\t{}\n", pid, state, mode, who)
    })
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

/// Read the line already on disk as (pid, state, mode, who).
fn read_prev(f: &Path) -> Option<(i32, String, String, String)> {
    let text = std::fs::read_to_string(f).ok()?;
    let line = text.lines().next()?;
    let mut it = line.split('\t');
    let pid = it.next()?.parse().ok()?;
    let state = it.next().unwrap_or("").to_string();
    let mode = it.next().unwrap_or("").to_string();
    let who = it.next().unwrap_or("").to_string();
    Some((pid, state, mode, who))
}

/// The common fields of a payload, which Claude writes before the per-event
/// ones. A tool's own arguments come after them under `tool_input`, and are
/// somebody else's JSON: an MCP tool taking an `agent_id` argument would
/// otherwise make a main-thread call read as a subagent's.
fn head(payload: &str) -> &str {
    payload
        .find("\"tool_input\"")
        .map_or(payload, |i| &payload[..i])
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
    let prev_ref = prev.as_ref().map(|(p, s, m, w)| Prev {
        pid: *p,
        state: s.as_str(),
        mode: m.as_str(),
        who: w.as_str(),
    });
    // Each field is looked for only on the event that carries it: this runs
    // after every tool call, and a tool's response can be most of a megabyte.
    let common = head(&payload);
    let mode = json_str(common, "permission_mode");
    let agent = json_str(common, "agent_id");
    let notification = (event == "Notification")
        .then(|| json_str(&payload, "notification_type"))
        .flatten();
    let source = (event == "SessionStart")
        .then(|| json_str(&payload, "source"))
        .flatten();
    let p = Payload {
        event: &event,
        mode: mode.as_deref(),
        agent: agent.as_deref(),
        notification: notification.as_deref(),
        background: event == "Stop"
            && crate::json::array_has_items(&payload, "background_tasks") == Some(true),
        source: source.as_deref(),
    };

    match decide(&p, pid, prev_ref.as_ref()) {
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
/// A hook line as read back: `<agent pid> \t <state> \t <mode> [\t <who>]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub state: String,
    pub mode: String,
    /// When the line was written, in epoch milliseconds: what the transcript's
    /// own timestamps are measured against.
    pub at: i64,
}

/// The hook line a session wrote about itself, as it stands on disk.
///
/// The pid is what makes it worth reading. It names the process the line is
/// about, so a line left by an earlier session in that pane, or written by a
/// nested `claude -p` that inherited $TMUX_PANE, does not match and is ignored.
pub fn hook_entry(pane_id: &str, pid: i32) -> Option<Entry> {
    let f = crate::paths::runtime_dir().join(pane_id.trim_start_matches('%'));
    let text = std::fs::read_to_string(&f).ok()?;
    let line = text.lines().next()?;
    let mut it = line.split('\t');
    let (hpid, hstate) = (it.next()?, it.next()?);
    if hpid.parse::<i32>().ok()? != pid || pid == 0 {
        return None;
    }
    let mode = it.next().filter(|m| !m.is_empty()).unwrap_or("-");
    let at = std::fs::metadata(&f)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_millis() as i64);
    Some(Entry {
        state: hstate.to_string(),
        mode: mode.to_string(),
        at,
    })
}

/// The hook line, brought up to date by the session's transcript wherever the
/// transcript knows something newer. The one to read; `hook_entry` is the raw
/// line.
///
/// Mostly this is the interrupt, which fires no hook at all (see `turn`). The
/// transcript is found the way `conv` finds it for a restart, off the pane map
/// and the live argv, and costs one read of its last 64 KB.
pub fn current(pane_id: &str, pid: i32) -> Option<Entry> {
    let mut e = hook_entry(pane_id, pid)?;
    let cwd = std::fs::read_link(format!("/proc/{}/cwd", pid))
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    if let Some(r) = crate::conv::resolve_from_pane(pane_id, &cwd, pid) {
        let turn = crate::turn::last_event_in(&r.transcript);
        e.state = crate::state::correct(&e.state, e.at, turn).to_string();
    }
    Some(e)
}

/// The state a session reported about itself, or None. Named so `restart` can
/// reach the same reading the list uses: one busy taxonomy in the tool, not two.
pub fn hook_state_of(pane_id: &str, pid: i32) -> Option<String> {
    current(pane_id, pid).map(|e| e.state)
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

    /// A payload for one event, the rest defaulted.
    fn ev(event: &str) -> Payload<'_> {
        Payload {
            event,
            ..Default::default()
        }
    }

    fn prev<'a>(pid: i32, state: &'a str, mode: &'a str) -> Prev<'a> {
        Prev {
            pid,
            state,
            mode,
            who: "",
        }
    }

    fn write(s: &str) -> Action {
        Action::Write(s.into())
    }

    #[test]
    fn each_event_maps_to_the_state_the_picker_shows() {
        let with = |event, mode| Payload {
            mode: Some(mode),
            ..ev(event)
        };
        assert_eq!(
            decide(&with("UserPromptSubmit", "acceptEdits"), 42, None),
            write("42\trun\tacceptEdits\n")
        );
        assert_eq!(
            decide(&with("PermissionRequest", "acceptEdits"), 42, None),
            write("42\tinput\tacceptEdits\n")
        );
        assert_eq!(
            decide(&with("Stop", "acceptEdits"), 42, None),
            write("42\tidle\tacceptEdits\n")
        );
        assert_eq!(
            decide(&ev("SessionStart"), 42, None),
            write("42\tidle\t-\n")
        );
    }

    #[test]
    fn a_tool_that_ran_says_the_turn_is_still_going() {
        let tool = Payload {
            mode: Some("auto"),
            ..ev("PostToolUse")
        };
        assert_eq!(decide(&tool, 42, None), write("42\trun\tauto\n"));
        // The same, whichever way the tool ended.
        let failed = Payload {
            event: "PostToolUseFailure",
            ..tool
        };
        assert_eq!(decide(&failed, 42, None), write("42\trun\tauto\n"));
        // Which is what closes a permission the user granted: nothing else fires
        // after the dialog goes away, and the line would otherwise stay `input`.
        assert_eq!(
            decide(&ev("PostToolUse"), 42, Some(&prev(42, "input", "default"))),
            write("42\trun\tdefault\n")
        );
    }

    #[test]
    fn a_background_subagents_tools_leave_the_main_thread_alone() {
        // Measured on 2.1.280: the main thread's `Stop` at 23:35:35, then the
        // subagent's own Bash call at 23:35:57, carrying its `agent_id`. Taken at
        // its word that put the session back in the working list while it sat
        // at an empty prompt.
        let sub = Payload {
            agent: Some("ad1a32b042459363f"),
            ..ev("PostToolUse")
        };
        for state in ["idle", "bg", "run"] {
            assert_eq!(
                decide(&sub, 42, Some(&prev(42, state, "auto"))),
                Action::Nothing,
                "{state}"
            );
        }
        assert_eq!(decide(&sub, 42, None), Action::Nothing);
    }

    #[test]
    fn a_subagents_tool_settles_only_the_dialog_that_subagent_raised() {
        let asked = Payload {
            agent: Some("sub1"),
            mode: Some("default"),
            ..ev("PermissionRequest")
        };
        assert_eq!(
            decide(&asked, 42, None),
            write("42\tinput\tdefault\tsub1\n")
        );
        let theirs = Prev {
            who: "sub1",
            ..prev(42, "input", "default")
        };
        let ran = |agent| Payload {
            agent: Some(agent),
            ..ev("PostToolUse")
        };
        assert_eq!(
            decide(&ran("sub1"), 42, Some(&theirs)),
            write("42\trun\tdefault\n")
        );
        // another subagent's call says nothing about that dialog…
        assert_eq!(decide(&ran("sub2"), 42, Some(&theirs)), Action::Nothing);
        // …nor about one the main thread is holding up
        assert_eq!(
            decide(&ran("sub1"), 42, Some(&prev(42, "ask", "default"))),
            Action::Nothing
        );
    }

    #[test]
    fn claudes_own_notification_confirms_a_dialog_is_on_screen() {
        let shown = |kind| Payload {
            notification: Some(kind),
            ..ev("Notification")
        };
        // it carries no mode, and keeps the asker the request named
        let asked = Prev {
            who: "sub1",
            ..prev(42, "input", "auto")
        };
        assert_eq!(
            decide(&shown("permission_prompt"), 42, Some(&asked)),
            write("42\task\tauto\tsub1\n")
        );
        assert_eq!(
            decide(
                &shown("elicitation_dialog"),
                42,
                Some(&prev(42, "run", "auto"))
            ),
            write("42\task\tauto\n")
        );
        // A minute after a turn, background work or not, and never after an
        // interrupt: not something to hang a state on.
        for kind in ["idle_prompt", "auth_success", "agent_needs_input"] {
            assert_eq!(
                decide(&shown(kind), 42, Some(&prev(42, "bg", "auto"))),
                Action::Nothing,
                "{kind}"
            );
        }
    }

    #[test]
    fn a_turn_that_leaves_work_running_is_not_idle() {
        let stop = |background| Payload {
            background,
            mode: Some("auto"),
            ..ev("Stop")
        };
        assert_eq!(decide(&stop(true), 42, None), write("42\tbg\tauto\n"));
        assert_eq!(decide(&stop(false), 42, None), write("42\tidle\tauto\n"));
    }

    #[test]
    fn an_api_error_ends_the_turn_too() {
        // `StopFailure` runs INSTEAD of `Stop`
        assert_eq!(
            decide(&ev("StopFailure"), 42, Some(&prev(42, "run", "auto"))),
            write("42\tidle\tauto\n")
        );
    }

    #[test]
    fn a_compaction_inside_a_turn_leaves_the_line_alone() {
        let compact = Payload {
            source: Some("compact"),
            ..ev("SessionStart")
        };
        for state in ["run", "input", "ask", "bg"] {
            assert_eq!(
                decide(&compact, 42, Some(&prev(42, state, "auto"))),
                Action::Nothing,
                "{state}"
            );
        }
        // typed at the prompt, after the `Stop` that already said idle
        assert_eq!(
            decide(&compact, 42, Some(&prev(42, "idle", "auto"))),
            write("42\tidle\tauto\n")
        );
        // and a line about some other process is no guide at all
        assert_eq!(
            decide(&compact, 42, Some(&prev(99, "run", "auto"))),
            write("42\tidle\t-\n")
        );
    }

    #[test]
    fn an_event_nobody_subscribed_to_changes_nothing() {
        // `SubagentStop` deliberately: it fires while the main turn carries on,
        // so writing `idle` from it would be wrong, and `run` would only repeat
        // what the tool events already said.
        for event in ["SubagentStop", "PreToolUse", "PermissionDenied"] {
            let p = Payload {
                mode: Some("auto"),
                ..ev(event)
            };
            assert_eq!(decide(&p, 42, None), Action::Nothing, "{event}");
        }
    }

    #[test]
    fn a_mode_an_earlier_event_knew_is_kept() {
        // no event announces a mode flip, so the last one seen has to persist
        assert_eq!(
            decide(&ev("Stop"), 42, Some(&prev(42, "run", "acceptEdits"))),
            write("42\tidle\tacceptEdits\n")
        );
        // …but not from a line about a different process
        assert_eq!(
            decide(&ev("Stop"), 42, Some(&prev(99, "run", "acceptEdits"))),
            write("42\tidle\t-\n")
        );
    }

    #[test]
    fn ending_removes_only_our_own_line() {
        let end = ev("SessionEnd");
        assert_eq!(
            decide(&end, 42, Some(&prev(42, "idle", "-"))),
            Action::Remove
        );
        // the line belongs to the session that launched us: leave it alone
        assert_eq!(
            decide(&end, 42, Some(&prev(99, "idle", "-"))),
            Action::Nothing
        );
        assert_eq!(decide(&end, 42, None), Action::Nothing);
    }

    #[test]
    fn the_common_fields_are_read_before_a_tools_own_arguments() {
        // A main-thread call to a tool that takes an `agent_id` argument of its
        // own: the payload's real `agent_id` is absent, and the tool's is not it.
        let p = r#"{"session_id":"s","permission_mode":"auto","hook_event_name":"PostToolUse","tool_name":"mcp__x__y","tool_input":{"agent_id":"not-a-subagent"}}"#;
        assert_eq!(json_str(head(p), "agent_id"), None);
        assert_eq!(
            json_str(head(p), "permission_mode").as_deref(),
            Some("auto")
        );
        let sub = r#"{"session_id":"s","agent_id":"a1","agent_type":"general-purpose","hook_event_name":"PostToolUse","tool_input":{}}"#;
        assert_eq!(json_str(head(sub), "agent_id").as_deref(), Some("a1"));
    }
}
