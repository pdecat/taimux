//! Codex CLI: one JSONL rollout per session, under `~/.codex/sessions/`.
//!
//! **Two schemas**, and a file is one or the other depending on which year wrote
//! it. They are told apart by the first record and their record shapes are
//! disjoint, so a backwards reader that cannot see line 0 still recognises both:
//!
//! - **A**: opens with `session_meta`, carrying `cwd` and the thread id. Turns
//!   are `event_msg`/`user_message` and `response_item` with an assistant role.
//! - **B**: opens with the session object itself (`id`, `cwd`, `git`). Turns are
//!   `message` records with a role, and the user's first one has an
//!   `<environment_context>` block stapled to it that nobody typed.
//!
//! Codex also keeps a `state_<n>.sqlite` index of the same sessions, which is
//! what its own `resume` picker reads. taimux does not: everything the list needs
//! is in the rollout, and reading the file keeps this agent working on a machine
//! whose Codex is older or newer than whatever schema that database is on.

use std::path::{Path, PathBuf};

use super::{Meta, Past, Prose, Turn};

fn sessions_dir() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default())
        .join(".codex")
        .join("sessions")
}

pub fn discover(out: &mut Vec<Past>) {
    let mut found: Vec<(i64, PathBuf)> = Vec::new();
    super::walk_jsonl(&sessions_dir(), &mut found);
    for (mtime, path) in found {
        out.push(Past {
            agent: "codex",
            key: path.to_string_lossy().into_owned(),
            mtime,
            meta: None,
        });
    }
}

/// The directory and the opening request, both of which live at the TOP of a
/// rollout whichever schema it is.
pub fn meta(key: &str) -> Meta {
    let Some(head) = head(Path::new(key)) else {
        return Meta::default();
    };
    let mut cwd = String::new();
    let mut title = String::new();
    for line in head.lines() {
        if cwd.is_empty() {
            // Schema A keeps it under `payload`, schema B at the top level; both
            // spell it `cwd` and neither has another field by that name.
            cwd = crate::json::field(line, "cwd");
        }
        if title.is_empty() {
            if let Some(t) = user_text(line) {
                title = super::title_from_prompt(&t);
            }
        }
        if !cwd.is_empty() && !title.is_empty() {
            break;
        }
    }
    Meta {
        src: if title.is_empty() { "" } else { "p" },
        cwd: cwd.replace('\t', " "),
        version: String::new(),
        title,
    }
}

/// The opening of a rollout: enough for the session record and the first prompt.
fn head(path: &Path) -> Option<String> {
    use std::io::Read;
    let mut buf = vec![0u8; 64 * 1024];
    let mut fh = std::fs::File::open(path).ok()?;
    let n = fh.read(&mut buf).ok()?;
    Some(String::from_utf8_lossy(&buf[..n]).into_owned())
}

/// The thread id, which is what `codex resume` takes.
///
/// Both schemas carry it: schema A under the `session_meta` payload, schema B as
/// the first record's own `id`. Failing both, the rollout filename carries it,
/// which is how Codex's own filesystem fallback finds one.
fn id_of(key: &str) -> String {
    if let Some(head) = head(Path::new(key)) {
        for line in head.lines().take(4) {
            let id = crate::json::field(line, "id");
            if is_uuid(&id) {
                return id;
            }
        }
    }
    Path::new(key)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
        .rsplit_once("rollout-")
        .map(|(_, rest)| rest.to_string())
        .unwrap_or_default()
}

fn is_uuid(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 36
        && b.iter().enumerate().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => *c == b'-',
            _ => c.is_ascii_hexdigit(),
        })
}

pub fn prose(key: &str, from: u64) -> Option<Prose> {
    super::file_prose(Path::new(key), from, extract)
}

pub fn extract(text: &str) -> String {
    let mut out = String::new();
    for line in text.lines() {
        if let Some(t) = user_text(line) {
            out.push_str(&t);
            out.push(' ');
        } else if let Some(t) = assistant_text(line) {
            out.push_str(&t);
            out.push(' ');
        }
    }
    out
}

/// What you typed, in either schema, with the environment block taken off.
///
/// Codex staples an `<environment_context>` to the first user message, listing
/// the cwd, the shell and the sandbox. Leaving it in would put every machine's
/// paths in every session's index, which is the same flood the claude reader
/// drops `<system-reminder>` for.
fn user_text(line: &str) -> Option<String> {
    let raw =
        if line.contains("\"type\":\"event_msg\"") && line.contains("\"type\":\"user_message\"") {
            grab_first(line, "\"message\":\"")
        } else if line.contains("\"type\":\"message\"") && line.contains("\"role\":\"user\"") {
            text_parts(line)
        } else {
            None
        }?;
    let t = strip_environment(&raw).trim().to_string();
    (!t.is_empty()).then_some(t)
}

