//! OpenCode: every conversation in one SQLite database, at
//! `~/.local/share/opencode/opencode.db`.
//!
//! The only agent here with no files to walk, which changes three things:
//!
//! - **The key is a session id**, not a path, and that id is what
//!   `opencode --session` takes, so nothing has to be translated back.
//! - **There is no transcript to point a handoff at.** Everything the receiving
//!   model gets has to be in the prompt.
//! - **Nothing is incremental.** A conversation's fingerprint is its
//!   `time_updated`, so a session that has said something since the last pass is
//!   re-read whole and one that has not is not read at all. Since a conversation
//!   nobody is in never changes, that is one read per session, ever.
//!
//! The store is opened through a symlink in this tool's runtime directory, so
//! the `-wal` SQLite makes lands there rather than in OpenCode's own directory.
//! See `sqlite`.

use std::path::PathBuf;

use super::{Meta, Past, Prose, Turn};
use crate::sqlite;

fn db_path() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default())
        .join(".local/share/opencode/opencode.db")
}

fn open() -> Option<sqlite::Db> {
    sqlite::open(&db_path())
}

/// A string safe to paste into a `WHERE` clause.
///
/// These ids come off rows this same database handed back, so this is a belt
/// rather than a defence: nothing here takes a session id from a user. A quote
/// doubles, as SQL says, and an id carrying anything else is refused outright
/// rather than escaped cleverly.
fn quote(id: &str) -> Option<String> {
    if id.is_empty() || id.len() > 128 || id.contains(['\'', '"', '\0', '\n']) {
        return None;
    }
    Some(format!("'{}'", id))
}

/// Every top-level conversation, with when it last said anything AND everything
/// a row says about it.
///
/// Archived ones and children (a sub-agent's session carries a `parent_id`) are
/// left out, which is the same pair of conditions OpenCode's own picker uses.
///
/// The title and directory ride along because they are columns of the row this
/// query is already reading. Leaving them for `meta` to ask about meant opening
/// a 71 MB database 101 more times in one pass, for values it had already had in
/// its hand.
pub fn discover(out: &mut Vec<Past>) {
    let Some(db) = open() else { return };
    let Some(rows) = db.rows(
        "SELECT id, time_updated, title, directory FROM session \
         WHERE time_archived IS NULL AND parent_id IS NULL",
    ) else {
        return;
    };
    for r in rows {
        if r.len() < 4 {
            continue;
        }
        let id = r[0].text();
        if id.is_empty() {
            continue;
        }
        out.push(Past {
            agent: "opencode",
            key: id,
            // OpenCode counts in milliseconds; everything else here is seconds.
            mtime: r[1].int() / 1000,
            meta: Some(meta_from(&r[2].text(), &r[3].text())),
        });
    }
}

/// A row's two descriptive columns, as a `Meta`.
fn meta_from(title: &str, directory: &str) -> Meta {
    let title = super::title_from_prompt(title);
    Meta {
        // `t`: OpenCode records a real title of its own, the way claude does.
        src: if title.is_empty() { "" } else { "t" },
        cwd: directory.replace('\t', " "),
        version: String::new(),
        title,
    }
}

/// When this conversation last said anything, in milliseconds, which is what
/// stands in for a byte count here.
pub fn fingerprint(key: &str) -> Option<u64> {
    let (db, id) = (open()?, quote(key)?);
    let rows = db.rows(&format!(
        "SELECT time_updated FROM session WHERE id = {} LIMIT 1",
        id
    ))?;
    Some(rows.first()?[0].int().max(0) as u64)
}

pub fn meta(key: &str) -> Meta {
    let (Some(db), Some(id)) = (open(), quote(key)) else {
        return Meta::default();
    };
    let Some(rows) = db.rows(&format!(
        "SELECT title, directory FROM session WHERE id = {} LIMIT 1",
        id
    )) else {
        return Meta::default();
    };
    let Some(r) = rows.first() else {
        return Meta::default();
    };
    meta_from(
        &r[0].text(),
        &r.get(1).map(|c| c.text()).unwrap_or_default(),
    )
}

/// A conversation's prose, whole.
///
/// The fingerprint is `time_updated` rather than a byte count: there is no file
/// to measure, and a session that has not been spoken to has not changed.
pub fn prose(key: &str) -> Option<Prose> {
    let (db, id) = (open()?, quote(key)?);
    let stamp = db
        .rows(&format!(
            "SELECT time_updated FROM session WHERE id = {} LIMIT 1",
            id
        ))?
        .first()
        .map(|r| r[0].int())
        .unwrap_or(0);
    // Text parts only. A tool call's arguments and its output are most of the
    // bytes and none of the prose, exactly as in a claude transcript.
    let rows = db.rows(&format!(
        "SELECT p.data FROM message m JOIN part p ON p.message_id = m.id \
         WHERE m.session_id = {} ORDER BY m.time_created ASC, p.time_created ASC",
        id
    ))?;
    let mut out = String::new();
    for r in rows {
        if let Some(t) = part_text(&r[0].text()) {
            out.push_str(&t);
            out.push(' ');
        }
    }
    Some(Prose {
        fingerprint: stamp.max(0) as u64,
        text: out,
        whole: true,
    })
}

