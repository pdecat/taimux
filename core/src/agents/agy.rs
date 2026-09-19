//! Antigravity CLI (`agy`): one JSONL transcript per conversation, under
//! `~/.gemini/antigravity-cli/brain/<id>/.system_generated/logs/transcript.jsonl`.
//!
//! Two record types are the conversation: `USER_INPUT`, whose content wraps what
//! you typed in `<USER_REQUEST>` alongside metadata blocks nobody wrote, and
//! `PLANNER_RESPONSE`. Everything else in the file is the agent's own plumbing.
//!
//! **The working directory is not in the transcript.** It is in
//! `history.jsonl`, keyed by conversation id, which is also the directory name.
//! That file is a few kilobytes and covers every conversation at once, so it is
//! read once rather than per session.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::{Meta, Past, Prose, Turn};

pub fn agy_dir() -> PathBuf {
    super::gemini::gemini_dir().join("antigravity-cli")
}

fn brain_dir() -> PathBuf {
    agy_dir().join("brain")
}

/// Conversation id to the directory it ran in, from the shell history file.
///
/// Built once per process, for the same reason the Gemini project map is.
fn cwd_map() -> &'static HashMap<String, String> {
    static MAP: std::sync::OnceLock<HashMap<String, String>> = std::sync::OnceLock::new();
    MAP.get_or_init(build_cwd_map)
}

fn build_cwd_map() -> HashMap<String, String> {
    let mut map = HashMap::new();
    let Ok(text) = std::fs::read_to_string(agy_dir().join("history.jsonl")) else {
        return map;
    };
    for line in text.lines() {
        let id = crate::json::field(line, "conversationId");
        let ws = crate::json::field(line, "workspace");
        if !id.is_empty() && !ws.is_empty() {
            map.insert(id, ws);
        }
    }
    map
}

