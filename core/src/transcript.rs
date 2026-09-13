//! A conversation, boiled down to the prose you might search for.
//!
//! A port of the bash prototype's `_index_extract` and the `_TRANSCRIPT_AWK` helpers it
//! shares with the preview. Faithful on purpose: the prototype was the
//! specification, its exclusions each cost something to learn, and this had to
//! agree byte for byte before it could replace it.
//!
//! Two things are kept and everything else is dropped: a user turn whose content
//! is a plain string (what you typed), and every `{"type":"text","text":…}` block
//! (both sides' prose, plus the text of anything pasted or attached). Each
//! exclusion is here because keeping it made search useless rather than merely
//! bigger:
//!
//! - tool RESULTS are file contents and command output, and nearly all of a
//!   transcript's bytes: 23 MB of transcript yields 428 KB of prose, or 7.6 MB
//!   with the results left in;
//! - harness-injected turns: `isMeta` caveats, subagents' sidechain turns, and
//!   the attachment types carrying a catalogue rather than content. rses measured
//!   what those cost: the deferred-tool list alone put `EnterWorktree` in every
//!   session, so "worktree" matched 154 of them instead of the 9 that discussed
//!   one;
//! - `<system-reminder>` and `<command-…>` blocks, which ride inside an otherwise
//!   real turn and are that same flood by another route.
//!
//! Tool-call INPUTS fall out for free: a tool_use carries its arguments under
//! `input`, never under `text`.

/// Attachment types that carry a catalogue rather than something a person wrote.
const INJECTED: [&str; 15] = [
    "deferred_tools_delta",
    "mcp_instructions_delta",
    "agent_listing_delta",
    "command_permissions",
    "output_style",
    "task_reminder",
    "total_tokens_reminder",
    "date_change",
    "skill_listing",
    "plan_mode",
    "plan_mode_exit",
    "diagnostics",
    "hook_success",
    "hook_non_blocking_error",
    "nested_memory",
];

/// A record that is not part of the conversation.
fn is_noise(line: &str) -> bool {
    line.contains("\"toolUseResult\"")
        || line.contains("\"isMeta\":true")
        || line.contains("\"isSidechain\":true")
}

fn is_injected_attachment(line: &str) -> bool {
    let Some(at) = line.find("\"attachment\":{\"type\":\"") else {
        return false;
    };
    let rest = &line[at + "\"attachment\":{\"type\":\"".len()..];
    INJECTED
        .iter()
        .any(|t| rest.len() > t.len() && rest.starts_with(*t) && rest.as_bytes()[t.len()] == b'"')
}

/// The body of a JSON string, given everything from just past its opening quote.
///
/// Both escape forms are parked on placeholder characters first, so the hunt for
/// the closing quote cannot stop on an escaped one. They come back as spaces
/// rather than as themselves, which is the bash version's approximation and
/// affordable here: this is a search index, not a parser.
fn json_body(p: &str) -> String {
    let mut t = p.replace("\\\\", "\u{1}").replace("\\\"", "\u{2}");
    if let Some(i) = t.find('"') {
        t.truncate(i);
    }
    t.replace(['\u{1}', '\u{2}'], " ")
}

/// Everything between the first opening tag and ITS closing tag, over and over.
///
/// The shortest match, which a regex `gsub` could not give: a prompt can sit
/// BETWEEN two `<system-reminder>` blocks, and a greedy match would swallow it
/// along with them.
fn untag(mut s: String, open: &str, close: &str) -> String {
    while let Some(a) = s.find(open) {
        let after = a + open.len();
        match s[after..].find(close) {
            Some(b) => {
                let end = after + b + close.len();
                s = format!("{} {}", &s[..a], &s[end..]);
            }
            None => {
                s.truncate(a);
                return s;
            }
        }
    }
    s
}

