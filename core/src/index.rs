//! Reading the search index and the sessions cache.
//!
//! Step 5 of removing fzf. The picker's two remaining features both live in
//! files the bash indexer writes, so this READS those files rather than porting
//! the writer with them. That keeps the step small and keeps both implementations
//! working off one copy of the data, which is also what makes them diffable.
//! Moving the writer is the rest of stage 4 and it is a separate job.
//!
//! Two formats, both deliberately built for a shell to read with one `read`:
//!
//! ```text
//! index/<pane id with everything awkward flattened>
//!   idx <fingerprint of what has been indexed> <epoch> <pane id> <key>
//!   <the blob, one line, no tabs>
//!
//! sessions
//!   sess <epoch> 2
//!   <mtime> <pane|-> <agent> <key> <cwd> <version> <title> <t|p>
//! ```
//!
//! One index file per PANE, not per transcript: the pane is what the list is
//! keyed by, and a `/clear` moving a pane onto a new conversation is then just a
//! header naming a transcript that is no longer this pane's.
//!
//! The `2` on the sessions header is a format version, and the only thing that
//! reads it is the check that refuses an older file. There is nothing to migrate:
//! this cache lives in `XDG_RUNTIME_DIR`, dies with the boot, and is rewritten
//! whole on every pass, so a version taimux does not recognise is one pass of
//! staleness rather than a problem.

use std::collections::HashMap;
use std::path::PathBuf;

/// Enough context to read a hit as a phrase, and no more: a snippet has to fit on
/// a row behind a label and a summary. The preview is where the wide version of
/// the same thing lives.
const ROW_CTX: usize = 20;
const PREVIEW_CTX: usize = 46;

pub fn index_dir() -> PathBuf {
    crate::paths::runtime_dir().join("index")
}

/// An index file's name: the pane id with everything awkward flattened out, so a
/// remote id (`ha:%6`) is a legal filename. The id itself is carried IN the file
/// rather than reverse-engineered from the name.
pub fn key_for(pane: &str) -> String {
    pane.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

pub fn sessions_file() -> PathBuf {
    crate::paths::runtime_dir().join("sessions")
}

/// The query split the way the picker splits it, with fzf's smart case: a query
/// that is all lower case matches case-insensitively, one carrying a capital is
/// taken literally.
pub struct Query {
    pub terms: Vec<String>,
    pub fold: bool,
}

impl Query {
    pub fn new(q: &str) -> Query {
        Query {
            terms: q.split_whitespace().map(|t| t.to_string()).collect(),
            fold: q == q.to_lowercase(),
        }
    }

    fn haystack(&self, line: &str) -> String {
        if self.fold {
            line.to_lowercase()
        } else {
            line.to_string()
        }
    }
}

/// One index file, split into the pane it is about and its blob.
fn read_entry(path: &std::path::Path) -> Option<(String, String)> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut lines = text.lines();
    let head: Vec<&str> = lines.next()?.split(' ').collect();
    // Anything that is not a well-formed header is a file being rewritten under
    // us or left by an older version, and it is skipped rather than guessed at.
    if head.first() != Some(&"idx") || head.len() < 4 {
        return None;
    }
    Some((head[3].to_string(), lines.next().unwrap_or("").to_string()))
}

/// Take one window of context around a hit, marking each end that was cut.
///
/// Character-wise, not byte-wise: a snippet cut mid-character is not text any
/// more, and these blobs carry whatever the sessions said.
fn window(blob: &str, at: usize, len: usize, ctx: usize) -> String {
    let chars: Vec<char> = blob.chars().collect();
    let s = at.saturating_sub(ctx);
    let e = (at + len + ctx).min(chars.len());
    let mut out = String::new();
    if s > 0 {
        out.push('…');
    }
    out.extend(&chars[s..e]);
    if e < chars.len() {
        out.push('…');
    }
    out
}

