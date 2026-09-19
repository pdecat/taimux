//! Where each agent keeps its past conversations, and how to read one.
//!
//! Everything else in taimux lists **panes**, so everything else can only find a
//! session that is still running. This module is the other half: the
//! conversations on disk, whoever wrote them, so a session you closed on Tuesday
//! is still findable on Thursday.
//!
//! Five stores, three shapes:
//!
//! | agent | where | shape |
//! |---|---|---|
//! | claude | `~/.claude/projects/<proj>/<uuid>.jsonl` | JSONL, one file per session |
//! | codex | `~/.codex/sessions/**/rollout-*.jsonl` | JSONL, two schemas |
//! | gemini | `~/.gemini/tmp/<proj>/chats/session-*.jsonl` | JSONL, with rewinds |
//! | agy | `~/.gemini/antigravity-cli/brain/<id>/…/transcript.jsonl` | JSONL |
//! | opencode | `~/.local/share/opencode/opencode.db` | SQLite |
//!
//! Each answers the same five questions, and the callers (the indexer, the row
//! builder, the preview, the handoff) ask them without knowing which agent they
//! are talking to:
//!
//! - **`discover`**: which conversations exist, and when each last said anything.
//! - **`meta`**: the directory, the version and the title a row shows.
//! - **`prose`**: the searchable text, incrementally where the store allows it.
//! - **`turns`**: the last few things said, for the preview and the handoff.
//! - **`resume`**: the command that opens it again.
//!
//! **A session is keyed by whatever its own tool calls it**: a transcript path
//! for the four file-backed agents, a session id for OpenCode. That is the
//! string `resume` has to hand back to the tool, so making up a second identity
//! for it would only mean translating back.
//!
//! **Only claude can be told apart from a live pane.** Which conversation a pane
//! is on is something the agent has to publish, and claude is the only one that
//! does (see `conv`). So a claude session that is open somewhere is excluded
//! from this list, and one belonging to any other agent is not: it is in the
//! history whether or not you are in it right now. That is why the list is
//! called past sessions rather than ended ones.

pub mod agy;
pub mod claude;
pub mod codex;
pub mod gemini;
pub mod opencode;

use std::path::Path;

/// Every agent whose history taimux can read, in the order a list ties break in.
pub const KNOWN: [&str; 5] = ["claude", "codex", "gemini", "opencode", "agy"];

/// One past conversation: which tool wrote it, what that tool calls it, and when
/// it last said anything.
#[derive(Debug, Clone, PartialEq)]
pub struct Past {
    pub agent: &'static str,
    pub key: String,
    pub mtime: i64,
    /// What a row says about it, when the store handed that over for free.
    ///
    /// A file-backed agent leaves this `None`: reading a title out of a
    /// transcript means opening it, which is exactly the cost the cache exists
    /// to avoid paying twice. A database hands back every column of the row it
    /// was already reading, so asking it again would be one query per session
    /// for something it already said.
    pub meta: Option<Meta>,
}

/// What a row says about a conversation.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Meta {
    pub cwd: String,
    /// The agent version the session recorded. Only claude records one.
    pub version: String,
    pub title: String,
    /// `t` for a title the session recorded, `p` for a prompt standing in for
    /// one. A live pane may borrow a real title but never a prompt.
    pub src: &'static str,
}

/// A conversation's searchable text, and what to do with it.
pub struct Prose {
    /// What "unchanged since last time" means for this store: a byte length for
    /// a file, a timestamp for a row in a database. Opaque to the caller, which
    /// only ever compares it with the one it kept.
    pub fingerprint: u64,
    pub text: String,
    /// `true` when `text` is the WHOLE conversation and replaces what was known;
    /// `false` when it is only what the store has gained since `from`.
    pub whole: bool,
}

/// One thing that was said.
#[derive(Debug, Clone, PartialEq)]
pub struct Turn {
    /// Whose turn it was. The agent's name is the caller's to supply, since a
    /// turn does not know which tool it belongs to.
    pub you: bool,
    pub text: String,
}

