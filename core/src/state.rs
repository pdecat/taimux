//! What a session is doing, read off its screen.
//!
//! A port of the bash prototype's `_screen_awaits_input`, `_screen_is_working`,
//! `_pane_state` and `_merge_state`. These take the captured text as an argument
//! rather than capturing it themselves, which is what makes them testable at all:
//! the bash originals are the same shape for the same reason.
//!
//! This is the most fragile reading in the tool (it infers "working" from the
//! *shape* of an activity line, and that has broken once already), so the rules
//! are copied deliberately rather than improved.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Input,
    Run,
    Idle,
    Unknown,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            State::Input => "input",
            State::Run => "run",
            State::Idle => "idle",
            State::Unknown => "unknown",
        }
    }
}

/// Waiting for an answer.
///
/// Two checks, and both earn their place: on a narrow pane the footer wraps and
/// pushes the choice list off the bottom, leaving the footer as the only sign.
/// The footer is read over the last few lines only, and the choice list from the
/// LOWEST prompt line, because both of those strings turn up in ordinary
/// scrollback and a window of lines cannot be trusted.
pub fn awaits_input(screen: &str) -> bool {
    // Trailing blank lines go first, and that is not tidiness. `capture-pane`
    // pads its output to the full pane height, so a dialog sitting four lines up
    // is beyond "the last four lines" of the raw text. The bash version never had
    // to think about it because `$(...)` strips trailing newlines for free; the
    // daemon reads the pipe directly and does not. Missing this made every
    // session waiting for an answer read as idle, which is the one state that
    // must never be wrong.
    let mut lines: Vec<&str> = screen.lines().collect();
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    let tail = lines.len().saturating_sub(4);
    if lines[tail..]
        .iter()
        .any(|l| l.contains("Do you want to proceed?") || l.contains("Esc to cancel"))
    {
        return true;
    }
    // the lowest line carrying the prompt glyph is the one that owns the dialog
    if let Some(box_line) = lines.iter().rev().find(|l| l.contains('❯')) {
        if let Some(pos) = box_line.find("❯ ") {
            let after = &box_line[pos + "❯ ".len()..];
            let mut c = after.chars();
            if matches!((c.next(), c.next()), (Some(d), Some('.')) if d.is_ascii_digit()) {
                return true;
            }
        }
    }
    false
}

/// Mid-turn.
///
/// The activity line Claude keeps above the prompt box ends in an ellipsis and a
/// bracketed counter, `Twisting… (35s · ↓ 1.6k tokens)` or `(esc to interrupt)`,
/// where a FINISHED turn reads `Crunched for 9m 55s` with no brackets at all.
/// That difference is the whole test. Blank lines are dropped first so the last
/// eight lines are eight lines of content.
pub fn is_working(screen: &str) -> bool {
    screen
        .lines()
        .filter(|l| !l.trim().is_empty())
        .rev()
        .take(8)
        .any(|l| {
            // …<spaces>( followed by a digit or "esc "
            l.match_indices('…').any(|(i, _)| {
                let rest = l[i + '…'.len_utf8()..].trim_start();
                match rest.strip_prefix('(') {
                    Some(after) => {
                        after.starts_with("esc ") || after.starts_with(|c: char| c.is_ascii_digit())
                    }
                    None => false,
                }
            })
        })
}

/// The screen's own answer, in the order the bash version asks: a dialog wins
/// over an activity line, since it is the row that wants you.
pub fn classify(screen: &str) -> State {
    if awaits_input(screen) {
        State::Input
    } else if is_working(screen) {
        State::Run
    } else if screen.contains('❯') {
        State::Idle
    } else {
        State::Unknown
    }
}