/// Where a term is in the blob, as a CHARACTER offset.
fn find_at(hay: &str, term: &str) -> Option<(usize, usize)> {
    let b = hay.find(term)?;
    Some((hay[..b].chars().count(), term.chars().count()))
}

/// pane id to the snippet that says why that row is in the list.
///
/// Every term has to be in the blob, the same rule the row is kept by. A row
/// whose session merely mentions one of them is not a hit.
pub fn snippets(q: &Query) -> HashMap<String, String> {
    let mut out = HashMap::new();
    if q.terms.is_empty() {
        return out;
    }
    let Ok(dir) = std::fs::read_dir(index_dir()) else {
        return out;
    };
    for e in dir.flatten() {
        let Some((pane, blob)) = read_entry(&e.path()) else {
            continue;
        };
        let hay = q.haystack(&blob);
        let mut parts: Vec<String> = Vec::new();
        let mut all = true;
        for t in &q.terms {
            match find_at(&hay, t) {
                Some((at, len)) => parts.push(window(&blob, at, len, ROW_CTX)),
                None => {
                    all = false;
                    break;
                }
            }
        }
        if all && !parts.is_empty() {
            out.insert(pane, parts.join(" · "));
        }
    }
    out
}

/// One place a term lands, split so the caller can pick the term out however it
/// draws: the picker styles a span, `taimux preview` writes reverse video.
pub struct Hit {
    pub before: String,
    pub term: String,
    pub after: String,
    /// Whether text was cut off that side, i.e. whether an ellipsis belongs there.
    pub cut_left: bool,
    pub cut_right: bool,
}

/// The wide version, for the preview: several windows of context rather than
/// one, since this is where you find out whether the hit is the one you were
/// after before jumping to it.
///
/// Context is around the FIRST term, which is the one the eye is looking for,
/// but every term still has to be in the blob: the same rule the row was kept
/// by, or the preview would explain a match the list did not actually make.
pub fn preview_hits(pane: &str, q: &Query, want: usize) -> Vec<Hit> {
    let Some((_, blob)) = read_entry(&index_dir().join(key_for(pane))) else {
        return Vec::new();
    };
    let hay = q.haystack(&blob);
    if q.terms.is_empty() || q.terms.iter().any(|t| !hay.contains(t.as_str())) {
        return Vec::new();
    }
    let first = &q.terms[0];
    if first.is_empty() {
        return Vec::new();
    }
    let chars: Vec<char> = blob.chars().collect();
    let mut out = Vec::new();
    let mut from = 0usize;
    while out.len() < want {
        let Some(b) = hay[from..].find(first.as_str()) else {
            break;
        };
        let at = hay[..from + b].chars().count();
        let len = first.chars().count();
        let s = at.saturating_sub(PREVIEW_CTX);
        let e = (at + len + PREVIEW_CTX).min(chars.len());
        out.push(Hit {
            before: chars[s..at].iter().collect(),
            term: chars[at..(at + len).min(chars.len())].iter().collect(),
            after: chars[(at + len).min(e)..e].iter().collect(),
            cut_left: s > 0,
            cut_right: e < chars.len(),
        });
        from += b + first.len();
    }
    out
}

/// The same hits as one string each, for a caller that does its own styling.
pub fn preview_match(pane: &str, q: &Query, want: usize) -> Vec<String> {
    preview_hits(pane, q, want)
        .into_iter()
        .map(|h| {
            format!(
                "{}{}{}{}{}",
                if h.cut_left { "…" } else { "" },
                h.before,
                h.term,
                h.after,
                if h.cut_right { "…" } else { "" }
            )
        })
        .collect()
}

/// One row of the sessions cache.
pub struct Session {
    pub mtime: i64,
    /// `-` when no pane is on this conversation. Only ever a pane id for claude,
    /// which is the one agent that publishes which conversation a pane is on.
    pub pane: String,
    /// Which tool wrote it.
    pub agent: String,
    /// What that tool calls it: a transcript path, or a session id for a store
    /// that keeps its conversations in a database.
    pub key: String,
    pub cwd: String,
    pub version: String,
    pub title: String,
    /// `t` for a title the session recorded, `p` for a prompt standing in for
    /// one.
    pub src: String,
}

