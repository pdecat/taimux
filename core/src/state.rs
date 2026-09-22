//! What a session is doing, read off its screen.
//!
//! A port of the bash prototype's `_screen_awaits_input`, `_screen_is_working`,
//! `_pane_state` and `_merge_state`. These take the captured text as an argument
//! rather than capturing it themselves, which is what makes them testable at all:
//! the bash originals are the same shape for the same reason.
//!
//! This is the most fragile reading in the tool (it infers what a turn is doing
//! from the *shape* of one line, and that has broken twice now), so the rules
//! started as a faithful copy of the prototype. Every one changed since has the
//! live screen that forced it quoted in its comment, and since Claude Code's
//! fullscreen renderer most of them have: it draws no turn line at all while a
//! reply streams, and lets its own viewport scroll away from the prompt box.
//!
//! The screen is the corrective now, not the source. The hook line and the
//! transcript (`hook::current`) say what a session is doing; the screen gets the
//! last word only where it says something positively.

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
            if is_choice(&box_line[pos + "❯ ".len()..]) {
                return true;
            }
        }
    }
    false
}

/// A rule: the full-width line Claude draws above and below its prompt box.
/// The top one can carry the session's name, `──── project: title ─`, and on a
/// narrow pane that label leaves only four dashes in front of it.
fn is_rule(l: &str) -> bool {
    l.trim_start().starts_with("──")
}

/// The line of the LIVE prompt box, the one you type into, if the screen shows it.
///
/// Not simply the lowest `❯`, because the conversation shows every prompt you
/// sent with the same glyph in front of it. Normally the box is still lower, but
/// the fullscreen renderer lets its viewport scroll up, and then the lowest `❯`
/// on screen is a prompt from an hour ago: found on a pane scrolled away from a
/// permission prompt it had been holding for 35 hours, which the list called
/// idle, because this used to be `screen.contains('❯')`. Reproduced by opening a
/// permission prompt and pressing Page Up.
///
/// The box is the one `❯` with a rule directly above it, on every pane that drew
/// one when this was measured: 29 here at 54 to 213 columns, and three on two
/// other hosts. A sent prompt has conversation above it, and a dialog's cursor
/// has the question. A scrolled viewport says so on its last line,
/// `N new messages (ctrl+End) ↓` or `Jump to bottom (ctrl+End) ↓`, and has no
/// box at all.
pub fn live_box(lines: &[&str]) -> Option<usize> {
    let last = lines.iter().rev().find(|l| !l.trim().is_empty())?;
    if last.contains("(ctrl+End)") {
        return None;
    }
    (0..lines.len()).rev().find(|&i| {
        lines[i].trim_start().starts_with('❯')
            && lines[..i]
                .iter()
                .rev()
                .find(|l| !l.trim().is_empty())
                .is_some_and(|l| is_rule(l))
    })
}

/// What the turn line above the prompt box says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Turn {
    /// Still going: an ellipsis and a bracketed counter.
    Running,
    /// Over: a duration and a finishing time, and no brackets at all. Or
    /// stopped: `⎿  Interrupted · What should Claude do instead?`.
    Done,
    /// Over, with work it started still in flight: `✻ Sautéed for 11s · done
    /// 11:39 PM · 1 shell still running`. The session will wake itself when that
    /// work reports back, so as far as the list is concerned it is busy.
    Background,
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

/// Stopped by the user. An interrupt fires no hook at all, and when it cuts off
/// a reply Claude draws no finished turn line either, only this, so without it
/// the screen had nothing to say about a turn that was plainly over and the hook
/// line went on reading `run` until the next prompt.
fn is_interrupted_line(l: &str) -> bool {
    l.contains("Interrupted · What should Claude do instead?")
}

/// A choice in a dialog: `❯ 1. Yes`. A digit, then a dot.
fn is_choice(after_glyph: &str) -> bool {
    let mut c = after_glyph.chars();
    matches!((c.next(), c.next()), (Some(d), Some('.')) if d.is_ascii_digit())
}

/// A prompt as the conversation shows it once sent: `❯ fix the tests`.
///
/// It opens a turn, so a turn line ABOVE it belongs to an earlier one. Not a
/// dialog's cursor, and not a slash command, which starts no turn: `❯ /exit` sits
/// between a finished turn and the box it was typed into without changing what
/// that turn line means.
fn is_sent_prompt(l: &str) -> bool {
    let Some(rest) = l.trim_start().strip_prefix('❯') else {
        return false;
    };
    let rest = rest.trim_start_matches([' ', '\u{a0}']);
    !rest.is_empty() && !rest.starts_with('/') && !is_choice(rest)
}