/// Every conversation every agent has, unsorted.
///
/// Uncapped on purpose. The bound that used to be here (the newest 200) was
/// invisible from the picker and cost exactly the sessions you go looking for:
/// a month-old conversation is the one you cannot find any other way, and it is
/// also the one a recency cap drops first. What makes it affordable is that a
/// conversation which has not been touched since the last pass is read from the
/// cache rather than from disk, so the cost of a pass is the directory walk plus
/// whatever has actually changed.
pub fn discover() -> Vec<Past> {
    let mut out = Vec::new();
    claude::discover(&mut out);
    codex::discover(&mut out);
    gemini::discover(&mut out);
    opencode::discover(&mut out);
    agy::discover(&mut out);
    out
}

/// The directory, version and title for one conversation.
pub fn meta(agent: &str, key: &str) -> Meta {
    match agent {
        "claude" => claude::meta(key),
        "codex" => codex::meta(key),
        "gemini" => gemini::meta(key),
        "opencode" => opencode::meta(key),
        "agy" => agy::meta(key),
        _ => Meta::default(),
    }
}

/// What "unchanged since the last pass" means for one conversation, asked as
/// cheaply as the store allows.
///
/// This exists because reading it to find out is what the fingerprint was
/// supposed to avoid. A pass over 820 conversations that opens each one to learn
/// it has nothing new costs a gigabyte of reads and fourteen seconds; the same
/// pass asking this first costs 820 `stat` calls and a hundred indexed lookups.
/// Measured, both ways, on this machine.
pub fn fingerprint(agent: &str, key: &str) -> Option<u64> {
    match agent {
        "opencode" => opencode::fingerprint(key),
        _ => std::fs::metadata(key).ok().map(|m| m.len()),
    }
}

/// The searchable prose of one conversation, from `from` onwards where the store
/// can do that.
pub fn prose(agent: &str, key: &str, from: u64) -> Option<Prose> {
    match agent {
        "claude" => claude::prose(key, from),
        "codex" => codex::prose(key, from),
        "gemini" => gemini::prose(key, from),
        "opencode" => opencode::prose(key),
        "agy" => agy::prose(key, from),
        _ => None,
    }
}

/// The last `want` turns, oldest first.
pub fn turns(agent: &str, key: &str, want: usize) -> Vec<Turn> {
    match agent {
        "claude" => claude::turns(key, want),
        "codex" => codex::turns(key, want),
        "gemini" => gemini::turns(key, want),
        "opencode" => opencode::turns(key, want),
        "agy" => agy::turns(key, want),
        _ => Vec::new(),
    }
}

/// The command that opens this conversation again, or why it cannot be.
///
/// `Err` rather than `None` because a key that visibly does nothing is the one
/// outcome worth avoiding: a tool with no resume-by-id has to say so.
pub fn resume(agent: &str, key: &str) -> Result<String, String> {
    match agent {
        "claude" => Ok(claude::resume(key)),
        "codex" => codex::resume(key),
        "gemini" => gemini::resume(key),
        "opencode" => Ok(opencode::resume(key)),
        "agy" => agy::resume(key),
        _ => Err(format!(
            "taimux does not know how to resume a {} session",
            agent
        )),
    }
}

/// The transcript file behind a conversation, where there is one.
///
/// The handoff points the receiving model at it, so it can read the whole
/// history rather than only the turns that fitted in a prompt. OpenCode keeps
/// its conversation in a database and so has none.
pub fn transcript(agent: &str, key: &str) -> Option<String> {
    match agent {
        "opencode" => None,
        _ if Path::new(key).is_file() => Some(key.to_string()),
        _ => None,
    }
}

// ── Shared reading ──────────────────────────────────────────────────────────

/// The last `window` bytes of a file, from the first line boundary inside them.
///
/// The preview and the title fallback both want the END of a conversation, and
/// a claude transcript here reaches 23 MB. Reading the whole thing to show three
/// lines is what made the ended list feel like it was thinking. A window at or
/// past the file's size reads it whole, so a small file is one read either way.
///
/// The second return says whether the window covered the whole file, which is
/// what lets a caller widen it rather than guess.
pub fn tail_lines(path: &Path, window: u64) -> (Vec<String>, bool) {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(meta) = std::fs::metadata(path) else {
        return (Vec::new(), true);
    };
    let size = meta.len();
    let Ok(mut fh) = std::fs::File::open(path) else {
        return (Vec::new(), true);
    };
    let whole = size <= window;
    let from = size.saturating_sub(window);
    if from > 0 && fh.seek(SeekFrom::Start(from)).is_err() {
        return (Vec::new(), true);
    }
    let mut raw = Vec::new();
    if fh.read_to_end(&mut raw).is_err() {
        return (Vec::new(), true);
    }
    let text = String::from_utf8_lossy(&raw);
    let mut lines: Vec<String> = text.lines().map(|l| l.to_string()).collect();
    // The first line of a window that started mid-file is a fragment.
    if !whole && !lines.is_empty() {
        lines.remove(0);
    }
    (lines, whole)
}

