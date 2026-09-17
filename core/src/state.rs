//! What a session is doing, read off its screen.
//!
//! A port of the bash prototype's `_screen_awaits_input`, `_screen_is_working`,
//! `_pane_state` and `_merge_state`. These take the captured text as an argument
//! rather than capturing it themselves, which is what makes them testable at all:
//! the bash originals are the same shape for the same reason.
//!
//! This is the most fragile reading in the tool (it infers what a turn is doing
//! from the *shape* of one line, and that has broken twice now), so the rules are
//! copied from the prototype deliberately rather than improved. Where one has
//! been changed since, `turn_marker` and the third overrule in `merge`, the live
//! screen that forced it is quoted in the comment.

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
/// Two checks, and both earn their place: on a narrow pane the choice list wraps
/// and pushes itself off the bottom, leaving the footer as the only sign, and
/// narrower still the footer wraps too. The footer is read over the last few
/// lines only, joined, and the choice list from the LOWEST prompt line, because
/// both of those strings turn up in ordinary scrollback and a window of lines
/// cannot be trusted.
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
    // Those lines are searched JOINED, not one at a time, because on a narrow
    // enough pane the footer itself wraps: `Esc to` on one line and `cancel` on
    // the next is the same footer, and reading them separately finds neither.
    // Joining can only add matches and never lose one, since a phrase that fits
    // inside a single line survives the join intact. Found on a 46-column pane
    // sitting on an unanswered question that the list was calling `run`, which is
    // the reading this whole function exists to prevent.
    let tail = lines.len().saturating_sub(4);
    let footer = lines[tail..]
        .iter()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if footer.contains("Do you want to proceed?") || footer.contains("Esc to cancel") {
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

/// What the turn line above the prompt box says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Turn {
    /// Still going: an ellipsis and a bracketed counter.
    Running,
    /// Over: a duration and a finishing time, and no brackets at all.
    Done,
}

/// Still going: `Twisting… (35s · ↓ 1.6k tokens)`, or `(esc to interrupt)`.
fn is_running_line(l: &str) -> bool {
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
}

/// Over: `✻ Crunched for 9m 55s · done 11:07 AM`.
///
/// Both halves are needed. A duration alone is ordinary prose, and the finishing
/// time alone is not a shape anything else writes. Sampled over the whole pane
/// list the verb varies freely (Churned, Sautéed, Cogitated, Brewed) and the
/// time reaches back through `done Monday 10:10 PM` to
/// `done Friday, Sep 4, 12:51 PM`, but ` for ` and `· done ` are in every one of
/// them. The leading glyph is deliberately not read: it animates while the turn
/// runs, so keying on it would be keying on a frame.
fn is_done_line(l: &str) -> bool {
    l.contains(" for ") && l.contains("· done ")
}

/// The lowest turn line on screen, if there is one.
///
/// Claude draws ONE of these per turn and rewrites it in place, from the running
/// shape to the finished one, so the lowest on screen belongs to the most recent
/// turn and no earlier turn can contradict it. That ordering is the whole reason
/// this returns a single answer rather than two independent booleans: a finished
/// turn still shows its line while the next turn is being typed, and the next
/// turn's line appears BELOW it.
///
/// Read over the last sixteen lines of content rather than the whole capture,
/// because both shapes turn up in ordinary output: a session discussing this very
/// code prints them. Sixteen because the turn line is nowhere near the bottom of
/// the screen. Under it Claude tucks a tip row and a token count, then the title
/// rule, the prompt box, its own rule and two status rows. Measured across 39
/// live panes it sat 2 to 9 lines up. The eight this started at was one line
/// short of the common case, which is how a session twenty-six minutes into a
/// turn read as idle off its screen, and left the hook line carrying it alone.
pub fn turn_marker(screen: &str) -> Option<Turn> {
    screen
        .lines()
        .filter(|l| !l.trim().is_empty())
        .rev()
        .take(16)
        .find_map(|l| {
            if is_running_line(l) {
                Some(Turn::Running)
            } else if is_done_line(l) {
                Some(Turn::Done)
            } else {
                None
            }
        })
}