/// What the turn on screen says about itself, if anything.
///
/// Claude draws ONE turn line per turn and rewrites it in place, from the running
/// shape to the finished one, so the lowest on screen belongs to the most recent
/// turn and no earlier turn can contradict it. That ordering is the whole reason
/// this returns a single answer rather than two independent booleans: a finished
/// turn still shows its line while the next turn is being typed, and the next
/// turn's line appears BELOW it.
///
/// Except that the fullscreen renderer draws **no** turn line while a reply is
/// streaming, only the reply (measured on 2.1.280, capturing every 0.4 s: the
/// counter went after 1.6 s and did not come back until the finished line did).
/// Left alone, the lowest turn line was then the PREVIOUS turn's finished one,
/// and a session a few lines into its answer read as done. Two things fix that:
///
/// - the status row under the box reads `esc to interrupt` for as long as a turn
///   runs, where it fits (it gives way to the mode hint on a narrow pane);
/// - a sent prompt is a boundary: a turn line above the newest prompt on screen
///   belongs to an earlier turn, and the one after it has none to show yet.
///
/// Read over the sixteen lines of content above the box rather than the whole
/// capture, because both shapes turn up in ordinary output: a session discussing
/// this very code prints them. Sixteen because the turn line is nowhere near the
/// bottom of the screen: Claude tucks a tip row and a token count under it, and
/// measured across 39 live panes it sat 2 to 9 lines up.
pub fn turn_marker(screen: &str) -> Option<Turn> {
    let lines: Vec<&str> = screen.lines().collect();
    let live = live_box(&lines);
    if let Some(b) = live {
        if lines[b + 1..]
            .iter()
            .any(|l| l.contains("esc to interrupt"))
        {
            return Some(Turn::Running);
        }
    }
    lines[..live.unwrap_or(lines.len())]
        .iter()
        .filter(|l| !l.trim().is_empty())
        .rev()
        .take(16)
        .find_map(|l| {
            if is_running_line(l) {
                Some(Some(Turn::Running))
            } else if is_done_line(l) {
                Some(Some(if l.contains(" still running") {
                    Turn::Background
                } else {
                    Turn::Done
                }))
            } else if is_interrupted_line(l) {
                Some(Some(Turn::Done))
            } else if is_sent_prompt(l) {
                Some(None)
            } else {
                None
            }
        })
        .flatten()
}

/// Mid-turn, or waiting on work the turn left running.
pub fn is_working(screen: &str) -> bool {
    matches!(turn_marker(screen), Some(Turn::Running | Turn::Background))
}

/// The screen's own answer, in the order the bash version asks: a dialog wins
/// over an activity line, since it is the row that wants you. Idle only for a
/// prompt box that is really there (`live_box`); a screen that shows none, a pane
/// too short to draw one or a viewport scrolled away from it, is `Unknown`, and
/// says nothing either way.
pub fn classify(screen: &str) -> State {
    if awaits_input(screen) {
        State::Input
    } else if is_working(screen) {
        State::Run
    } else if live_box(&screen.lines().collect::<Vec<_>>()).is_some() {
        State::Idle
    } else {
        State::Unknown
    }
}