/// The version of the cache format this build writes and will read.
const FORMAT: &str = "2";

/// The header a sessions cache opens with.
pub fn header(now: i64) -> String {
    format!("sess {} {}\n", now, FORMAT)
}

pub fn sessions() -> Vec<Session> {
    let Ok(text) = std::fs::read_to_string(sessions_file()) else {
        return Vec::new();
    };
    let mut lines = text.lines();
    // A cache written by another version is skipped rather than misread: its
    // fields are in different places, and this one is rewritten within a pass.
    match lines.next().and_then(|h| h.split(' ').nth(2)) {
        Some(FORMAT) => {}
        _ => return Vec::new(),
    }
    lines
        .filter_map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            if f.len() < 8 {
                return None;
            }
            Some(Session {
                mtime: f[0].parse().unwrap_or(0),
                pane: f[1].to_string(),
                agent: f[2].to_string(),
                key: f[3].to_string(),
                cwd: f[4].to_string(),
                version: f[5].to_string(),
                title: f[6].to_string(),
                src: f[7].to_string(),
            })
        })
        .collect()
}

/// The picker's id for a past conversation: `dead:<agent>:<key>`.
///
/// A past session has no pane, so the row needs a name of its own, and that name
/// has to carry the agent as well as the key: the key alone says which
/// conversation but not which tool knows how to open it, and two agents can key
/// by a path.
///
/// The `dead:` prefix is unchanged from when this list was claude-only, and it
/// stays: it is what every caller matches on, it appears in the golden layout
/// fixture, and renaming it would be a wire change bought with nothing.
pub fn past_id(agent: &str, key: &str) -> String {
    format!("dead:{}:{}", agent, key)
}

/// The agent and key back out of a row id, or None where it is not one.
///
/// The key can itself contain colons (a path can), so this splits ONCE: the
/// agent is the first segment and everything after it is the key.
pub fn split_past_id(id: &str) -> Option<(&str, &str)> {
    let rest = id.strip_prefix("dead:")?;
    let (agent, key) = rest.split_once(':')?;
    (!agent.is_empty() && !key.is_empty()).then_some((agent, key))
}

/// How long ago, in one column's worth: minutes under the hour, hours under two
/// days, days after that. A rounded age is what you actually remember a session
/// by, where a timestamp would be four columns of digits to date yourself.
pub fn age(then: i64, now: i64) -> String {
    let d = now - then;
    if d < 0 {
        "now".into()
    } else if d < 3600 {
        format!("{}m", d / 60)
    } else if d < 172800 {
        format!("{}h", d / 3600)
    } else {
        format!("{}d", d / 86400)
    }
}

/// The past-sessions list, in the same eight fields a pane row has so the layout
/// does not have to know the difference.
///
/// **A claude session open in a pane is left out**, since it is right there in
/// every other list; one belonging to any other agent is not, because taimux
/// cannot tell. Which conversation a pane is on is something the agent has to
/// publish, and claude is the only one that does. That asymmetry is the whole
/// reason this list is "past sessions" rather than "ended" ones: it is the
/// history, and a conversation you happen to be in is still part of it.
pub fn dead_rows(now: i64) -> String {
    if !sessions_file().is_file() {
        // The indexer runs behind the picker, so the first time this mode is
        // opened after a boot it can genuinely have nothing to say. Better that
        // it says so than that it looks like a box with no history in it.
        return "dead:!\t-\t-\t-\t\tdead\t-\tstill reading your conversations…\n".to_string();
    }
    let mut s = String::new();
    for e in sessions() {
        if e.pane != "-" {
            continue; // still open in a pane, and on every other list already
        }
        let title = if e.title.is_empty() {
            "(no title)"
        } else {
            &e.title
        };
        let cwd = if e.cwd.is_empty() { "?" } else { &e.cwd };
        s.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\tdead\t-\t{}\n",
            past_id(&e.agent, &e.key),
            age(e.mtime, now),
            cwd,
            e.agent,
            e.version,
            title
        ));
    }
    s
}