/// Mid-turn.
pub fn is_working(screen: &str) -> bool {
    turn_marker(screen) == Some(Turn::Running)
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
/// restartable.
///
/// A `run` line goes stale too, and that one is the worst of the three, because
/// nothing ever clears it: the pane sits in the working list for good and
/// `restart` refuses it. Found on a session Claude had put in the background
/// (`sessionKind: "bg"`), whose own transcript records `taimux hook` running on
/// every `Stop` with no error while the line it should have written never
/// appeared, six and a half hours of it. The screen answers that one too, but
/// only positively: a FINISHED turn line, with an idle prompt box under it and no
/// later turn line anywhere below, is the screen saying the turn is over as
/// plainly as the box says no dialog is up. The absence of a turn line is NOT
/// that answer and is left to the hook, which is the case this must not break: a
/// session streaming a long reply can show no turn line at all while it does so,
/// and then `run` is the only thing that knows.
pub fn merge(screen: &str, hook: Option<&str>) -> State {
    let screen_state = classify(screen);
    if screen_state == State::Input {
        return State::Input;
    }
    if screen_state == State::Idle && hook == Some("input") {
        return State::Idle;
    }
    if screen_state == State::Run && hook == Some("idle") {
        return State::Run;
    }
    if screen_state == State::Idle && hook == Some("run") && turn_marker(screen) == Some(Turn::Done)
    {
        return State::Idle;
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
    if screen_state == State::Unknown {
        State::Idle
    } else {
        screen_state
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
    fn a_footer_that_wrapped_across_two_lines_is_still_a_footer() {
        // 46 columns, live: the question's own footer breaks mid-phrase, so
        // neither line carries it and the pane read as working instead of asking.
        let s = "──────────────────────────────\n\
                 \x20 6. Chat about this\n\
                 \n\
                 Enter to select · ↑/↓ to navigate · Esc to\n\
                 cancel\n";
        assert!(awaits_input(s));
        assert_eq!(classify(s), State::Input);
        // and the join does not invent one out of two unrelated lines
        assert!(!awaits_input("nothing to escape here\ncancel the order\n"));
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
        assert!(!is_working("✻ Crunched for 9m 55s · done 11:07 AM\n ❯ "));
        // an ellipsis with no counter after it is just prose
        assert!(!is_working("I will think about it…\n ❯ "));
    }

    #[test]
    fn the_rows_claude_tucks_under_the_turn_line_do_not_bury_it() {
        // Live capture shape: the tip and the token count sit under the activity
        // line, then the title rule, the box, its rule and two status rows, which
        // put it NINE lines of content up. Read over eight it vanished, and a
        // session twenty-six minutes into a turn read idle off its screen.
        let s = "✽ Symbioting… (26m 38s · ↓ 19.7k tokens)\n\
                 \x20 ⎿  Tip: Use git worktrees to run multiple sessions\n\
                 \x20                                    253088 tokens\n\
                 ──── gitlab-webhook-receiver: fix-ruff-ci-errors ─\n\
                 ❯ \n\
                 ─────────────────────────────────────────────────\n\
                 \x20 Opus 5 (1M, max) v2.1.274  [█░░] 25%  5h 12%\n\
                 \x20 ⏵⏵ auto mode on (shift+tab to cycle) · 11 agents\n";
        assert_eq!(turn_marker(s), Some(Turn::Running));
        assert_eq!(classify(s), State::Run);
    }

    #[test]
    fn a_finished_turn_line_is_read_whatever_the_verb_and_however_old() {
        for l in [
            "✻ Churned for 1m 19s · done 11:07 AM",
            "✻ Sautéed for 10m 29s · done 11:19 PM",
            "✻ Cogitated for 13m 58s · done Friday 11:34 AM",
            "✻ Brewed for 6m 54s · done Thursday, Aug 27, 2:52 PM",
        ] {
            assert_eq!(turn_marker(&format!("{l}\n ❯ ")), Some(Turn::Done), "{l}");
        }
        // half of the shape is not the shape: prose, and the status row
        assert_eq!(turn_marker("it ran for 3 hours\n ❯ "), None);
        assert_eq!(turn_marker("· done deal\n ❯ "), None);
    }

    #[test]
    fn the_lowest_turn_line_is_the_one_that_counts() {
        // The turn now running, under the one it followed: Claude writes a fresh
        // line per turn and the previous turn's finished one stays on screen.
        let s = "✻ Churned for 30s · done 9:56 AM\n> do the next thing\n✽ Thinking… (2s)\n ❯ ";
        assert_eq!(turn_marker(s), Some(Turn::Running));
        // and the other way round, which is the whole point: a turn that ended
        assert_eq!(
            turn_marker("✽ Thinking… (2s)\n✻ Churned for 30s · done 9:56 AM\n ❯ "),
            Some(Turn::Done)
        );
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

    // The four screens the merge rules turn on. FINISHED and RUNNING carry a turn
    // line; STREAMING is the one that carries none, which is a real screen and not
    // a contrived one: a session part way through a long reply shows exactly this.
    const DIALOG: &str = "✽ Twisting… (35s)\n  2. No\n ❯ 1. Yes\n";
    const FINISHED: &str = "✻ Crunched for 9m 55s · done 11:07 AM\n ❯ \n";
    const RUNNING: &str = "✽ Twisting… (35s · ↓ 1.6k tokens)\n ❯ \n";
    const STREAMING: &str = "…and that is the third reason it cannot work.\n ❯ \n";
    const UNREADABLE: &str = "just some output\n";

    #[test]
    fn a_dialog_on_screen_beats_any_hook_line() {
        for hook in [None, Some("run"), Some("idle"), Some("input")] {
            assert_eq!(merge(DIALOG, hook), State::Input);
        }
    }

    #[test]
    fn an_idle_screen_overrules_a_stale_hook_input() {
        // the 38-hour-old line that made a pane refuse every restart
        assert_eq!(merge(FINISHED, Some("input")), State::Idle);
        assert_eq!(merge(STREAMING, Some("input")), State::Idle);
    }

    #[test]
    fn a_working_screen_overrules_a_stale_hook_idle() {
        // The line a session's last turn closed with, left behind because the
        // opening `run` of the turn now on screen never arrived. Two days old on
        // the pane this was found on, which was mid-turn at the time.
        assert_eq!(merge(RUNNING, Some("idle")), State::Run);
        // …and only that screen overrules it: every other reading still takes
        // the line at its word (the rest of them in the test below).
        assert_eq!(merge(FINISHED, Some("idle")), State::Idle);
    }

    #[test]
    fn a_finished_turn_on_screen_overrules_a_stale_hook_run() {
        // The line a backgrounded session left behind: six and a half hours at
        // `run` with the pane sitting at an empty prompt box under a turn that
        // had plainly ended, and `restart` refusing it the whole time.
        assert_eq!(merge(FINISHED, Some("run")), State::Idle);
    }

    #[test]
    fn a_screen_with_no_turn_line_still_leaves_run_alone() {
        // The case the rule above must not eat. A session part way through a long
        // reply shows no turn line at all, so the screen has nothing to say and
        // the hook is the only thing that knows a turn is in flight.
        assert_eq!(merge(STREAMING, Some("run")), State::Run);
        assert_eq!(merge(UNREADABLE, Some("run")), State::Run);
    }

    #[test]
    fn otherwise_the_hook_is_taken_as_it_stands() {
        assert_eq!(merge(UNREADABLE, Some("idle")), State::Idle);
        // a session mid-permission is working, for the list's purposes
        assert_eq!(merge(RUNNING, Some("input")), State::Run);
        assert_eq!(merge(UNREADABLE, Some("input")), State::Run);
    }

    #[test]
    fn with_no_hook_the_screen_decides_and_unreadable_reads_as_idle() {
        assert_eq!(merge(RUNNING, None), State::Run);
        assert_eq!(merge(FINISHED, None), State::Idle);
        assert_eq!(merge(STREAMING, None), State::Idle);
        assert_eq!(merge(UNREADABLE, None), State::Idle);
        assert_eq!(merge(UNREADABLE, Some("nonsense")), State::Idle);
    }
}