/// The text of one `part` row, which stores its payload as JSON.
fn part_text(data: &str) -> Option<String> {
    if !data.contains("\"type\":\"text\"") {
        return None;
    }
    let at = data.find("\"text\":\"")? + "\"text\":\"".len();
    let body = crate::transcript::json_body(&data[at..]);
    let t = crate::transcript::clean(&body);
    (!t.is_empty()).then_some(t)
}

/// The last `want` turns, oldest first.
///
/// Which messages carry text is only knowable from their parts, and a session
/// can end in a long run of tool-only ones, so a `LIMIT` on the messages would
/// silently cut the preview short. The messages are listed newest first and
/// their parts fetched until enough turns are in hand.
pub fn turns(key: &str, want: usize) -> Vec<Turn> {
    let (Some(db), Some(id)) = (open(), quote(key)) else {
        return Vec::new();
    };
    let Some(msgs) = db.rows(&format!(
        "SELECT m.id, m.data FROM message m WHERE m.session_id = {} \
         ORDER BY m.time_created DESC",
        id
    )) else {
        return Vec::new();
    };
    let mut out: Vec<Turn> = Vec::new();
    for m in msgs {
        if super::enough(&out, want) {
            break;
        }
        let you = match role_of(&m[1].text()) {
            Some(r) => r,
            None => continue,
        };
        let Some(mid) = quote(&m[0].text()) else {
            continue;
        };
        let Some(parts) = db.rows(&format!(
            "SELECT data FROM part WHERE message_id = {} ORDER BY time_created ASC",
            mid
        )) else {
            continue;
        };
        let text: Vec<String> = parts
            .iter()
            .filter_map(|p| part_text(&p[0].text()))
            .collect();
        let text = text.join(" ").trim().to_string();
        if !text.is_empty() {
            out.push(Turn { you, text });
        }
    }
    out.reverse();
    out
}

/// Whether a message row is yours, theirs, or neither.
fn role_of(data: &str) -> Option<bool> {
    match crate::json::field(data, "role").as_str() {
        "user" => Some(true),
        "assistant" => Some(false),
        _ => None,
    }
}

/// `opencode --session <id>`, which its `--help` advertises as "session id to
/// continue".
///
/// rses could not do this: when it was written OpenCode had no resume-by-id and
/// the only way back into a conversation was its own picker. It has one now.
pub fn resume(key: &str) -> String {
    format!(
        "command opencode --session {}",
        crate::tmux::shell_quote(key)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ids come off rows this database handed back, so this is a belt rather
    /// than a defence. It still refuses rather than escaping cleverly.
    #[test]
    fn an_id_that_could_end_a_string_literal_is_refused() {
        assert_eq!(quote("ses_abc123").as_deref(), Some("'ses_abc123'"));
        assert!(quote("a' OR 1=1 --").is_none());
        assert!(quote("a\"b").is_none());
        assert!(quote("a\nb").is_none());
        assert!(quote("").is_none());
        assert!(quote(&"x".repeat(200)).is_none());
    }

    #[test]
    fn only_a_text_part_contributes_prose() {
        assert_eq!(
            part_text(r#"{"type":"text","text":"what was said"}"#).as_deref(),
            Some("what was said")
        );
        // a tool call is most of the bytes and none of the prose
        assert!(part_text(r#"{"type":"tool","state":{"output":"BIG"}}"#).is_none());
        assert!(part_text(r#"{"type":"text","text":""}"#).is_none());
    }

    #[test]
    fn a_message_row_says_whose_turn_it_was() {
        assert_eq!(role_of(r#"{"role":"user"}"#), Some(true));
        assert_eq!(role_of(r#"{"role":"assistant"}"#), Some(false));
        assert_eq!(role_of(r#"{"role":"system"}"#), None);
        assert_eq!(role_of("{}"), None);
    }

    #[test]
    fn the_resume_names_the_session() {
        assert_eq!(resume("ses_abc"), "command opencode --session ses_abc");
    }

    /// With no database, every one of these answers empty rather than failing:
    /// a machine without OpenCode is the ordinary case.
    #[test]
    fn a_machine_without_opencode_simply_has_no_rows() {
        let _g = crate::env::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let had = std::env::var_os("HOME");
        std::env::set_var("HOME", "/nowhere/at/all");
        let mut out = Vec::new();
        discover(&mut out);
        assert!(out.is_empty());
        assert_eq!(meta("ses_x"), Meta::default());
        assert!(prose("ses_x").is_none());
        assert!(turns("ses_x", 3).is_empty());
        match had {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
    }
}