/// The screen and the hook line, reconciled.
///
/// A dialog ON SCREEN outranks everything: it is the state that must never be
/// wrong and the one the hook cannot close, because a permission *granted* fires
/// no closing event and nothing ever rewrites the line.
///
/// The converse matters just as much. A screen positively showing an idle prompt
/// box, with no dialog over it, is proof a hook `input` has gone stale. Two were
/// found stuck that way on a live server, one 38 hours old, each making its pane
/// read as working forever and refuse every restart.
///
/// A hook `idle` goes stale the same way, and costs the same in the other
/// direction: an activity line with a live counter under it is the screen saying
/// "mid-turn" as positively as the prompt box says "idle", so a line still
/// reading `idle` is one that stopped being written, its turn's opening `run`
/// having never landed. Found on a pane working away at a two-minute turn whose
/// line was two days old: the list called it idle, and `restart` counted it
/// restartable. Only `idle` is overruled here: a `run` line, and an `input` line
/// the screen does not contradict, are the hook telling the list what the screen
/// cannot show, which is the whole reason it is written. A session streaming a
/// long answer shows no activity line at all while it does so.
pub fn merge(screen: State, hook: Option<&str>) -> State {
    if screen == State::Input {
        return State::Input;
    }
    if screen == State::Idle && hook == Some("input") {
        return State::Idle;
    }
    if screen == State::Run && hook == Some("idle") {
        return State::Run;
    }
    match hook {
        Some("run") => return State::Run,
        Some("idle") => return State::Idle,
        // a session mid-permission is working, as far as the list is concerned
        Some("input") => return State::Run,
        _ => {}
    }
    // No line, or one nobody recognises: a screen that could not be read reads as
    // idle, exactly as it always did.
    if screen == State::Unknown {
        State::Idle
    } else {
        screen
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_numbered_choice_list_is_a_session_asking() {
        let s = "some output\n  2. Yes, and don't ask again\n  3. No\n ❯ 1. Yes\n";
        assert!(awaits_input(s));
        assert_eq!(classify(s), State::Input);
    }

    #[test]
    fn the_footer_alone_is_enough_when_the_list_has_wrapped_off() {
        // a narrow pane pushes the choice list off the bottom
        assert!(awaits_input("blah\nblah\n Esc to cancel · Tab to amend\n"));
        assert!(awaits_input("Do you want to proceed?\n"));
    }

    #[test]
    fn the_blank_lines_capture_pane_pads_with_do_not_hide_the_dialog() {
        // Found by differential test against the bash version on a live pane
        // sitting on "Yes, I trust this folder": tmux pads to the pane height, so
        // the footer was four lines up but twenty lines from the end.
        let s = " ❯ No, exit\n   Yes, I trust this folder\n\n Enter to confirm · Esc to cancel\n"
            .to_string()
            + &"\n".repeat(20);
        assert!(awaits_input(&s));
        assert_eq!(classify(&s), State::Input);
    }

    #[test]
    fn those_strings_higher_up_the_scrollback_do_not_count() {
        // they turn up in ordinary output, which is why only the last lines are read
        let s = "Do you want to proceed?\n".to_string() + &"filler\n".repeat(20) + " ❯ ";
        assert!(!awaits_input(&s));
        assert_eq!(classify(&s), State::Idle);
    }

    #[test]
    fn an_activity_line_means_working_but_a_finished_turn_does_not() {
        assert!(is_working("✽ Twisting… (35s · ↓ 1.6k tokens)\n ❯ "));
        assert!(is_working("✽ Thinking… (esc to interrupt)\n ❯ "));
        // no brackets: the turn is over
        assert!(!is_working("Crunched for 9m 55s\n ❯ "));
        // an ellipsis with no counter after it is just prose
        assert!(!is_working("I will think about it…\n ❯ "));
    }

    #[test]
    fn a_dialog_outranks_an_activity_line() {
        let s = "✽ Twisting… (35s)\n  2. No\n ❯ 1. Yes\n";
        assert_eq!(classify(s), State::Input);
    }

    #[test]
    fn a_bare_prompt_is_idle_and_no_prompt_at_all_is_unknown() {
        assert_eq!(classify(" ❯ "), State::Idle);
        assert_eq!(classify("just some output\n"), State::Unknown);
        assert_eq!(classify(""), State::Unknown);
    }

    #[test]
    fn a_dialog_on_screen_beats_any_hook_line() {
        for hook in [None, Some("run"), Some("idle"), Some("input")] {
            assert_eq!(merge(State::Input, hook), State::Input);
        }
    }

    #[test]
    fn an_idle_screen_overrules_a_stale_hook_input() {
        // the 38-hour-old line that made a pane refuse every restart
        assert_eq!(merge(State::Idle, Some("input")), State::Idle);
    }

    #[test]
    fn a_working_screen_overrules_a_stale_hook_idle() {
        // The line a session's last turn closed with, left behind because the
        // opening `run` of the turn now on screen never arrived. Two days old on
        // the pane this was found on, which was mid-turn at the time.
        assert_eq!(merge(State::Run, Some("idle")), State::Run);
        // …and only that screen overrules it: every other reading still takes
        // the line at its word (the rest of them in the test below).
        assert_eq!(merge(State::Idle, Some("idle")), State::Idle);
    }

    #[test]
    fn otherwise_the_hook_is_taken_as_it_stands() {
        assert_eq!(merge(State::Idle, Some("run")), State::Run);
        assert_eq!(merge(State::Unknown, Some("idle")), State::Idle);
        // a session mid-permission is working, for the list's purposes
        assert_eq!(merge(State::Run, Some("input")), State::Run);
        assert_eq!(merge(State::Unknown, Some("input")), State::Run);
    }

    #[test]
    fn with_no_hook_the_screen_decides_and_unreadable_reads_as_idle() {
        assert_eq!(merge(State::Run, None), State::Run);
        assert_eq!(merge(State::Idle, None), State::Idle);
        assert_eq!(merge(State::Unknown, None), State::Idle);
        assert_eq!(merge(State::Unknown, Some("nonsense")), State::Idle);
    }
}