/// Has a backwards reader seen enough?
///
/// **Not at the quota alone.** A long stretch of tool calls puts a dozen results
/// between two prompts, so the last six records of a working session are
/// routinely six things the agent said and nothing you asked. The answer to
/// "where did this one get to" needs the question in it, so the reader keeps
/// going past its quota until one of your turns is in view, and gives up four
/// turns later rather than reading the whole conversation for a session that
/// genuinely opens with the agent.
pub fn enough(out: &[Turn], want: usize) -> bool {
    (out.len() >= want && out.iter().any(|t| t.you)) || out.len() >= want + 4
}

/// The last `want` turns of a list read forwards, under the same rule.
pub fn tail_of(all: Vec<Turn>, want: usize) -> Vec<Turn> {
    let mut from = all.len().saturating_sub(want);
    while from > 0 && !all[from..].iter().any(|t| t.you) && all.len() - from < want + 4 {
        from -= 1;
    }
    all[from..].to_vec()
}

/// Tail windows, tried in order and only as far as needed.
///
/// Every real session ends inside the first; a long run of trailing tool records
/// escalates; the last covers any transcript worth previewing. Ported from rses,
/// which arrived at them against the same corpus.
pub const WINDOWS: [u64; 3] = [256 * 1024, 4 * 1024 * 1024, 64 * 1024 * 1024];

/// Walk a tail backwards for turns, widening the window until there are enough
/// of them or the whole file has been read.
pub fn tail_turns(
    path: &Path,
    want: usize,
    from_lines: fn(&[String], usize) -> Vec<Turn>,
) -> Vec<Turn> {
    let mut turns = Vec::new();
    for w in WINDOWS {
        let (lines, whole) = tail_lines(path, w);
        turns = from_lines(&lines, want);
        if turns.len() >= want || whole {
            break;
        }
    }
    turns
}

/// A whole file's prose, or only what it has gained since `from`.
///
/// **Grown**: read the new bytes alone, which is a few kilobytes for a session
/// that is being worked in. **Shrunk**: not the same file any more whatever its
/// name says, so it is read whole. A seek landing inside a character is also a
/// whole read rather than a failure, since the next pass would hit it again.
pub fn file_prose(path: &Path, from: u64, extract: fn(&str) -> String) -> Option<Prose> {
    use std::io::{Read, Seek, SeekFrom};
    let size = std::fs::metadata(path).ok()?.len();
    let grown = from > 0 && from < size;
    let mut fh = std::fs::File::open(path).ok()?;
    if grown && fh.seek(SeekFrom::Start(from)).is_err() {
        return None;
    }
    let mut text = String::new();
    if fh.read_to_string(&mut text).is_err() {
        let mut raw = Vec::new();
        let mut fh = std::fs::File::open(path).ok()?;
        fh.read_to_end(&mut raw).ok()?;
        return Some(Prose {
            fingerprint: size,
            text: extract(&String::from_utf8_lossy(&raw)),
            whole: true,
        });
    }
    Some(Prose {
        fingerprint: size,
        text: extract(&text),
        whole: !grown,
    })
}

/// Every `.jsonl` under a directory, at any depth, following symlinks.
///
/// The depth matters: claude's subagents write four levels down, and stopping at
/// two missed them.
pub fn walk_jsonl(dir: &Path, out: &mut Vec<(i64, std::path::PathBuf)>) {
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        // metadata() follows symlinks, which is what `find -L` does.
        let Ok(m) = std::fs::metadata(&p) else {
            continue;
        };
        if m.is_dir() {
            walk_jsonl(&p, out);
            continue;
        }
        if p.extension().and_then(|x| x.to_str()) != Some("jsonl") {
            continue;
        }
        out.push((mtime_of(&m), p));
    }
}