/// Strip the tags whose contents are never worth searching, and flatten the
/// result to one line of single-spaced text.
fn clean(s: &str) -> String {
    // JSON escapes first: \n, \r, \t and \uXXXX all become a space, which also
    // means no control character can survive into a row or a terminal.
    let mut out = String::with_capacity(s.len());
    let b: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < b.len() {
        if b[i] == '\\' && i + 1 < b.len() && matches!(b[i + 1], 'n' | 'r' | 't' | 'u') {
            out.push(' ');
            i += 2;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    let out = untag(out, "<system-reminder>", "</system-reminder>");
    let out = strip_command_tags(&out);
    let out: String = out
        .chars()
        // Tabs are what awk replaces. Control characters cannot occur (JSON
        // forbids them raw, and \uXXXX escapes became spaces above), so mapping
        // them is belt-and-braces that cannot diverge: verified across all 436
        // transcripts here.
        .map(|c| if c == '\t' || c.is_control() { ' ' } else { c })
        .collect();
    // collapse runs of spaces, then trim
    let mut collapsed = String::with_capacity(out.len());
    let mut last_space = false;
    for c in out.chars() {
        if c == ' ' {
            if !last_space {
                collapsed.push(c);
            }
            last_space = true;
        } else {
            collapsed.push(c);
            last_space = false;
        }
    }
    // ASCII spaces only, matching awk's `sub(/^ +/)`. Rust's `trim` is
    // Unicode-aware and would also strip U+00A0, which diverged from the bash
    // version on four real transcripts that open with a non-breaking space.
    // Arguably the better behaviour, but the prototype was the specification and
    // a search index that disagrees with itself is a bug.
    collapsed.trim_matches(' ').to_string()
}

/// `<command-name>…</command-name>` and `<local-command-stdout>…`, stripped on
/// exactly the rule the bash version uses:
///
/// ```text
/// <command-[a-z-]*>[^<]*</command-[a-z-]*>
/// ```
///
/// The `[^<]*` is load-bearing and not an accident of writing a regex. A block
/// whose CONTENT contains a `<` is left alone, and real transcripts contain such
/// blocks: `<local-command-stdout>Usage: /cd <path></local-command-stdout>` is one,
/// and stripping it diverged from bash on a live transcript. Searching for the
/// closing tag anywhere would swallow more than the specification says.
fn strip_command_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    'outer: while let Some(lt) = rest.find('<') {
        let tail = &rest[lt..];
        for prefix in ["<local-command-", "<command-"] {
            let Some(after_open) = tail.strip_prefix(prefix) else {
                continue;
            };
            // [a-z-]* then '>'
            let name_len = after_open
                .find(|c: char| !(c.is_ascii_lowercase() || c == '-'))
                .unwrap_or(after_open.len());
            let Some(body) = after_open[name_len..].strip_prefix('>') else {
                continue;
            };
            // [^<]* : the content may not contain a '<' at all
            let content_len = body.find('<').unwrap_or(body.len());
            let closing = &body[content_len..];
            let close_prefix = format!("</{}", &prefix[1..]);
            let Some(after_close) = closing.strip_prefix(close_prefix.as_str()) else {
                continue;
            };
            let close_name_len = after_close
                .find(|c: char| !(c.is_ascii_lowercase() || c == '-'))
                .unwrap_or(after_close.len());
            let Some(remainder) = after_close[close_name_len..].strip_prefix('>') else {
                continue;
            };
            out.push_str(&rest[..lt]);
            out.push(' ');
            rest = remainder;
            continue 'outer;
        }
        out.push_str(&rest[..lt + 1]);
        rest = &rest[lt + 1..];
    }
    out.push_str(rest);
    out
}

/// Every value of one repeated key on this line, appended as it is found.
fn grab(line: &str, key: &str, out: &mut String) {
    let mut from = 0;
    while let Some(at) = line[from..].find(key) {
        let start = from + at + key.len();
        let cleaned = clean(&json_body(&line[start..]));
        if !cleaned.is_empty() {
            out.push_str(&cleaned);
            out.push(' ');
        }
        from = start;
    }
}

