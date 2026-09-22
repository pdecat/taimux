//! What a transcript says about the turn a session is on.
//!
//! The hook line is the primary reading, and it has one hole no hook can fill:
//! **an interrupt fires nothing.** `Stop` does not run when the user stopped the
//! turn (the hooks reference says so, and a probe against 2.1.280 confirmed it
//! three ways: Esc part way through a reply, Esc on a permission prompt, and not
//! even an `idle_prompt` notification a minute later), so the line goes on
//! reading `run`, or `input`, until the next prompt is typed. Across twelve
//! transcripts over two days that was about fifty turns, each one a session the
//! list called working after it had been told to stop.
//!
//! The transcript records it at once and in a fixed shape, so the reader asks it
//! one question: which is the newest record that opened or closed a turn, and when
//! was it written? Set against the hook line's own mtime, that says which of the
//! two knows more. `state::correct` makes that call; this only reads.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// A record that opens or closes a turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// A turn opened by a prompt somebody typed. That also runs
    /// `UserPromptSubmit`, so this only matters when that hook's line is missing.
    Prompt,
    /// The user stopped the turn. Nothing else, anywhere, says so.
    Interrupt,
    /// The turn ran to its end, an API error included: Claude writes the same
    /// duration record either way.
    Over,
}

/// What one transcript line says about the turn, if anything.
///
/// Matched on the exact serialisation Claude writes rather than parsed, the same
/// trade the rest of this crate makes, and each shape was checked against real
/// transcripts before it was trusted:
///
/// - an **interrupt** is a user record whose message OPENS with
///   `[Request interrupted by user` (`]`, or ` for tool use]` when a tool was
///   refused or cancelled). Anchored on the message itself, because a tool
///   result carries a content array of its own, serialised exactly the same way,
///   and a session reading transcripts puts the phrase in one;
/// - a **prompt** is one somebody typed, `"origin":{"kind":"human"}`. Slash
///   commands, their output, a compaction summary and a message queued behind a
///   running tool carry no origin, and none of them opens a turn. Nor, reliably,
///   does a `task-notification`: a background task killed by a restart is
///   reported into the resumed conversation and nothing answers it, which is how
///   a session idle for five hours first read as working here. The ones that do
///   open a turn run `UserPromptSubmit` like a typed prompt, so the line has them
///   already;
/// - the **end** of a turn is the `stop_hook_summary` or `turn_duration` system
///   record, the second written after an API error too.
///
/// A sidechain record belongs to a subagent, not to the turn on screen.
pub fn event_of(line: &str) -> Option<Event> {
    if line.contains("\"isSidechain\":true") {
        return None;
    }
    match crate::json::field(line, "type").as_str() {
        "user" => {
            if line.contains(
                "\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"[Request interrupted by user",
            ) || line.contains("\"message\":{\"role\":\"user\",\"content\":\"[Request interrupted by user")
            {
                Some(Event::Interrupt)
            } else if line.contains("\"origin\":{\"kind\":\"human\"") {
                Some(Event::Prompt)
            } else {
                None
            }
        }
        "system" => match crate::json::field(line, "subtype").as_str() {
            "turn_duration" | "stop_hook_summary" => Some(Event::Over),
            _ => None,
        },
        _ => None,
    }
}

/// The newest turn event in some transcript text, with when it was written, in
/// epoch milliseconds.
///
/// `None` when the text holds none, which a long turn's tail legitimately can:
/// the prompt is then further back than was read, and every tool call since has
/// been keeping the hook line current anyway.
pub fn last_event(text: &str) -> Option<(Event, i64)> {
    text.lines().rev().find_map(|line| {
        let ev = event_of(line)?;
        let at = epoch_ms(&crate::json::field(line, "timestamp"))?;
        Some((ev, at))
    })
}

/// How much of a transcript's end is read. The records this looks for are
/// small and sit at the very end when they matter at all: an interrupt is the
/// last thing a stopped turn writes, and a turn's closing pair is its last two.
pub const TAIL: u64 = 64 * 1024;

/// The last `max` bytes of a file, from the first whole line in them.
pub fn read_tail(path: &Path, max: u64) -> Option<String> {
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    let start = len.saturating_sub(max);
    f.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = Vec::with_capacity((len - start) as usize);
    f.read_to_end(&mut buf).ok()?;
    let mut text = String::from_utf8_lossy(&buf).into_owned();
    // A window that starts mid-file starts mid-line, and possibly mid-character.
    // Both are confined to the partial first line, which goes.
    if start > 0 {
        match text.find('\n') {
            Some(i) => {
                text.drain(..=i);
            }
            None => return Some(String::new()),
        }
    }
    Some(text)
}

/// The newest turn event in the transcript at `path`.
pub fn last_event_in(path: &Path) -> Option<(Event, i64)> {
    last_event(&read_tail(path, TAIL)?)
}

/// An ISO 8601 UTC timestamp to epoch milliseconds, which is all claude writes:
/// `2026-09-02T01:23:45.678Z`.
///
/// Hand-rolled rather than shelling to `date -d`, which is what bash did per
/// pane. Only the shape claude actually emits is accepted, with or without the
/// fraction; anything else reads as "no answer", and every caller treats that as
/// not-evidence rather than as busy.
pub fn epoch_ms(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 19
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
    {
        return None;
    }
    let n = |a: usize, z: usize| s.get(a..z)?.parse::<i64>().ok();
    let (y, mo, d) = (n(0, 4)?, n(5, 7)?, n(8, 10)?);
    let (h, mi, sec) = (n(11, 13)?, n(14, 16)?, n(17, 19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) {
        return None;
    }
    // The fraction, to the millisecond: `.678Z` is 678, `.6Z` is 600.
    let mut ms = 0;
    if b.get(19) == Some(&b'.') {
        let digits: String = s[20..].chars().take_while(|c| c.is_ascii_digit()).collect();
        let three: String = digits.chars().chain("000".chars()).take(3).collect();
        ms = three.parse::<i64>().ok()?;
    }
    Some((days_from_civil(y, mo, d) * 86400 + h * 3600 + mi * 60 + sec) * 1000 + ms)
}

/// Days since 1970-01-01 for a civil date. Howard Hinnant's algorithm, which is
/// exact for every date and has no calendar table to get wrong.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

#[cfg(test)]
mod tests {
    use super::*;

    // Real records, trimmed of the fields nothing here reads. The shapes, and the
    // ORDER of their keys, are what 2.1.280 writes.
    const TYPED: &str = r#"{"parentUuid":null,"isSidechain":false,"promptId":"p1","type":"user","message":{"role":"user","content":"Write a story. No tools."},"uuid":"u1","timestamp":"2026-09-22T21:30:34.222Z","permissionMode":"default","origin":{"kind":"human"},"promptSource":"typed","turnOrigin":"human"}"#;
    const NOTIFIED: &str = r#"{"isSidechain":false,"type":"user","message":{"role":"user","content":"<task-notification>\n<task-id>b1</task-id>"},"timestamp":"2026-09-22T16:53:07.499Z","origin":{"kind":"task-notification"},"promptSource":"system"}"#;
    const SCHEDULED: &str = r#"{"isSidechain":false,"type":"user","message":{"role":"user","content":"Watch task wakeup"},"isMeta":true,"timestamp":"2026-09-22T10:00:00.000Z","promptSource":"system","turnOrigin":"scheduled"}"#;
    const STOPPED: &str = r#"{"parentUuid":"a","isSidechain":false,"promptId":"p1","type":"user","message":{"role":"user","content":[{"type":"text","text":"[Request interrupted by user]"}]},"uuid":"u2","timestamp":"2026-09-22T21:30:37.289Z","interruptedMessageId":"msg_1"}"#;
    const REFUSED: &str = r#"{"isSidechain":false,"type":"user","message":{"role":"user","content":[{"type":"text","text":"[Request interrupted by user for tool use]"}]},"timestamp":"2026-09-22T21:35:10.000Z"}"#;
    const SUMMARY: &str = r#"{"parentUuid":"b","isSidechain":false,"type":"system","subtype":"stop_hook_summary","hookCount":1,"timestamp":"2026-09-22T21:31:20.975Z"}"#;
    const DURATION: &str = r#"{"parentUuid":"c","isSidechain":false,"type":"system","subtype":"turn_duration","durationMs":16582,"timestamp":"2026-09-22T21:31:20.977Z"}"#;

    #[test]
    fn each_turn_record_is_recognised() {
        assert_eq!(event_of(TYPED), Some(Event::Prompt));
        assert_eq!(event_of(STOPPED), Some(Event::Interrupt));
        assert_eq!(event_of(REFUSED), Some(Event::Interrupt));
        assert_eq!(event_of(SUMMARY), Some(Event::Over));
        assert_eq!(event_of(DURATION), Some(Event::Over));
    }

    #[test]
    fn a_notification_or_a_wakeup_is_not_taken_for_a_prompt() {
        // Written into a resumed conversation for a background task the restart
        // killed, and never answered: taking it for a prompt made a session five
        // hours idle read as working. The ones that do open a turn fire
        // `UserPromptSubmit` of their own.
        assert_eq!(event_of(NOTIFIED), None);
        assert_eq!(event_of(SCHEDULED), None);
    }

    #[test]
    fn records_that_open_no_turn_are_not_prompts() {
        // A tool result, a slash command, a compaction summary, and a message
        // queued behind a running tool (delivered with the result, mid-turn).
        for l in [
            r#"{"type":"user","message":{"role":"user","content":[{"tool_use_id":"t1","type":"tool_result","content":"ok"}]},"timestamp":"2026-09-22T21:00:00.000Z"}"#,
            r#"{"type":"user","message":{"role":"user","content":"<command-name>/model</command-name>"},"timestamp":"2026-09-22T21:00:00.000Z"}"#,
            r#"{"type":"user","message":{"role":"user","content":"This session is being continued from a previous conversation"},"isCompactSummary":true,"timestamp":"2026-09-22T21:00:00.000Z"}"#,
            r#"{"type":"user","message":{"role":"user","content":[{"tool_use_id":"t2","type":"tool_result","content":"x"},{"type":"text","text":"and draft a reply"}]},"timestamp":"2026-09-22T21:00:00.000Z"}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"done"}],"stop_reason":"end_turn"},"timestamp":"2026-09-22T21:00:00.000Z"}"#,
            r#"{"type":"system","subtype":"away_summary","timestamp":"2026-09-22T21:00:00.000Z"}"#,
        ] {
            assert_eq!(event_of(l), None, "{l}");
        }
    }

    #[test]
    fn quoting_the_interrupt_phrase_is_not_an_interrupt() {
        // A session grepping transcripts prints the phrase in a tool result. Its
        // content opens with the result, so the anchor does not reach it.
        let l = r#"{"type":"user","message":{"role":"user","content":[{"tool_use_id":"t3","type":"tool_result","content":[{"type":"text","text":"[Request interrupted by user]"}]}]},"timestamp":"2026-09-22T21:00:00.000Z"}"#;
        assert_eq!(event_of(l), None);
    }

    #[test]
    fn a_subagents_records_say_nothing_about_the_turn_on_screen() {
        let l = TYPED.replace("\"isSidechain\":false", "\"isSidechain\":true");
        assert_eq!(event_of(&l), None);
    }

    #[test]
    fn the_newest_event_wins_and_carries_its_time() {
        let t = [
            TYPED,
            SUMMARY,
            DURATION,
            TYPED,
            STOPPED,
            r#"{"type":"last-prompt"}"#,
        ]
        .join("\n");
        assert_eq!(
            last_event(&t),
            Some((
                Event::Interrupt,
                epoch_ms("2026-09-22T21:30:37.289Z").unwrap()
            ))
        );
        assert_eq!(last_event(TYPED).map(|(e, _)| e), Some(Event::Prompt));
        assert_eq!(last_event("{}\n{\"type\":\"assistant\"}"), None);
    }

    #[test]
    fn a_tail_starts_at_its_first_whole_line() {
        let dir = std::env::temp_dir().join(format!("taimux-turn-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("t.jsonl");
        std::fs::write(&f, "first line\nsecond line\nthird\n").unwrap();
        assert_eq!(
            read_tail(&f, 1000).as_deref(),
            Some("first line\nsecond line\nthird\n")
        );
        // 15 bytes back lands inside "second line": that line goes, whole
        assert_eq!(read_tail(&f, 15).as_deref(), Some("third\n"));
        assert_eq!(read_tail(&dir.join("absent"), 10), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_timestamp_becomes_epoch_milliseconds() {
        assert_eq!(epoch_ms("2026-09-02T01:23:45.678Z"), Some(1788312225678));
        assert_eq!(epoch_ms("2026-09-02T01:23:45Z"), Some(1788312225000));
        assert_eq!(epoch_ms("2026-09-02T01:23:45.6Z"), Some(1788312225600));
        assert_eq!(epoch_ms("1970-01-01T00:00:00Z"), Some(0));
        // a leap day, which a hand-rolled calendar is where it goes wrong
        assert_eq!(epoch_ms("2024-02-29T00:00:00Z"), Some(1709164800000));
    }

    #[test]
    fn an_unparseable_timestamp_is_no_answer() {
        assert_eq!(epoch_ms(""), None);
        assert_eq!(epoch_ms("yesterday"), None);
        assert_eq!(epoch_ms("2026-09-02 01:23:45"), None);
        assert_eq!(epoch_ms("2026-13-02T01:23:45Z"), None);
    }
}