/// A file's modification time, in whole seconds since the epoch.
pub fn mtime_of(m: &std::fs::Metadata) -> i64 {
    m.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// A title taken from something somebody typed: one line, single-spaced, and cut
/// to something a row can hold.
pub fn title_from_prompt(s: &str) -> String {
    let mut t: String = s
        .chars()
        .map(|c| if c.is_control() || c == '\t' { ' ' } else { c })
        .collect();
    while t.contains("  ") {
        t = t.replace("  ", " ");
    }
    let t = t.trim();
    if t.chars().count() > 80 {
        format!("{}…", t.chars().take(79).collect::<String>())
    } else {
        t.to_string()
    }
}

/// Everything between two markers, the first time they occur.
pub fn between<'a>(s: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let a = s.find(open)? + open.len();
    let b = s[a..].find(close)? + a;
    Some(&s[a..b])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_prompt_becomes_a_row_sized_title() {
        assert_eq!(
            title_from_prompt("  the\tthing   I asked\n"),
            "the thing I asked"
        );
        let long = "x".repeat(200);
        let t = title_from_prompt(&long);
        assert_eq!(t.chars().count(), 80);
        assert!(t.ends_with('…'));
        // nothing that could shift a tab-separated cache field survives
        assert!(!title_from_prompt("a\tb\nc").contains('\t'));
    }

    #[test]
    fn a_marked_span_is_found_or_it_is_not() {
        assert_eq!(between("a<X>body</X>b", "<X>", "</X>"), Some("body"));
        assert_eq!(between("no markers", "<X>", "</X>"), None);
        assert_eq!(between("<X>unterminated", "<X>", "</X>"), None);
    }

    /// A window that starts mid-file drops its first line, because that line is
    /// a fragment; a window covering the file keeps every line and says so.
    #[test]
    fn a_tail_drops_the_fragment_it_started_in() {
        let d = std::env::temp_dir().join(format!("tmxtail{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("t.jsonl");
        std::fs::write(&f, "aaaa\nbbbb\ncccc\ndddd\n").unwrap();

        let (lines, whole) = tail_lines(&f, 1024);
        assert!(whole);
        assert_eq!(lines, vec!["aaaa", "bbbb", "cccc", "dddd"]);

        // a window of 12 bytes starts at offset 8, mid-way through "bbbb", so
        // that fragment goes and the two whole lines after it stay
        let (lines, whole) = tail_lines(&f, 12);
        assert!(!whole);
        assert_eq!(lines, vec!["cccc", "dddd"]);

        let _ = std::fs::remove_dir_all(&d);
    }

    /// The incremental read: a grown file contributes only its new bytes, a
    /// shrunken one is read whole because it is not the same file any more.
    #[test]
    fn prose_is_read_from_where_the_last_pass_stopped() {
        let d = std::env::temp_dir().join(format!("tmxprose{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("t");
        std::fs::write(&f, "one\n").unwrap();
        let p = file_prose(&f, 0, |s| s.replace('\n', " ")).expect("read");
        assert_eq!((p.fingerprint, p.whole), (4, true));
        assert_eq!(p.text, "one ");

        std::fs::write(&f, "one\ntwo\n").unwrap();
        let p = file_prose(&f, 4, |s| s.replace('\n', " ")).expect("read");
        assert_eq!((p.fingerprint, p.whole), (8, false));
        assert_eq!(p.text, "two ");

        // shrunk: the offset is past the end, so it is not an increment
        std::fs::write(&f, "x\n").unwrap();
        let p = file_prose(&f, 8, |s| s.replace('\n', " ")).expect("read");
        assert!(p.whole);
        assert_eq!(p.text, "x ");

        let _ = std::fs::remove_dir_all(&d);
    }

    /// An agent nothing knows about answers empty rather than panicking, since
    /// the cache is a file anything could have written.
    #[test]
    fn an_unknown_agent_answers_nothing() {
        assert_eq!(meta("nosuchagent", "k"), Meta::default());
        assert!(prose("nosuchagent", "k", 0).is_none());
        assert!(turns("nosuchagent", "k", 3).is_empty());
        assert!(resume("nosuchagent", "k").is_err());
    }
}
