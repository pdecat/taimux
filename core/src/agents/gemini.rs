//! Gemini CLI: one JSONL chat log per session, under
//! `~/.gemini/tmp/<project>/chats/session-*.jsonl`.
//!
//! Two things here are Gemini's own and neither is optional:
//!
//! - **The project directory is named by a hash.** `<project>` is either a
//!   friendly label or the SHA-256 of the absolute working directory, depending
//!   on which version wrote it; both forms are present here, 25 of each. The
//!   directory is resolved through `~/.gemini/projects.json`, which maps the path
//!   to its friendly name, hashed both ways so either spelling lands.
//! - **A rewind deletes turns that are still in the file.** A `$rewindTo` record
//!   drops its target message *and* everything after it, and it sits AFTER them,
//!   so a backwards scan meets rewound turns before it learns they are gone.
//!   Meeting one therefore skips back to (and past) its target, which is exactly
//!   the run it deleted.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::{Meta, Past, Prose, Turn};

fn tmp_dir() -> PathBuf {
    gemini_dir().join("tmp")
}

pub fn gemini_dir() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".gemini")
}

/// Directory name (hash OR friendly label) to the absolute path it stands for.
///
/// Built from `projects.json`, which is keyed the other way round. Both spellings
/// are inserted because `tmp/` uses either, and the session header's
/// `projectHash` is a third caller of the same map.
///
/// Built ONCE per process: it was being rebuilt per session, which re-read and
/// re-hashed the whole file 132 times in one pass for an answer that cannot have
/// changed in between.
fn project_map() -> &'static HashMap<String, String> {
    static MAP: std::sync::OnceLock<HashMap<String, String>> = std::sync::OnceLock::new();
    MAP.get_or_init(build_project_map)
}

fn build_project_map() -> HashMap<String, String> {
    let mut map = HashMap::new();
    let Ok(text) = std::fs::read_to_string(gemini_dir().join("projects.json")) else {
        return map;
    };
    // One flat object under "projects", `"<abs path>": "<label>"`. Read as pairs
    // of quoted strings rather than parsed, on the same rule as everything else
    // here, and whitespace-tolerantly: the file is pretty-printed, but nothing
    // says it has to be.
    let Some(at) = text.find("\"projects\"") else {
        return map;
    };
    let mut rest = &text[at + "\"projects\"".len()..];
    while let (Some(path), r1) = quoted(rest) {
        let (label, r2) = quoted(r1);
        map.insert(crate::sha256::hex(path.as_bytes()), path.clone());
        if let Some(label) = label {
            if !label.is_empty() {
                map.insert(label, path);
            }
        }
        rest = r2;
    }
    map
}

/// The next double-quoted string in the text, and what follows it.
///
/// Keys here are absolute paths and labels are plain words, so there is no
/// escape to handle: a path containing a quote would not be one taimux could
/// resolve anyway.
fn quoted(s: &str) -> (Option<String>, &str) {
    let Some(a) = s.find('"') else {
        return (None, "");
    };
    let rest = &s[a + 1..];
    let Some(b) = rest.find('"') else {
        return (None, "");
    };
    (Some(rest[..b].to_string()), &rest[b + 1..])
}

pub fn discover(out: &mut Vec<Past>) {
    for sub in std::fs::read_dir(tmp_dir()).into_iter().flatten().flatten() {
        let chats = sub.path().join("chats");
        for f in std::fs::read_dir(&chats).into_iter().flatten().flatten() {
            let p = f.path();
            let name = p
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            if !name.starts_with("session-") {
                continue;
            }
            let ext = p.extension().and_then(|x| x.to_str()).unwrap_or("");
            if ext != "jsonl" && ext != "json" {
                continue;
            }
            let Ok(m) = std::fs::metadata(&p) else {
                continue;
            };
            out.push(Past {
                agent: "gemini",
                key: p.to_string_lossy().into_owned(),
                mtime: super::mtime_of(&m),
                meta: None,
            });
        }
    }
}

