//! Claude Code: one JSONL transcript per conversation, under
//! `~/.claude/projects/<project>/<session-id>.jsonl`.
//!
//! The only agent taimux can match to a live pane, which is why it is also the
//! only one whose sessions leave this list when one is open (see `conv`).

use std::path::Path;

use super::{Meta, Past, Prose, Turn};
use crate::conv;

/// Every conversation on disk: the transcripts sitting directly in a project
/// directory, and nothing deeper.
///
/// **Depth is what tells a conversation from a subagent.** A subagent writes to
/// `projects/<proj>/<session>/subagents/agent-*.jsonl`, and a workflow's to
/// `…/subagents/workflows/<id>/agent-*.jsonl`. Those are sidechains of a
/// conversation that is itself in the list, they carry no title, no directory
/// and no version, and `claude --resume` on one opens something nobody ever had
/// a pane on. 72 of them were on this list, which is 72 rows you cannot act on
/// in front of the ones you can.
pub fn discover(out: &mut Vec<Past>) {
    let root = conv::claude_dir().join("projects");
    for proj in std::fs::read_dir(&root).into_iter().flatten().flatten() {
        for f in std::fs::read_dir(proj.path())
            .into_iter()
            .flatten()
            .flatten()
        {
            let p = f.path();
            if p.extension().and_then(|x| x.to_str()) != Some("jsonl") {
                continue;
            }
            let Ok(m) = std::fs::metadata(&p) else {
                continue;
            };
            if m.is_dir() {
                continue;
            }
            out.push(Past {
                agent: "claude",
                key: p.to_string_lossy().into_owned(),
                mtime: super::mtime_of(&m),
                meta: None,
            });
        }
    }
}

/// The cwd, the claude version and the title, read BACKWARDS and stopped as soon
/// as all three are in hand.
///
/// That is what makes listing hundreds of them affordable: all three are
/// re-stated on recent records, so a 23 MB conversation costs no more than a
/// small one. Only the tail of the file is read for the same reason.
pub fn meta(key: &str) -> Meta {
    let path = Path::new(key);
    let (lines, _) = super::tail_lines(path, TAIL);
    let (mut cwd, mut ver, mut ttl, mut ait, mut lp) = (
        String::new(),
        String::new(),
        String::new(),
        String::new(),
        String::new(),
    );
    for (n, line) in lines.iter().rev().enumerate() {
        if cwd.is_empty() {
            cwd = val(line, "cwd");
        }
        if ver.is_empty() {
            ver = val(line, "version");
        }
        // The two kinds interleave for the whole life of a session, so the last
        // one written is usually the unprefixed ai-title while the pane shows the
        // custom one. Prefer the custom one and keep the other as a fallback.
        if ttl.is_empty() && line.contains("\"type\":\"custom-title\"") {
            ttl = val(line, "customTitle");
        }
        if ait.is_empty() && line.contains("\"type\":\"ai-title\"") {
            ait = val(line, "aiTitle");
        }
        // A session can end before it was ever titled: a short one, or one that
        // was cleared. The prompt it was last given identifies such a session far
        // better than "(no title)": one of Patrick's turned out to be the tail of
        // a pasted config file, which is exactly the session you would go looking
        // for.
        if lp.is_empty() && line.contains("\"type\":\"last-prompt\"") {
            lp = val(line, "lastPrompt");
        }
        if !cwd.is_empty() && !ver.is_empty() && !ttl.is_empty() {
            break;
        }
        if n > 400 {
            break; // a transcript that never says: stop digging
        }
    }
    if ttl.is_empty() {
        ttl = ait;
    }
    let mut src = if ttl.is_empty() { "" } else { "t" };
    if ttl.is_empty() && !lp.is_empty() {
        ttl = super::title_from_prompt(
            &lp.replace("\\n", " ")
                .replace("\\r", " ")
                .replace("\\t", " "),
        );
        src = "p";
    }
    let clean = |s: String| s.replace('\t', " ");
    Meta {
        cwd: clean(cwd),
        version: clean(ver),
        title: clean(ttl),
        src,
    }
}

/// How far back `meta` reads. Every one of the three values it wants is restated
/// within a few records of the end; 400 lines of a busy transcript comfortably
/// fits in this, and a session that never says lands on the line cap instead.
const TAIL: u64 = 512 * 1024;

fn val(line: &str, key: &str) -> String {
    crate::json::field(line, key)
}

pub fn prose(key: &str, from: u64) -> Option<Prose> {
    super::file_prose(Path::new(key), from, crate::transcript::extract)
}

