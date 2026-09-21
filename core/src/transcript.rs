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
//! `input`, never under `text`. **Their URLs are put back**, and nothing else
//! about them is: see `tool_urls`.

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
pub fn is_noise(line: &str) -> bool {
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
/// `\\` and `\"` become spaces, which is the bash version's approximation and
/// affordable here: this is a search index, not a parser. Neither ends the
/// string, which is the only thing that actually has to be right.
///
/// **One pass over the body, not three over the line.** This used to park both
/// escape forms on placeholder characters, look for the closing quote, then put
/// them back, and every one of those three steps copied everything from the
/// match to the END OF THE LINE. A transcript record holds many text blocks on
/// one line, so the cost was quadratic in the line and the index spent most of
/// its time copying text it was about to throw away: 978 MB of transcripts took
/// 14s to extract, and 1.6s after this. Byte for byte the same output, which the
/// tests below pin.
pub fn json_body(p: &str) -> String {
    let b = p.as_bytes();
    let mut out = String::new();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'"' => break,
            b'\\' if i + 1 < b.len() && (b[i + 1] == b'\\' || b[i + 1] == b'"') => {
                out.push(' ');
                i += 2;
            }
            _ => {
                // Copy whole characters: a body carries whatever was said, and a
                // string cut inside a character is not text any more.
                let start = i;
                i += 1;
                while i < b.len() && (b[i] & 0xC0) == 0x80 {
                    i += 1;
                }
                out.push_str(&p[start..i]);
            }
        }
    }
    out
}