/// The directory from the header's hash, and the first thing you asked as the
/// title: Gemini records no title of its own, so the opening prompt is the only
/// thing that identifies a session.
pub fn meta(key: &str) -> Meta {
    let path = Path::new(key);
    let Ok(text) = std::fs::read_to_string(path) else {
        return Meta::default();
    };
    let map = project_map();
    let mut cwd = String::new();
    // Whitespace-tolerant, because the legacy layout writes `"projectHash": "…"`
    // with a space and the compact one does not.
    if let Some((_, h)) = string_fields(&text, "projectHash").first() {
        cwd = map.get(h).cloned().unwrap_or_default();
    }
    // The directory the file sits in is the same question asked another way, and
    // it answers for a session whose header never carried a hash.
    if cwd.is_empty() {
        if let Some(dir) = path
            .parent()
            .and_then(|p| p.parent())
            .and_then(|p| p.file_name())
        {
            cwd = map
                .get(&dir.to_string_lossy().into_owned())
                .cloned()
                .unwrap_or_default();
        }
    }
    let title = scan_turns(&text)
        .into_iter()
        .find(|t| t.you)
        .map(|t| super::title_from_prompt(&t.text))
        .unwrap_or_default();
    Meta {
        // `p`, never `t`: this is the prompt you opened with, not a title the
        // session recorded, and the two are not interchangeable to a caller that
        // borrows one for a live pane.
        src: if title.is_empty() { "" } else { "p" },
        cwd,
        version: String::new(),
        title,
    }
}

pub fn prose(key: &str, from: u64) -> Option<Prose> {
    super::file_prose(Path::new(key), from, extract)
}

/// Every piece of conversation text in a chat log, as one line.
pub fn extract(text: &str) -> String {
    let mut out = String::new();
    for t in scan_turns(text) {
        out.push_str(&t.text);
        out.push(' ');
    }
    out
}

pub fn turns(key: &str, want: usize) -> Vec<Turn> {
    let path = Path::new(key);
    // Read whole rather than tailed. A legacy chat has no line structure to tail
    // at all, and a rewind is only meaningful against the turns before it, so a
    // window that starts after one cannot tell what it deleted. These files are
    // small: the largest here is under a megabyte, where a claude transcript
    // reaches 37.
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    super::tail_of(scan_turns(&text), want)
}

/// Every turn in a chat log, oldest first, rewinds applied.
///
/// **Layout-agnostic, and it has to be**: Gemini has written these two ways, and
/// both are on this machine. The current one is a JSON object per line; the
/// legacy one is a single pretty-printed object holding a `messages` array, and
/// that is 106 of the 132 sessions here. A line-based reader finds nothing
/// whatsoever in the second, which is what made every legacy session show up as
/// `(no title)` with no directory.
///
/// So neither layout is parsed as lines. The text is cut at each message's own
/// `id` (and at each `$rewindTo`, which has none), and each piece is read for a
/// role and whatever prose follows it. Whitespace around every colon is
/// tolerated, since the pretty layout has it and the compact one does not.
fn scan_turns(text: &str) -> Vec<Turn> {
    enum Mark {
        Id(String),
        Rewind(String),
    }
    let mut marks: Vec<(usize, Mark)> = Vec::new();
    for (at, v) in string_fields(text, "id") {
        marks.push((at, Mark::Id(v)));
    }
    for (at, v) in string_fields(text, "$rewindTo") {
        marks.push((at, Mark::Rewind(v)));
    }
    marks.sort_by_key(|(at, _)| *at);

    let mut out: Vec<Turn> = Vec::new();
    let mut ids: Vec<String> = Vec::new();
    for (i, (at, mark)) in marks.iter().enumerate() {
        match mark {
            // A rewind drops its target message AND everything after it.
            Mark::Rewind(target) => {
                if let Some(n) = ids.iter().position(|x| x == target) {
                    out.truncate(n);
                    ids.truncate(n);
                }
            }
            Mark::Id(id) => {
                let end = marks.get(i + 1).map(|(a, _)| *a).unwrap_or(text.len());
                let chunk = &text[*at..end];
                let Some(you) = role_of(chunk) else { continue };
                let t = prose_of(chunk);
                if t.is_empty() {
                    continue;
                }
                ids.push(id.clone());
                out.push(Turn { you, text: t });
            }
        }
    }
    out
}

