//! Driving tmux: switching the client, and rendering a pane for the preview.
//!
//! Small, and mostly a matter of getting the command lines exactly right, which
//! is why every one of them is built from a slice
//! rather than a formatted string: a pane id, a directory or a transcript path
//! goes in as one argument and can never be re-split by a shell, because there is
//! no shell in the path at all.
//!
//! That is the one real gain here beyond the fork. `_switch_dead` had to
//! `printf '%q'` its transcript path because the whole command was handed to a
//! shell by `tmux new-window`; the shell is still there for the claude it starts,
//! but the path now reaches it as an argument rather than as text to re-parse.

use crate::env;
use std::process::Command;

/// Run tmux and hand back stdout, trimmed of its trailing newline. `None` when
/// tmux refuses, which for a pane id usually means the pane has gone.
pub fn ask(args: &[&str]) -> Option<String> {
    let out = Command::new("tmux").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(
        String::from_utf8_lossy(&out.stdout)
            .trim_end_matches('\n')
            .to_string(),
    )
}

/// Run tmux for its effect. Failure is not worth reporting on most of these: a
/// pane that has gone between the list and the keypress is an ordinary race, and
/// the switch below checks the pane exists first anyway.
pub fn run(args: &[&str]) -> bool {
    Command::new("tmux")
        .args(args)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// tmux's stdout exactly as it came, trailing newlines and all.
///
/// `ask` trims them, which is right for a one-line answer and WRONG for a
/// capture: `capture-pane` pads its output to the pane height, and those blank
/// lines are part of the picture. Trimming them cost the preview a line against
/// the bash version, which piped the capture straight through and so kept them.
pub fn ask_raw(args: &[&str]) -> Option<String> {
    let out = Command::new("tmux").args(args).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// One pane's screen WITH its colours, which is what a preview of a live agent
/// session has to show. The plain `-p` the list uses would draw it all grey.
pub fn capture_coloured(pane: &str) -> Option<String> {
    ask_raw(&["capture-pane", "-ep", "-t", pane])
}

/// Move the attached client to a pane, and zoom it.
///
/// The pane's session is asked for first, and a pane that cannot answer is a
/// refusal rather than a best effort: switching a client to a session that is not
/// there leaves it somewhere nobody chose.
///
/// `resize-pane -Z` TOGGLES, so a pane that is already zoomed must be left alone
/// or the zoom is undone by the very key that asked for it.
pub fn switch_local(pane: &str, zoom: bool) -> bool {
    let Some(sess) = ask(&["display-message", "-p", "-t", pane, "#{session_name}"]) else {
        return false;
    };
    if sess.is_empty() {
        return false;
    }
    run(&["select-window", "-t", pane]);
    run(&["select-pane", "-t", pane]);
    if zoom {
        let zoomed = ask(&["display-message", "-p", "-t", pane, "#{window_zoomed_flag}"]);
        if zoomed.as_deref() != Some("1") {
            run(&["resize-pane", "-Z", "-t", pane]);
        }
    }
    run(&["switch-client", "-t", &sess])
}

/// Open a past conversation again, in its own tool, in the directory it ran in.
///
/// **Through `command`**, so no shell alias fires and tmux-resurrect can read the
/// pane's argv back later. And it refuses when the directory has gone rather than
/// falling back to `$HOME`: a session resumed in the wrong place writes its
/// history into a different project, silently.
///
/// Each agent supplies its own command, and one of them supplies a refusal
/// instead: Gemini's CLI cannot be told which session to resume. That refusal is
/// carried through to the keypress rather than swallowed, because a key that
/// looks like it did nothing is the worst outcome available here.
pub fn resume_dead(agent: &str, key: &str, cwd: &str) -> Result<(), String> {
    // A file-backed session has to still be on disk; a database-backed one has
    // no file to check and its store answered a moment ago.
    if key.starts_with('/') && !std::path::Path::new(key).is_file() {
        return Err("that conversation is no longer on disk".into());
    }
    let cmd = crate::agents::resume(agent, key)?;
    if cwd.is_empty() || !std::path::Path::new(cwd).is_dir() {
        return Err(format!(
            "{} is gone, so there is nowhere to resume it",
            if cwd.is_empty() { "its directory" } else { cwd }
        ));
    }
    // tmux hands this string to a shell, and everything variable in it was
    // quoted once, by the agent that built it.
    if run(&["new-window", "-c", cwd, &cmd]) {
        Ok(())
    } else {
        Err("tmux would not open a window".into())
    }
}

/// The directory a past conversation ran in, off the sessions cache.
pub fn past_cwd(agent: &str, key: &str) -> String {
    crate::index::sessions()
        .into_iter()
        .find(|e| e.agent == agent && e.key == key)
        .map(|e| e.cwd)
        .unwrap_or_default()
}

/// Single-quote a string for the shell tmux will hand it to, the way bash's
/// `printf %q` does for anything awkward: wrap it, and end-quote around each
/// embedded quote.
pub fn shell_quote(s: &str) -> String {
    if !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"@%+=:,./-_".contains(&b))
    {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// The rule under the preview's header. Forty-four dashes, which is what the
/// bash version drew, and what every golden comparison of this output expects.
const RULE: &str = "────────────────────────────────────────────";

/// Where the words you typed turn up in what this session actually SAID, drawn
/// for a terminal rather than for the picker: the term itself in reverse video,
/// since whoever renders this has no say over the row.
fn match_block(pane: &str, query: &str, want: usize) -> String {
    if query.chars().count() < 3 {
        return String::new();
    }
    let hits = crate::index::preview_hits(pane, &crate::index::Query::new(query), want);
    if hits.is_empty() {
        return String::new();
    }
    let mut s = String::new();
    for h in &hits {
        s.push_str(&format!(
            "\x1b[90m{}{}\x1b[0m\x1b[7m{}\x1b[0m\x1b[90m{}{}\x1b[0m\n",
            if h.cut_left { "…" } else { "" },
            h.before,
            h.term,
            h.after,
            if h.cut_right { "…" } else { "" }
        ));
    }
    s.push_str("\x1b[90m──────────────── ⌕ ────────────────\x1b[0m\n\n");
    s
}

/// A live pane's screen, under a header saying exactly where it is.
///
/// The last hundred lines, because what a session is doing is at the bottom of
/// it, and because a preview pane is short.
pub fn preview_live(pane: &str, query: &str) -> String {
    let mut s = match_block(pane, query, preview_want());
    let hdr = ask(&[
        "display-message",
        "-p",
        "-t",
        pane,
        "#{session_name}:#{window_index}.#{pane_index}   #{pane_current_path}",
    ])
    .unwrap_or_default();
    s.push_str(&format!("\x1b[1;36m{}\x1b[0m\n", hdr));
    s.push_str(&format!("\x1b[90m{}\x1b[0m\n\n", RULE));
    let screen = capture_coloured(pane).unwrap_or_default();
    let lines: Vec<&str> = screen.lines().collect();
    let from = lines.len().saturating_sub(100);
    for l in &lines[from..] {
        s.push_str(l);
        s.push('\n');
    }
    s
}

fn preview_want() -> usize {
    env::var("TAIMUX_SEARCH_PREVIEW")
        .and_then(|v| v.parse().ok())
        .unwrap_or(4)
}

/// A PAST conversation has no screen to capture: what it has is the last things
/// that were said in it, which is what tells you whether it is the one you were
/// looking for.
pub fn preview_dead(id: &str, query: &str) -> String {
    let mut s = match_block(id, query, preview_want());
    if id == "dead:!" {
        s.push_str("\x1b[90mthe list is still being built\x1b[0m\n");
        return s;
    }
    let Some((agent, key)) = crate::index::split_past_id(id) else {
        s.push_str("\x1b[31mthat row does not name a conversation\x1b[0m\n");
        return s;
    };
    let meta = crate::index::sessions()
        .into_iter()
        .find(|e| e.agent == agent && e.key == key);
    let cwd = meta.as_ref().map(|m| m.cwd.clone()).unwrap_or_default();
    let ver = meta.as_ref().map(|m| m.version.clone()).unwrap_or_default();
    s.push_str(&format!(
        "\x1b[1;36m{}\x1b[0m  \x1b[2m{}\x1b[0m\n",
        if cwd.is_empty() { "?" } else { &cwd },
        if ver.is_empty() {
            agent.to_string()
        } else {
            format!("{} {}", agent, ver)
        }
    ));
    s.push_str(&format!("\x1b[90m{}\x1b[0m\n\n", RULE));

    let want = env::var("TAIMUX_DEAD_TURNS")
        .and_then(|v| v.parse().ok())
        .unwrap_or(6);
    let cols: usize = env::var("TAIMUX_PREVIEW_COLUMNS")
        .and_then(|v| v.parse().ok())
        .unwrap_or(100);
    let turns = crate::agents::turns(agent, key, want);
    if turns.is_empty() {
        if key.starts_with('/') && !std::path::Path::new(key).is_file() {
            s.push_str("\x1b[31mthis conversation is no longer on disk\x1b[0m\n");
        } else {
            s.push_str("\x1b[90m(nothing was said in this one)\x1b[0m\n");
        }
    }
    // Two lines a turn is enough to recognise one, and the preview pane is
    // short. A turn cut off says so rather than pretending it ended there.
    let cap = (cols * 2).saturating_sub(2);
    for t in turns {
        let what = if t.text.chars().count() > cap {
            format!("{}…", t.text.chars().take(cap).collect::<String>())
        } else {
            t.text
        };
        s.push_str(&format!(
            "\x1b[{}m{}\x1b[0m {}\n\n",
            if t.you { "1;36" } else { "2" },
            if t.you { "❯" } else { " " },
            what
        ));
    }
    s
}

// ---- moved out of main.rs: these are tmux calls, and main.rs is a dispatcher.

/// One pane's screen. The single most repeated fork in a list build: 26 panes,
/// ~2ms each, on a timer, for every open picker. Caching it here is most of why
/// the daemon exists.
pub fn capture(pane_id: &str) -> Option<String> {
    let out = crate::stat::capture(|| {
        Command::new("tmux")
            .args(["capture-pane", "-p", "-t", pane_id])
            .output()
    })
    .ok()?;
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_path_needs_no_quoting() {
        assert_eq!(
            shell_quote("/home/p/.claude/a-b_c.jsonl"),
            "/home/p/.claude/a-b_c.jsonl"
        );
    }

    #[test]
    fn a_space_or_a_quote_is_wrapped() {
        assert_eq!(shell_quote("/a b/c.jsonl"), "'/a b/c.jsonl'");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
        assert_eq!(shell_quote(""), "''");
    }

    /// The characters that would let a path become more than one argument, or a
    /// command of its own. A transcript path is attacker-controlled only in the
    /// sense that a project directory can be named anything.
    #[test]
    fn anything_a_shell_would_act_on_is_quoted() {
        for s in [
            "a;b", "a|b", "a$b", "a`b`", "a&b", "a>b", "a\nb", "a*b", "~a",
        ] {
            let q = shell_quote(s);
            assert!(q.starts_with('\''), "{} was left bare as {}", s, q);
        }
    }

    #[test]
    fn resuming_refuses_a_transcript_that_has_gone() {
        let e = resume_dead("claude", "/nowhere/at/all.jsonl", "/tmp").expect_err("refused");
        assert!(e.contains("no longer on disk"));
    }

    /// A directory that has gone is a refusal, not a fallback to $HOME.
    #[test]
    fn resuming_refuses_a_directory_that_has_gone() {
        let d = std::env::temp_dir().join(format!("jmtx{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let tr = d.join("t.jsonl");
        std::fs::write(&tr, "").unwrap();
        let e =
            resume_dead("claude", &tr.to_string_lossy(), "/nowhere/at/all").expect_err("refused");
        assert!(e.contains("/nowhere/at/all"));
        let e = resume_dead("claude", &tr.to_string_lossy(), "").expect_err("refused");
        assert!(e.contains("its directory"));
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A conversation with nothing to resume BY refuses before the directory is
    /// judged, because the refusal is about the conversation and saying "its
    /// directory is gone" would send you looking in the wrong place.
    #[test]
    fn a_conversation_with_no_id_refuses_before_the_directory_is_judged() {
        let d = std::env::temp_dir().join(format!("jmtxg{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        // An agy transcript with no conversation directory above it: there is no
        // `--conversation` argument to be had.
        let tr = d.join("transcript.jsonl");
        std::fs::write(&tr, "").unwrap();
        let e = resume_dead("agy", &tr.to_string_lossy(), "/nowhere/at/all").expect_err("refused");
        assert!(e.contains("no id to resume by"), "{e}");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A row id carries the agent AND the key, and the key can hold colons of
    /// its own, so it splits once.
    #[test]
    fn a_past_row_id_round_trips() {
        let id = crate::index::past_id("claude", "/a/b:c.jsonl");
        assert_eq!(id, "dead:claude:/a/b:c.jsonl");
        assert_eq!(
            crate::index::split_past_id(&id),
            Some(("claude", "/a/b:c.jsonl"))
        );
        assert_eq!(crate::index::split_past_id("%7"), None);
        assert_eq!(crate::index::split_past_id("dead:!"), None);
    }
}