/// pane id to the title its conversation recorded, for a live pane that publishes
/// none of its own.
///
/// Only a REAL title (`t`), never a last prompt. A session started as
/// `claude attach <id>` through the shell alias has the subcommand shifted out of
/// its slot and reads it as a prompt, so its fallback is the word "attach", which
/// as a summary says nothing at all.
pub fn pane_titles() -> HashMap<String, String> {
    sessions()
        .into_iter()
        .filter(|e| e.pane != "-" && !e.title.is_empty() && e.src == "t")
        .map(|e| (e.pane, e.title))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ages_read_the_way_you_remember_a_session() {
        assert_eq!(age(1000, 1000), "0m");
        assert_eq!(age(0, 1800), "30m");
        assert_eq!(age(0, 7200), "2h");
        assert_eq!(age(0, 172800), "2d");
        // a clock that has gone backwards is not a negative age
        assert_eq!(age(2000, 1000), "now");
    }

    #[test]
    fn a_window_marks_the_ends_it_cut() {
        let blob = "the quick brown fox jumps over the lazy dog and keeps going";
        let w = window(blob, 10, 5, 4);
        assert!(w.starts_with('…'));
        assert!(w.ends_with('…'));
        assert!(w.contains("brown"));

        // a hit at the very start is not marked as cut there
        let w = window(blob, 0, 3, 4);
        assert!(!w.starts_with('…'));
        assert!(w.ends_with('…'));
    }

    /// Character offsets, not byte offsets. A blob carries whatever the sessions
    /// said, and a snippet cut mid-character is not text any more.
    #[test]
    fn windows_are_cut_on_characters_not_bytes() {
        let blob = "ééé needle ééé";
        let hay = blob.to_lowercase();
        let (at, len) = find_at(&hay, "needle").expect("found");
        assert_eq!(at, 4); // three e-acutes and a space
        let w = window(blob, at, len, 4);
        assert!(w.contains("needle"));
        assert!(w.chars().count() <= 4 + 6 + 4 + 2);
    }

    #[test]
    fn smart_case_matches_the_way_fzf_does() {
        let q = Query::new("auth layer");
        assert!(q.fold);
        assert_eq!(q.terms.len(), 2);
        assert_eq!(q.haystack("AUTH Layer"), "auth layer");

        let q = Query::new("Auth");
        assert!(!q.fold);
        assert_eq!(q.haystack("AUTH Layer"), "AUTH Layer");
    }

    #[test]
    fn a_header_that_is_not_a_header_is_skipped() {
        let d = std::env::temp_dir().join(format!("jmidx{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let good = d.join("good");
        std::fs::write(&good, "idx 10 20 %7 /a/b.jsonl\nsome words here\n").unwrap();
        assert_eq!(
            read_entry(&good),
            Some(("%7".into(), "some words here".into()))
        );

        let bad = d.join("bad");
        std::fs::write(&bad, "half a line\n").unwrap();
        assert_eq!(read_entry(&bad), None);

        // a file being rewritten under us: the header is there but truncated
        let short = d.join("short");
        std::fs::write(&short, "idx 10 20\n").unwrap();
        assert_eq!(read_entry(&short), None);

        // …and one with no blob yet reads as an empty blob rather than failing
        let head = d.join("head");
        std::fs::write(&head, "idx 10 20 %8 /a/b.jsonl\n").unwrap();
        assert_eq!(read_entry(&head), Some(("%8".into(), String::new())));
        let _ = std::fs::remove_dir_all(&d);
    }
}