/// One line of a transcript, contributing whatever prose it holds.
pub fn extract_line(line: &str, out: &mut String) {
    if is_noise(line) || is_injected_attachment(line) {
        return;
    }
    grab(line, "\"type\":\"text\",\"text\":\"", out);
    grab(line, "\"role\":\"user\",\"content\":\"", out);
    // An attachment that survived the filter is something pasted, attached or
    // queued. These are its human-readable halves: the NAME of an attached file
    // (never its body, which is nested a level deeper and out of reach of these
    // patterns), an edited snippet, a queued prompt.
    if line.contains("\"attachment\":") {
        grab(line, "\"filename\":\"", out);
        grab(line, "\"prompt\":\"", out);
        grab(line, "\"snippet\":\"", out);
    }
}

/// A whole transcript's prose, as one line.
pub fn extract(text: &str) -> String {
    let mut out = String::new();
    for line in text.lines() {
        extract_line(line, &mut out);
    }
    out
}

/// The last few turns of an ENDED conversation, newest last.
///
/// An ended session has no screen to capture: what it has is the last things
/// that were said in it, which is what tells you whether it is the one you were
/// looking for. Returned as (who, what) so the caller paints it; "you" for your
/// turns, "claude" for its.
///
/// Read BACKWARDS, and it reaches back PAST the quota until one of your turns is
/// in view. A long stretch of tool calls puts a dozen tool results between two
/// prompts and every one of those is skipped, so the last six records of a
/// working session are routinely six things the agent said and nothing you
/// asked. The answer to "where did this one get to" needs the question in it.
pub fn last_turns(text: &str, want: usize, cols: usize) -> Vec<(&'static str, String)> {
    let mut out: Vec<(&'static str, String)> = Vec::new();
    let mut seen_you = false;
    for (n, line) in text.lines().rev().enumerate() {
        if n > 800 {
            break;
        }
        if is_noise(line) {
            continue;
        }
        let who = if line.contains("\"type\":\"assistant\"") {
            "claude"
        } else if line.contains("\"role\":\"user\"") {
            "you"
        } else {
            continue;
        };
        let Some(t) = first_text(line) else { continue };
        let t = clean(&t);
        if t.is_empty() {
            continue;
        }
        // Two lines a turn is enough to recognise one, and the preview pane is
        // short. A turn cut off says so rather than pretending it ended there.
        let cap = (cols * 2).saturating_sub(2);
        let t = if t.chars().count() > cap {
            format!("{}…", t.chars().take(cap).collect::<String>())
        } else {
            t
        };
        if who == "you" {
            seen_you = true;
        }
        out.push((who, t));
        if (out.len() >= want && seen_you) || out.len() >= want + 4 {
            break;
        }
    }
    out.reverse();
    out
}

/// The first piece of prose in a record, whichever shape it is stored in.
fn first_text(line: &str) -> Option<String> {
    for key in [
        "\"type\":\"text\",\"text\":\"",
        "\"role\":\"user\",\"content\":\"",
    ] {
        if let Some(at) = line.find(key) {
            return Some(json_body(&line[at + key.len()..]));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(line: &str) -> String {
        let mut s = String::new();
        extract_line(line, &mut s);
        s
    }

    #[test]
    fn a_plain_user_turn_and_an_assistant_text_block_are_kept() {
        assert_eq!(
            one(
                r#"{"type":"user","message":{"role":"user","content":"please fix the worktree bug"}}"#
            ),
            "please fix the worktree bug "
        );
        assert_eq!(
            one(
                r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Looking at it"}]}}"#
            ),
            "Looking at it "
        );
    }

    #[test]
    fn a_tool_result_is_not() {
        // file contents and command output are nearly all of a transcript
        let l = r#"{"type":"user","toolUseResult":{"n":1},"message":{"role":"user","content":[{"type":"tool_result","content":"SECRETFILEBODY"}]}}"#;
        assert_eq!(one(l), "");
    }

    #[test]
    fn nor_injected_turns() {
        assert_eq!(
            one(
                r#"{"type":"user","isMeta":true,"message":{"role":"user","content":"METACAVEAT"}}"#
            ),
            ""
        );
        assert_eq!(
            one(
                r#"{"type":"assistant","isSidechain":true,"message":{"role":"assistant","content":[{"type":"text","text":"SUB"}]}}"#
            ),
            ""
        );
        assert_eq!(
            one(
                r#"{"type":"attachment","attachment":{"type":"deferred_tools_delta","content":"EnterWorktree NOISE"}}"#
            ),
            ""
        );
    }

    #[test]
    fn an_attachment_type_that_merely_starts_the_same_is_not_injected() {
        // "plan_mode" must not swallow a hypothetical "plan_mode_extra"
        let l = r#"{"type":"attachment","attachment":{"type":"plan_mode_extra","filename":"/w/keep.py"}}"#;
        assert_eq!(one(l), "/w/keep.py ");
    }

    #[test]
    fn an_attached_file_contributes_its_name_but_never_its_body() {
        let l = r#"{"type":"attachment","attachment":{"type":"file","filename":"/w/keepme.py","content":{"type":"text","file":{"filePath":"/w/keepme.py","content":"BODY"}}}}"#;
        let got = one(l);
        assert!(got.contains("/w/keepme.py"), "{got}");
        assert!(!got.contains("BODY"), "{got}");
    }

    #[test]
    fn a_tool_calls_arguments_fall_out_for_free() {
        // tool_use carries its arguments under "input", never under "text"
        let l = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","name":"Bash","input":{"command":"echo TOOLINPUT"}}]}}"#;
        assert_eq!(one(l), "");
    }

    #[test]
    fn a_prompt_between_two_reminders_survives_both() {
        // the case a greedy strip gets wrong, which is why untag is shortest-match
        let l = r#"{"type":"user","message":{"role":"user","content":"<system-reminder>NOISEONE</system-reminder>REALPROMPT<system-reminder>NOISETWO</system-reminder>"}}"#;
        let got = one(l);
        assert!(got.contains("REALPROMPT"), "{got}");
        assert!(
            !got.contains("NOISEONE") && !got.contains("NOISETWO"),
            "{got}"
        );
    }

    #[test]
    fn a_command_block_is_stripped_but_only_when_its_content_has_no_angle_bracket() {
        let l = r#"{"type":"user","message":{"role":"user","content":"A<command-name>clear</command-name>B"}}"#;
        assert_eq!(one(l).trim(), "A B");
        // Found by differential on a live transcript: the bash rule is [^<]*, so a
        // block whose content contains '<' is left alone rather than swallowed.
        let l = r#"{"type":"user","message":{"role":"user","content":"A<local-command-stdout>Usage: /cd <path></local-command-stdout>B"}}"#;
        let got = one(l);
        assert!(got.contains("Usage: /cd <path>"), "{got}");
    }

    #[test]
    fn an_unterminated_reminder_takes_the_rest_of_the_line_with_it() {
        let l = r#"{"type":"user","message":{"role":"user","content":"KEEP<system-reminder>never closed"}}"#;
        assert_eq!(one(l).trim(), "KEEP");
    }

    #[test]
    fn escapes_become_spaces_so_no_control_char_reaches_a_row() {
        let l = r#"{"type":"user","message":{"role":"user","content":"line one\nline two\ttabbed[31m"}}"#;
        let got = one(l);
        assert!(got.contains("line one line two"), "{got}");
        assert!(!got.contains('\t') && !got.contains('\u{1b}'), "{got}");
    }

    #[test]
    fn the_blob_is_one_line_with_no_tabs_in_it() {
        let text = "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"a\"}}\n\
                    {\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"b\"}]}}\n";
        let got = extract(text);
        assert_eq!(got, "a b ");
        assert!(!got.contains('\n') && !got.contains('\t'));
    }

    #[test]
    fn an_escaped_quote_does_not_end_the_string_early() {
        let l =
            r#"{"type":"user","message":{"role":"user","content":"he said \"hello\" then left"}}"#;
        let got = one(l);
        assert!(got.contains("then left"), "{got}");
    }
}