pub fn turns(key: &str, want: usize) -> Vec<Turn> {
    super::tail_turns(Path::new(key), want, from_lines)
}

/// The claude turn reader, over a window of lines.
///
/// It reaches back past its quota for one of your turns; see `agents::enough`,
/// which is where that rule lives now that every agent follows it.
fn from_lines(lines: &[String], want: usize) -> Vec<Turn> {
    let mut out: Vec<Turn> = Vec::new();
    for (n, line) in lines.iter().rev().enumerate() {
        if n > 800 {
            break;
        }
        if crate::transcript::is_noise(line) {
            continue;
        }
        let you = if line.contains("\"type\":\"assistant\"") {
            false
        } else if line.contains("\"role\":\"user\"") {
            true
        } else {
            continue;
        };
        let Some(t) = crate::transcript::first_text(line) else {
            continue;
        };
        let t = crate::transcript::clean(&t);
        if t.is_empty() {
            continue;
        }
        out.push(Turn { you, text: t });
        if super::enough(&out, want) {
            break;
        }
    }
    out.reverse();
    out
}

/// **Through `command`**, so no shell alias fires and tmux-resurrect can read the
/// pane's argv back later.
pub fn resume(key: &str) -> String {
    format!("command claude --resume {}", crate::tmux::shell_quote(key))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str, body: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("tmxcl{}{}", std::process::id(), name));
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("t.jsonl");
        std::fs::write(&f, body).unwrap();
        f
    }

    #[test]
    fn meta_reads_backwards_and_prefers_a_custom_title() {
        let f = fixture(
            "a",
            concat!(
                r#"{"cwd":"/w","version":"2.1.1"}"#,
                "\n",
                r#"{"type":"ai-title","aiTitle":"what the model called it"}"#,
                "\n",
                r#"{"type":"custom-title","customTitle":"what I called it"}"#,
                "\n"
            ),
        );
        let m = meta(&f.to_string_lossy());
        assert_eq!((m.cwd.as_str(), m.version.as_str()), ("/w", "2.1.1"));
        assert_eq!(m.title, "what I called it");
        assert_eq!(m.src, "t");
        let _ = std::fs::remove_dir_all(f.parent().unwrap());
    }

    /// An untitled session falls back to the last prompt, and says so with `p`,
    /// because a LIVE pane may borrow a real title but never a prompt.
    #[test]
    fn an_untitled_session_falls_back_to_its_last_prompt() {
        let f = fixture(
            "b",
            concat!(
                r#"{"cwd":"/w","version":"2.1.1"}"#,
                "\n",
                r#"{"type":"last-prompt","lastPrompt":"the thing\nI asked"}"#,
                "\n"
            ),
        );
        let m = meta(&f.to_string_lossy());
        assert_eq!(m.title, "the thing I asked");
        assert_eq!(m.src, "p");
        let _ = std::fs::remove_dir_all(f.parent().unwrap());
    }

    /// A tab in any cached field would shift every field after it.
    #[test]
    fn tabs_are_kept_out_of_the_cached_fields() {
        let f = fixture(
            "c",
            "{\"cwd\":\"/w\\ta\",\"version\":\"1\",\"type\":\"custom-title\",\"customTitle\":\"a\\tb\"}\n",
        );
        let m = meta(&f.to_string_lossy());
        assert!(!m.cwd.contains('\t'));
        assert!(!m.title.contains('\t'));
        let _ = std::fs::remove_dir_all(f.parent().unwrap());
    }

    /// The preview reaches back past its quota until one of YOUR turns is in
    /// view, because a working session ends in a run of the agent's own.
    #[test]
    fn the_turns_reach_back_for_a_prompt() {
        let mut body = String::from(
            "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"THEQUESTION\"}}\n",
        );
        for i in 0..5 {
            body.push_str(&format!(
                "{{\"type\":\"assistant\",\"message\":{{\"role\":\"assistant\",\"content\":[{{\"type\":\"text\",\"text\":\"answer {}\"}}]}}}}\n",
                i
            ));
        }
        let f = fixture("d", &body);
        let t = turns(&f.to_string_lossy(), 2);
        assert!(
            t.iter().any(|x| x.you && x.text == "THEQUESTION"),
            "{:?}",
            t
        );
        // oldest first: the question leads
        assert!(t[0].you);
        let _ = std::fs::remove_dir_all(f.parent().unwrap());
    }

    #[test]
    fn the_resume_goes_through_command_and_quotes_its_path() {
        assert_eq!(resume("/a/b.jsonl"), "command claude --resume /a/b.jsonl");
        assert_eq!(resume("/a b.jsonl"), "command claude --resume '/a b.jsonl'");
    }
}