/// The hook line, brought up to date by the transcript where the transcript is
/// the newer of the two.
///
/// `hook_at` is when the line was written and `turn` the newest record that
/// opened or closed a turn (`turn::last_event`), both in epoch milliseconds. A
/// line newer than that record already knows about it; an older one does not:
///
/// - an **interrupt** after the line ends the turn, whatever the line said. No
///   hook fires for one, so this is the only way the line learns of it;
/// - the **end** of a turn after a line reading `run`, `input` or `ask` means the
///   line missed its `Stop`. After `bg` it is the record that same `Stop` was
///   written alongside, a few milliseconds later, and says nothing new;
/// - a typed **prompt** after a line reading `idle` or `bg` is a turn whose
///   `UserPromptSubmit` never reached the line.
pub fn correct(hook: &str, hook_at: i64, turn: Option<(crate::turn::Event, i64)>) -> &str {
    use crate::turn::Event;
    match turn {
        Some((ev, at)) if at > hook_at => match ev {
            Event::Interrupt => "idle",
            Event::Over if matches!(hook, "run" | "input" | "ask") => "idle",
            Event::Prompt if matches!(hook, "idle" | "bg") => "run",
            _ => hook,
        },
        _ => hook,
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
/// read as working forever and refuse every restart. The same holds for `ask`.
///
/// `ask` is the one line that says **waiting** without the screen: it is written
/// on Claude's own notification that a prompt has sat unanswered on screen, so
/// it is shown as waiting even where the screen cannot show the dialog, on a
/// pane too short to draw one or scrolled away from it. It gives way to a screen
/// that positively disagrees: the idle box above, or a turn line counting, which
/// is a permission granted to a tool that is still running.
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
/// only positively: a FINISHED turn line, or an interrupted one, with an idle
/// prompt box under it and no later turn line or sent prompt below it, is the
/// screen saying the turn is over as plainly as the box says no dialog is up. The
/// absence of a turn line is NOT that answer and is left to the hook, which is
/// the case this must not break: a session streaming a reply shows no turn line
/// at all while it does so, and then `run` is the only thing that knows.
///
/// `bg` reads as working: the turn is over, but work it started is still in
/// flight and the session will wake itself when it reports back.
pub fn merge(screen: &str, hook: Option<&str>) -> State {
    let screen_state = classify(screen);
    if screen_state == State::Input {
        return State::Input;
    }
    match hook {
        Some("input" | "ask") if screen_state == State::Idle => State::Idle,
        Some("ask") if screen_state == State::Run => State::Run,
        Some("ask") => State::Input,
        Some("idle") if screen_state == State::Run => State::Run,
        Some("run") if screen_state == State::Idle && turn_marker(screen) == Some(Turn::Done) => {
            State::Idle
        }
        // a session mid-permission is working, as far as the list is concerned
        Some("run" | "input" | "bg") => State::Run,
        Some("idle") => State::Idle,
        // No line, or one nobody recognises: a screen that could not be read reads
        // as idle, exactly as it always did.
        _ if screen_state == State::Unknown => State::Idle,
        _ => screen_state,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RULE: &str = "────────────────────────────────────────────────────────────────";

    /// The bottom of a real screen at rest: the token count, the box between its
    /// rules, and the status row, under whatever the conversation ends with.
    fn at_prompt(above: &str) -> String {
        format!(
            "{above}\n                                  38487 tokens\n{RULE}\n❯ \n{RULE}\n  ⏸ manual mode on · ? for shortcuts · ← 11 agents\n"
        )
    }

    /// The same mid-turn, where the status row has room to say so.
    fn mid_turn(above: &str) -> String {
        at_prompt(above).replace("? for shortcuts", "esc to interrupt")
    }

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
        let s = at_prompt(&("Do you want to proceed?\n".to_string() + &"filler\n".repeat(20)));
        assert!(!awaits_input(&s));
        assert_eq!(classify(&s), State::Idle);
    }

    #[test]
    fn the_live_box_is_the_glyph_under_a_rule() {
        let s = at_prompt("● done");
        let lines: Vec<&str> = s.lines().collect();
        assert_eq!(lines[live_box(&lines).unwrap()], "❯ ");
        // the rule above it can carry the session's name, down to four dashes
        let named = "──── gitlab-webhook-receiver: fix-ruff-ci-errors ─\n❯ \n";
        assert!(live_box(&named.lines().collect::<Vec<_>>()).is_some());
        // a prompt already sent has conversation above it, not a rule
        let sent = "✻ Baked for 4s · done 11:46 PM\n\n❯ Run exactly this\n";
        assert_eq!(live_box(&sent.lines().collect::<Vec<_>>()), None);
    }

    #[test]
    fn a_viewport_scrolled_up_shows_no_prompt_box() {
        // Probe, 2.1.280 fullscreen: a permission prompt open, then Page Up. The
        // lowest glyph on screen is a prompt sent earlier, and the dialog is
        // nowhere. This read as idle, and so did a real pane holding a prompt
        // for 35 hours.
        let paged = "● Got it — you prefer tea!\n\n✻ Baked for 4s · done 11:46 PM\n\n\
                     ❯ Run exactly this with the Bash too Jump to bottom (ctrl+End) ↓\n";
        assert_eq!(classify(paged), State::Unknown);
        let away = "❯ https://example.slack.com/archives/C07/p17\n     some reply text\n\
                    \x20                                          6 new messages (ctrl+End) ↓\n";
        assert_eq!(classify(away), State::Unknown);
        // So the line decides: confirmed on screen is waiting, the rest working.
        assert_eq!(merge(paged, Some("ask")), State::Input);
        assert_eq!(merge(paged, Some("input")), State::Run);
        assert_eq!(merge(away, Some("run")), State::Run);
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
            assert_eq!(turn_marker(&at_prompt(l)), Some(Turn::Done), "{l}");
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
    fn a_reply_streaming_under_the_last_turns_line_is_not_that_turn() {
        // Fullscreen draws no turn line while a reply streams, so the lowest one
        // on screen is the PREVIOUS turn's. Where the status row has room it says
        // so outright…
        let streaming = "✻ Baked for 12s · done 11:37 PM\n\n❯ Write a story about a baker\n\n\
                         \x20 Maria had learned to bake at her father's side";
        assert_eq!(turn_marker(&mid_turn(streaming)), Some(Turn::Running));
        assert_eq!(merge(&mid_turn(streaming), Some("run")), State::Run);
        // …and where it has not, the prompt sent since is the boundary: that
        // finished line belongs to a turn before it.
        assert_eq!(turn_marker(&at_prompt(streaming)), None);
        assert_eq!(merge(&at_prompt(streaming), Some("run")), State::Run);
    }

    #[test]
    fn a_slash_command_is_no_boundary() {
        // `/exit` typed and thought better of: it opened no turn, so the finished
        // line above it still says what the last turn did.
        let s = at_prompt(
            "✻ Cooked for 55m 46s · done 7:28 PM\n\n❯ /exit\n\n\
             ● Background shell command didn't finish before the previous session ended",
        );
        assert_eq!(turn_marker(&s), Some(Turn::Done));
    }

    #[test]
    fn an_interrupted_turn_is_over() {
        // Esc part way through a reply: no finished line, no hook, only this.
        let s = at_prompt(
            "  She paused, gathering words like scattered coins\n\
             \x20 ⎿  Interrupted · What should Claude do instead?",
        );
        assert_eq!(turn_marker(&s), Some(Turn::Done));
        assert_eq!(classify(&s), State::Idle);
        assert_eq!(merge(&s, Some("run")), State::Idle);
        // and an interrupt an earlier prompt left behind is not this turn's
        let older = at_prompt(
            "  ⎿  Interrupted · What should Claude do instead?\n\n❯ try again\n\n  Working on it",
        );
        assert_eq!(turn_marker(&older), None);
        assert_eq!(merge(&older, Some("run")), State::Run);
    }

    #[test]
    fn a_turn_that_left_work_running_is_busy() {
        let s = at_prompt("✻ Sautéed for 11s · done 11:39 PM · 1 shell still running");
        assert_eq!(turn_marker(&s), Some(Turn::Background));
        assert_eq!(classify(&s), State::Run);
        // whether or not the line knew about it
        for hook in [None, Some("idle"), Some("run"), Some("bg")] {
            assert_eq!(merge(&s, hook), State::Run, "{hook:?}");
        }
    }

    #[test]
    fn a_dialog_outranks_an_activity_line() {
        let s = "✽ Twisting… (35s)\n  2. No\n ❯ 1. Yes\n";
        assert_eq!(classify(s), State::Input);
    }

    #[test]
    fn a_prompt_box_is_idle_and_a_screen_without_one_is_unknown() {
        assert_eq!(classify(&at_prompt("● done")), State::Idle);
        // a glyph with no rule over it is not a box
        assert_eq!(classify(" ❯ "), State::Unknown);
        assert_eq!(classify("just some output\n"), State::Unknown);
        assert_eq!(classify(""), State::Unknown);
        // Two rows of a pane in a crowded window, which is all fullscreen draws
        // there: nothing of the box.
        assert_eq!(
            classify("\n                       new task? /clear to save 772.2k tokens\n"),
            State::Unknown
        );
    }

    // The screens the merge rules turn on. FINISHED and RUNNING carry a turn
    // line; STREAMING is the one that carries none, which is a real screen and not
    // a contrived one: a session part way through a long reply shows exactly this.
    const DIALOG: &str = "✽ Twisting… (35s)\n  2. No\n ❯ 1. Yes\n";
    fn finished() -> String {
        at_prompt("✻ Crunched for 9m 55s · done 11:07 AM")
    }
    fn running() -> String {
        at_prompt("✽ Twisting… (35s · ↓ 1.6k tokens)")
    }
    fn streaming() -> String {
        at_prompt("…and that is the third reason it cannot work.")
    }
    const UNREADABLE: &str = "just some output\n";

    #[test]
    fn a_dialog_on_screen_beats_any_hook_line() {
        for hook in [
            None,
            Some("run"),
            Some("idle"),
            Some("input"),
            Some("ask"),
            Some("bg"),
        ] {
            assert_eq!(merge(DIALOG, hook), State::Input);
        }
    }

    #[test]
    fn an_idle_screen_overrules_a_stale_hook_input() {
        // the 38-hour-old line that made a pane refuse every restart
        assert_eq!(merge(&finished(), Some("input")), State::Idle);
        assert_eq!(merge(&streaming(), Some("input")), State::Idle);
        // the confirmed kind too: a prompt refused with Esc fires nothing
        assert_eq!(merge(&finished(), Some("ask")), State::Idle);
    }

    #[test]
    fn a_confirmed_dialog_is_waiting_where_the_screen_cannot_show_it() {
        assert_eq!(merge(UNREADABLE, Some("ask")), State::Input);
        assert_eq!(merge("", Some("ask")), State::Input);
        // …but a counter running means the permission was granted and the tool
        // is at work, which fires nothing until it finishes
        assert_eq!(merge(&running(), Some("ask")), State::Run);
    }

    #[test]
    fn a_working_screen_overrules_a_stale_hook_idle() {
        // The line a session's last turn closed with, left behind because the
        // opening `run` of the turn now on screen never arrived. Two days old on
        // the pane this was found on, which was mid-turn at the time.
        assert_eq!(merge(&running(), Some("idle")), State::Run);
        // …and only that screen overrules it: every other reading still takes
        // the line at its word (the rest of them in the test below).
        assert_eq!(merge(&finished(), Some("idle")), State::Idle);
    }

    #[test]
    fn a_finished_turn_on_screen_overrules_a_stale_hook_run() {
        // The line a backgrounded session left behind: six and a half hours at
        // `run` with the pane sitting at an empty prompt box under a turn that
        // had plainly ended, and `restart` refusing it the whole time.
        assert_eq!(merge(&finished(), Some("run")), State::Idle);
    }

    #[test]
    fn a_screen_with_no_turn_line_still_leaves_run_alone() {
        // The case the rule above must not eat. A session part way through a long
        // reply shows no turn line at all, so the screen has nothing to say and
        // the hook is the only thing that knows a turn is in flight.
        assert_eq!(merge(&streaming(), Some("run")), State::Run);
        assert_eq!(merge(UNREADABLE, Some("run")), State::Run);
    }

    #[test]
    fn otherwise_the_hook_is_taken_as_it_stands() {
        assert_eq!(merge(UNREADABLE, Some("idle")), State::Idle);
        // a session mid-permission is working, for the list's purposes
        assert_eq!(merge(&running(), Some("input")), State::Run);
        assert_eq!(merge(UNREADABLE, Some("input")), State::Run);
        // and one waiting on its background work is too
        assert_eq!(merge(UNREADABLE, Some("bg")), State::Run);
        assert_eq!(merge(&finished(), Some("bg")), State::Run);
    }

    #[test]
    fn with_no_hook_the_screen_decides_and_unreadable_reads_as_idle() {
        assert_eq!(merge(&running(), None), State::Run);
        assert_eq!(merge(&finished(), None), State::Idle);
        assert_eq!(merge(&streaming(), None), State::Idle);
        assert_eq!(merge(UNREADABLE, None), State::Idle);
        assert_eq!(merge(UNREADABLE, Some("nonsense")), State::Idle);
    }

    #[test]
    fn a_transcript_newer_than_the_line_brings_it_up_to_date() {
        use crate::turn::Event::*;
        let line_at = 1_000;
        // An interrupt fires no hook: the transcript is the only one to know.
        for hook in ["run", "input", "ask", "bg", "idle"] {
            assert_eq!(
                correct(hook, line_at, Some((Interrupt, 1_500))),
                "idle",
                "{hook}"
            );
        }
        // A turn's end the line missed…
        for hook in ["run", "input", "ask"] {
            assert_eq!(
                correct(hook, line_at, Some((Over, 1_500))),
                "idle",
                "{hook}"
            );
        }
        // …but after `bg`, the record the same Stop was written alongside
        assert_eq!(correct("bg", line_at, Some((Over, 1_007))), "bg");
        // A prompt whose UserPromptSubmit never landed.
        assert_eq!(correct("idle", line_at, Some((Prompt, 1_500))), "run");
        assert_eq!(correct("bg", line_at, Some((Prompt, 1_500))), "run");
        assert_eq!(correct("ask", line_at, Some((Prompt, 1_500))), "ask");
    }

    #[test]
    fn a_line_newer_than_the_transcript_already_knows() {
        use crate::turn::Event::*;
        // The next prompt's UserPromptSubmit lands after the record it answers
        // (17 ms after, measured), so the interrupt before it is old news.
        assert_eq!(correct("run", 2_000, Some((Interrupt, 1_500))), "run");
        assert_eq!(correct("run", 2_000, Some((Over, 1_500))), "run");
        assert_eq!(correct("idle", 2_000, Some((Prompt, 1_983))), "idle");
        assert_eq!(correct("run", 2_000, None), "run");
    }
}