/// Everything between the first opening tag and ITS closing tag, over and over.
///
/// The shortest match, which a regex `gsub` could not give: a prompt can sit
/// BETWEEN two `<system-reminder>` blocks, and a greedy match would swallow it
/// along with them.
pub fn untag(mut s: String, open: &str, close: &str) -> String {
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
pub fn clean(s: &str) -> String {
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
pub fn grab(line: &str, key: &str, out: &mut String) {
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

/// The schemes a URL is recognised by. Nothing else is: a bare `www.` or a
/// `git@host:` is not something you would type into the picker expecting a link
/// back, and every extra pattern is another way for prose to be mistaken for one.
const SCHEMES: [&str; 2] = ["https://", "http://"];

/// Where the URL at the start of `s` ends.
///
/// It stops only at characters a URL cannot hold: whitespace, and the handful
/// RFC 3986 excludes outright (`"`, `<`, `>`, `` ` ``, `\`, `{`, `}`, `|`, `^`).
/// In a transcript that is also what keeps it inside its own JSON string, since
/// the quote that would end the string, and every escape that could smuggle a
/// control character in, both begin with one of them.
///
/// Deliberately GENEROUS at the tail. A URL cut short is one that a search for
/// the whole thing no longer finds, which is the failure that matters here,
/// while a few characters of trailing punctuation only make a snippet untidy.
/// So the obvious sentence enders are trimmed back off and nothing else is
/// guessed at: a closing bracket may well be part of the address.
fn url_len(s: &str) -> usize {
    let n = s
        .find(|c: char| {
            c.is_whitespace() || matches!(c, '"' | '<' | '>' | '`' | '\\' | '{' | '}' | '|' | '^')
        })
        .unwrap_or(s.len());
    s[..n]
        .trim_end_matches(['.', ',', ';', ':', '!', '?'])
        .len()
}

/// Every URL in `hay`, in the order they appear.
fn urls(hay: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(at) = hay[from..].find("http") {
        let at = from + at;
        let rest = &hay[at..];
        match SCHEMES.iter().find(|s| rest.starts_with(**s)) {
            Some(scheme) => {
                let n = url_len(rest);
                if n > scheme.len() {
                    out.push(&rest[..n]);
                }
                from = at + n.max(1);
            }
            // "http" inside a word: step past it rather than past the word, so
            // a URL butted up against it is still found.
            None => from = at + "http".len(),
        }
    }
    out
}

/// The URLs in this record's tool-call arguments, each appended once.
///
/// The arguments themselves stay out, for the reason they always have: they
/// carry whole file bodies and shell command lines, and indexing them drowns
/// the prose they sit beside. Their URLs are the exception, and they earn it.
/// A URL is short, it is specific, and it is exactly the thing that sends you
/// looking for a session two days later, so the page an agent FETCHED, or the
/// endpoint a command called, is worth as much as the ones either of you typed.
///
/// `"input":{` can only ever be a real JSON key here. Inside a string every
/// quote is escaped, so no prose can spell it and no `<system-reminder>` riding
/// inside a turn can either, which is what keeps this away from the injected
/// text the rest of the module works to drop. Everything from the first one to
/// the end of the record is searched, the arguments being the tail of an
/// assistant record, and a URL the prose beside them already contributed is
/// skipped rather than stored twice.
fn tool_urls(line: &str, prose: &str) -> String {
    let Some(at) = line.find("\"input\":{") else {
        return String::new();
    };
    let mut out = String::new();
    for u in urls(&line[at..]) {
        if prose.contains(u) || out.contains(u) {
            continue;
        }
        out.push_str(u);
        out.push(' ');
    }
    out
}

/// One line of a transcript, contributing whatever prose it holds.
pub fn extract_line(line: &str, out: &mut String) {
    if is_noise(line) || is_injected_attachment(line) {
        return;
    }
    let at = out.len();
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
    // Last, and against what this line has just contributed, so a link that was
    // both fetched and talked about is stored once.
    let extra = tool_urls(line, &out[at..]);
    out.push_str(&extra);
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
pub fn first_text(line: &str) -> Option<String> {
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

    /// …except their URLs, which are the one part of a call worth remembering:
    /// the page that was fetched, the endpoint that was called.
    #[test]
    fn a_tool_calls_urls_do_not() {
        let l = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","name":"WebFetch","input":{"url":"https://docs.cloud.google.com/compute/docs/machine-resource","prompt":"WHATTHEPROMPTSAID"}}]}}"#;
        let got = one(l);
        assert!(
            got.contains("https://docs.cloud.google.com/compute/docs/machine-resource"),
            "{got}"
        );
        // and only the URL: the rest of the arguments stay out
        assert!(!got.contains("WHATTHEPROMPTSAID"), "{got}");

        // a command line is arguments too, and the endpoint it called counts
        let l = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","name":"Bash","input":{"command":"curl -s https://api.example.net/v1/things | jq .","description":"CALLIT"}}]}}"#;
        let got = one(l);
        assert!(got.contains("https://api.example.net/v1/things"), "{got}");
        assert!(!got.contains("CALLIT"), "{got}");
    }

    /// A tool RESULT is still dropped whole, URLs and all: that is a fetched
    /// page's own content, and one of those carries hundreds of links.
    #[test]
    fn a_tool_results_urls_stay_out() {
        let l = r#"{"type":"user","toolUseResult":{"n":1},"message":{"role":"user","content":[{"type":"tool_result","content":"see https://spam.example.com/one and https://spam.example.com/two"}]}}"#;
        assert_eq!(one(l), "");
    }

    /// A link that was both talked about and fetched is stored once, so the
    /// preview does not show the same hit twice.
    #[test]
    fn a_url_in_the_prose_beside_the_call_is_not_stored_twice() {
        let l = concat!(
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"reading https://example.com/page now"},"#,
            r#"{"type":"tool_use","name":"WebFetch","input":{"url":"https://example.com/page"}}]}}"#
        );
        let got = one(l);
        assert_eq!(got.matches("https://example.com/page").count(), 1, "{got}");
    }

    #[test]
    fn a_url_ends_where_a_url_can_no_longer_go() {
        // the JSON quote that closes the string, and the escape before it
        assert_eq!(
            urls(r#""url":"https://a.example/b?c=1&d=2""#),
            ["https://a.example/b?c=1&d=2"]
        );
        assert_eq!(
            urls(r#"said \"https://a.example/b\" once"#),
            ["https://a.example/b"]
        );
        // a sentence's full stop is not part of the address; a path's is
        assert_eq!(
            urls("see https://a.example/b. Then"),
            ["https://a.example/b"]
        );
        assert_eq!(
            urls("see https://a.example/b.html now"),
            ["https://a.example/b.html"]
        );
        // a closing bracket may well be, so it is kept: cutting it would lose
        // the hit for anyone searching the whole address
        assert_eq!(
            urls("(https://a.example/b_(c)"),
            ["https://a.example/b_(c)"]
        );
        // "http" inside a word is not a scheme, and does not hide the one after it
        assert_eq!(urls("httpd https://a.example/b"), ["https://a.example/b"]);
        // a scheme with nothing after it is not an address
        assert!(urls("https:// and http://").is_empty());
        assert!(urls("nothing here at all").is_empty());
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