/// Whose turn a message is, or neither.
fn role_of(chunk: &str) -> Option<bool> {
    match string_fields(chunk, "type")
        .first()
        .map(|(_, v)| v.as_str())
    {
        Some("user") => Some(true),
        Some("gemini") => Some(false),
        _ => None,
    }
}

/// Blocks the harness writes into a turn rather than anything you typed.
///
/// `<session_context>` is the worst of them: it carries the whole workspace
/// directory listing, up to 200 entries, at the top of every session. Indexing
/// it would put every project's file names in every conversation that ever
/// touched that project, which is the same flood the claude reader drops
/// `<system-reminder>` for, and it would make the opening prompt useless as a
/// title.
const INJECTED: [&str; 4] = [
    "session_context",
    "project_context",
    "loaded_context",
    "tool_output_masked",
];

/// One message's prose: the text of its parts, or a plain string content.
fn prose_of(chunk: &str) -> String {
    let mut parts: Vec<String> = string_fields(chunk, "text")
        .into_iter()
        .map(|(_, v)| v)
        .collect();
    if parts.is_empty() {
        parts = string_fields(chunk, "content")
            .into_iter()
            .map(|(_, v)| v)
            .collect();
    }
    let mut joined = parts
        .into_iter()
        .filter(|p| !p.trim().is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    for tag in INJECTED {
        joined = crate::transcript::untag(joined, &format!("<{}>", tag), &format!("</{}>", tag));
    }
    joined.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Every `"key" : "value"` in the text, in order, as (where the key started,
/// the value), tolerating whitespace the way JSON allows it.
///
/// The value is read with the transcript reader's escape handling, so a quote
/// inside a message cannot end it early.
fn string_fields(text: &str, key: &str) -> Vec<(usize, String)> {
    let needle = format!("\"{}\"", key);
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(at) = text[from..].find(&needle) {
        let start = from + at;
        from = start + needle.len();
        let rest = text[from..].trim_start();
        let Some(rest) = rest.strip_prefix(':') else {
            continue;
        };
        let rest = rest.trim_start();
        let Some(rest) = rest.strip_prefix('"') else {
            continue; // an array or a number: not a string field
        };
        let body = crate::transcript::json_body(rest);
        out.push((start, crate::transcript::clean(&body)));
    }
    out
}

/// `gemini --resume <session id>`.
///
/// The id is in the chat log's header. The filename carries only its first eight
/// characters, which Gemini's own lookup accepts but which can collide, so the
/// header is read and the filename is the fallback rather than the other way
/// round.
///
/// Sessions are scoped per project over there, and this opens in the directory
/// the session ran in, so the two agree.
///
/// **Not tested against a live Gemini**, which is not installed on the machine
/// this was written on: the flag is what its own session-management documentation
/// specifies. Everything else here was checked against real data.
pub fn resume(key: &str) -> Result<String, String> {
    let id = session_id(Path::new(key));
    if id.is_empty() {
        return Err("that chat log carries no session id to resume by".into());
    }
    Ok(format!(
        "command gemini --resume {}",
        crate::tmux::shell_quote(&id)
    ))
}

/// The session id a chat log names itself by.
fn session_id(path: &Path) -> String {
    if let Ok(text) = std::fs::read_to_string(path) {
        if let Some((_, id)) = string_fields(&text, "sessionId").first() {
            if !id.is_empty() {
                return id.clone();
            }
        }
    }
    // `session-2026-06-05T11-40-5095293a` : the tail is the id's first eight
    // characters, which is what Gemini's own by-id lookup matches on.
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .and_then(|n| n.rsplit_once('-').map(|(_, t)| t.to_string()))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOG: &str = concat!(
        r#"{"sessionId":"s1","projectHash":"HASH","kind":"main"}"#,
        "\n",
        r#"{"id":"m1","type":"user","content":"the first question"}"#,
        "\n",
        r#"{"id":"m2","type":"gemini","content":[{"text":"the first answer"}]}"#,
        "\n",
        r#"{"id":"m3","type":"user","content":"the second question"}"#,
        "\n"
    );

    /// The legacy layout: one pretty-printed object with a `messages` array, and
    /// whitespace after every colon. 106 of the 132 sessions here are this, and a
    /// line-based reader found nothing at all in any of them.
    const LEGACY: &str = r#"{
  "sessionId": "s1",
  "projectHash": "HASH",
  "messages": [
    {
      "id": "m1",
      "type": "user",
      "content": [
        { "text": "the first question" }
      ]
    },
    {
      "id": "m2",
      "type": "gemini",
      "content": [
        { "text": "the first answer" }
      ]
    }
  ]
}"#;

    #[test]
    fn both_content_shapes_are_read() {
        let t = scan_turns(LOG);
        assert_eq!(t.len(), 3);
        assert_eq!(t[0].text, "the first question");
        assert!(t[0].you);
        assert_eq!(t[1].text, "the first answer");
        assert!(!t[1].you);
    }

    /// Both LAYOUTS, read by the same code, which is the point of reading them
    /// by marker rather than by line.
    #[test]
    fn the_legacy_layout_reads_the_same_as_the_current_one() {
        let t = scan_turns(LEGACY);
        assert_eq!(t.len(), 2, "{:?}", t);
        assert_eq!(t[0].text, "the first question");
        assert!(t[0].you);
        assert_eq!(t[1].text, "the first answer");
        assert!(!t[1].you);
    }

    /// A rewind drops its target AND everything after it.
    #[test]
    fn a_rewind_deletes_the_run_it_points_at() {
        let log = format!("{}{}\n", LOG, r#"{"$rewindTo":"m2"}"#);
        let t = scan_turns(&log);
        assert_eq!(t.len(), 1, "{:?}", t);
        assert_eq!(t[0].text, "the first question");
    }

    /// The block the harness writes at the top of a session carries the whole
    /// workspace listing. Indexing it would put every project's file names in
    /// every conversation that touched that project.
    #[test]
    fn the_injected_context_block_never_reaches_the_index() {
        let log = concat!(
            r#"{"id":"m1","type":"user","content":"<session_context>\nWorkspace: /w\n├───secrets.yaml\n</session_context>\nthe real question"}"#,
            "\n"
        );
        let p = extract(log);
        assert!(p.contains("the real question"), "{p}");
        assert!(!p.contains("secrets.yaml"), "{p}");
    }

    #[test]
    fn the_prose_index_covers_both_sides() {
        let p = extract(LOG);
        assert!(p.contains("the first question"), "{p}");
        assert!(p.contains("the first answer"), "{p}");
        // the header is not a turn
        assert!(!p.contains("HASH"), "{p}");
    }

    /// The id comes off the header, and off the filename when there is no
    /// header to read: the filename carries only the first eight characters,
    /// which is what Gemini's own lookup matches on.
    #[test]
    fn the_session_id_comes_from_the_header_or_the_filename() {
        let d = std::env::temp_dir().join(format!("tmxgem{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("session-2026-06-05T11-40-5095293a.jsonl");
        std::fs::write(&f, LOG).unwrap();
        assert_eq!(
            resume(&f.to_string_lossy()).expect("resumable"),
            "command gemini --resume s1"
        );
        // no header: the filename's tail stands in
        std::fs::write(&f, "").unwrap();
        assert_eq!(
            resume(&f.to_string_lossy()).expect("resumable"),
            "command gemini --resume 5095293a"
        );
        let _ = std::fs::remove_dir_all(&d);
    }
}