fn assistant_text(line: &str) -> Option<String> {
    let is_a = (line.contains("\"type\":\"response_item\"")
        || line.contains("\"type\":\"message\""))
        && line.contains("\"role\":\"assistant\"");
    if !is_a {
        return None;
    }
    let t = text_parts(line)?.trim().to_string();
    (!t.is_empty()).then_some(t)
}

/// The text of a content array, whichever of the three part types it used.
fn text_parts(line: &str) -> Option<String> {
    let mut s = String::new();
    crate::transcript::grab(line, "\"text\":\"", &mut s);
    let s = s.trim().to_string();
    (!s.is_empty()).then_some(s)
}

fn grab_first(line: &str, key: &str) -> Option<String> {
    let at = line.find(key)? + key.len();
    let body = crate::transcript::json_body(&line[at..]);
    Some(crate::transcript::clean(&body))
}

fn strip_environment(s: &str) -> String {
    let mut out = s.to_string();
    while let Some(a) = out.find("<environment_context>") {
        match out[a..].find("</environment_context>") {
            Some(b) => {
                let end = a + b + "</environment_context>".len();
                out = format!("{} {}", &out[..a], &out[end..]);
            }
            None => {
                out.truncate(a);
                break;
            }
        }
    }
    out
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
        } else if let Some(t) = assistant_text(line) {
            out.push(Turn {
                you: false,
                text: t,
            });
        }
    }
    out.reverse();
    out
}

/// `codex resume <id>`.
pub fn resume(key: &str) -> Result<String, String> {
    let id = id_of(key);
    if id.is_empty() {
        return Err("that rollout carries no session id to resume by".into());
    }
    Ok(format!(
        "command codex resume {}",
        crate::tmux::shell_quote(&id)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = concat!(
        r#"{"type":"session_meta","payload":{"id":"11111111-2222-3333-4444-555555555555","cwd":"/w"}}"#,
        "\n",
        r#"{"type":"event_msg","payload":{"type":"user_message","message":"the ask"}}"#,
        "\n",
        r#"{"type":"response_item","payload":{"role":"assistant","content":[{"type":"output_text","text":"the answer"}]}}"#,
        "\n"
    );

    const B: &str = concat!(
        r#"{"id":"99999999-8888-7777-6666-555555555555","cwd":"/other","git":{"branch":"main"}}"#,
        "\n",
        r#"{"type":"message","role":"user","content":[{"type":"input_text","text":"<environment_context>\nshell: bash\n</environment_context>\nthe real ask"}]}"#,
        "\n",
        r#"{"type":"message","role":"assistant","content":[{"type":"output_text","text":"schema b answer"}]}"#,
        "\n"
    );

    #[test]
    fn both_schemas_yield_the_same_two_sides() {
        let pa = extract(A);
        assert!(pa.contains("the ask") && pa.contains("the answer"), "{pa}");
        let pb = extract(B);
        assert!(
            pb.contains("the real ask") && pb.contains("schema b answer"),
            "{pb}"
        );
    }

    /// The environment block is the harness talking, and it carries every
    /// machine's paths. Leaving it in would put them in every session's index.
    #[test]
    fn the_environment_block_never_reaches_the_index() {
        let p = extract(B);
        assert!(!p.contains("shell: bash"), "{p}");
        assert!(p.contains("the real ask"), "{p}");
        // an unterminated block takes the rest with it rather than surviving
        assert_eq!(
            strip_environment("keep<environment_context>rest").trim(),
            "keep"
        );
    }

    /// A tail cannot see line 0, so both schemas have to be recognisable per
    /// record. They are, because their shapes are disjoint.
    #[test]
    fn a_backwards_reader_recognises_either_schema() {
        for src in [A, B] {
            let lines: Vec<String> = src.lines().map(|l| l.to_string()).collect();
            let t = from_lines(&lines, 4);
            assert_eq!(t.len(), 2, "{:?}", t);
            assert!(t[0].you && !t[1].you);
        }
    }

    #[test]
    fn a_uuid_is_recognised_and_nothing_else_is() {
        assert!(is_uuid("11111111-2222-3333-4444-555555555555"));
        assert!(!is_uuid("11111111-2222-3333-4444-55555555555"));
        assert!(!is_uuid("zzzzzzzz-2222-3333-4444-555555555555"));
    }
}