/// The conversation id a transcript belongs to: the directory three levels up.
fn id_of(key: &str) -> String {
    Path::new(key)
        .ancestors()
        .nth(3)
        .and_then(|p| p.file_name())
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

pub fn discover(out: &mut Vec<Past>) {
    for sub in std::fs::read_dir(brain_dir())
        .into_iter()
        .flatten()
        .flatten()
    {
        let p = sub
            .path()
            .join(".system_generated")
            .join("logs")
            .join("transcript.jsonl");
        let Ok(m) = std::fs::metadata(&p) else {
            continue;
        };
        out.push(Past {
            agent: "agy",
            key: p.to_string_lossy().into_owned(),
            mtime: super::mtime_of(&m),
            meta: None,
        });
    }
}

/// The directory from the history file, and the opening request as the title.
pub fn meta(key: &str) -> Meta {
    let cwd = cwd_map().get(&id_of(key)).cloned().unwrap_or_default();
    // The opening request is at the top of the file, so a small head read finds
    // it whatever the transcript has grown to since.
    let title = head_request(Path::new(key)).unwrap_or_default();
    Meta {
        src: if title.is_empty() { "" } else { "p" },
        cwd,
        version: String::new(),
        title,
    }
}

/// How far into a transcript the opening request can be. The first record is a
/// `USER_INPUT`, so this is generous rather than tuned.
const HEAD: usize = 64 * 1024;

fn head_request(path: &Path) -> Option<String> {
    use std::io::Read;
    let mut buf = vec![0u8; HEAD];
    let mut fh = std::fs::File::open(path).ok()?;
    let n = fh.read(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf[..n]);
    for line in text.lines() {
        if let Some(t) = user_text(line) {
            return Some(super::title_from_prompt(&t));
        }
    }
    None
}

pub fn prose(key: &str, from: u64) -> Option<Prose> {
    super::file_prose(Path::new(key), from, extract)
}

/// Both sides' prose, with the metadata blocks that ride inside a user record
/// left out: they are the harness talking, not you.
pub fn extract(text: &str) -> String {
    let mut out = String::new();
    for line in text.lines() {
        if let Some(t) = user_text(line) {
            out.push_str(&t);
            out.push(' ');
        } else if let Some(t) = planner_text(line) {
            out.push_str(&t);
            out.push(' ');
        }
    }
    out
}

/// What you typed in a `USER_INPUT` record.
///
/// The content carries `<USER_REQUEST>` and then blocks nobody wrote
/// (`<ADDITIONAL_METADATA>`, `<USER_SETTINGS_CHANGE>`), so only the request is
/// taken. A record with no request marker at all is taken whole, since older
/// transcripts wrote it bare.
fn user_text(line: &str) -> Option<String> {
    if !line.contains("\"type\":\"USER_INPUT\"") {
        return None;
    }
    let raw = content_of(line)?;
    let t = match super::between(&raw, "<USER_REQUEST>", "</USER_REQUEST>") {
        Some(inner) => inner.trim().to_string(),
        None => raw.trim().to_string(),
    };
    (!t.is_empty()).then_some(t)
}

fn planner_text(line: &str) -> Option<String> {
    if !line.contains("\"type\":\"PLANNER_RESPONSE\"") {
        return None;
    }
    let t = content_of(line)?.trim().to_string();
    (!t.is_empty()).then_some(t)
}

/// A record's `content`, unescaped enough to read.
fn content_of(line: &str) -> Option<String> {
    let at = line.find("\"content\":\"")? + "\"content\":\"".len();
    let body = crate::transcript::json_body(&line[at..]);
    Some(crate::transcript::clean(&body))
}

pub fn turns(key: &str, want: usize) -> Vec<Turn> {
    super::tail_turns(Path::new(key), want, from_lines)
}

fn from_lines(lines: &[String], want: usize) -> Vec<Turn> {
    let mut out: Vec<Turn> = Vec::new();
    for line in lines.iter().rev() {
        if super::enough(&out, want) {
            break;
        }
        if let Some(t) = user_text(line) {
            out.push(Turn { you: true, text: t });
        } else if let Some(t) = planner_text(line) {
            out.push(Turn {
                you: false,
                text: t,
            });
        }
    }
    out.reverse();
    out
}

/// `agy --conversation <id>`, which is the flag its own `--help` advertises for
/// exactly this.
pub fn resume(key: &str) -> Result<String, String> {
    let id = id_of(key);
    if id.is_empty() {
        return Err("that conversation has no id to resume by".into());
    }
    Ok(format!(
        "command agy --conversation {}",
        crate::tmux::shell_quote(&id)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: &str = concat!(
        r#"{"type":"USER_INPUT","content":"<USER_REQUEST>\nfix the gpg agent\n</USER_REQUEST>\n<ADDITIONAL_METADATA>\nnoise\n</ADDITIONAL_METADATA>"}"#,
        "\n",
        r#"{"type":"PLANNER_RESPONSE","content":"Looking at the agent now"}"#,
        "\n",
        r#"{"type":"TOOL_CALL","content":"never shown"}"#,
        "\n"
    );

    /// The request is taken out of its marker, and the metadata block that rides
    /// with it is not: that block is in every record and would match everything.
    #[test]
    fn only_the_request_half_of_a_user_record_is_kept() {
        let t = user_text(T.lines().next().unwrap()).expect("request");
        assert_eq!(t, "fix the gpg agent");
        assert!(!t.contains("noise"));
    }

    /// A transcript that wrote the request bare is still readable.
    #[test]
    fn a_record_with_no_marker_is_taken_whole() {
        let l = r#"{"type":"USER_INPUT","content":"just this"}"#;
        assert_eq!(user_text(l).as_deref(), Some("just this"));
    }

    #[test]
    fn the_prose_is_both_sides_and_nothing_else() {
        let p = extract(T);
        assert!(p.contains("fix the gpg agent"), "{p}");
        assert!(p.contains("Looking at the agent now"), "{p}");
        assert!(!p.contains("never shown"), "{p}");
    }

    #[test]
    fn turns_come_back_oldest_first() {
        let lines: Vec<String> = T.lines().map(|l| l.to_string()).collect();
        let t = from_lines(&lines, 5);
        assert_eq!(t.len(), 2);
        assert!(t[0].you);
        assert!(!t[1].you);
    }

    /// The id is the directory three levels above the transcript, and it is what
    /// `--conversation` takes.
    #[test]
    fn the_conversation_id_comes_off_the_path() {
        let k = "/h/.gemini/antigravity-cli/brain/abc-123/.system_generated/logs/transcript.jsonl";
        assert_eq!(id_of(k), "abc-123");
        assert_eq!(
            resume(k).expect("resumable"),
            "command agy --conversation abc-123"
        );
        // …and a path that is not one refuses rather than resuming nothing
        assert!(resume("transcript.jsonl").is_err());
    }
}
