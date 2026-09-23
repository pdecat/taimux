//! The picker, drawn here instead of by fzf.
//!
//! Step 1 answered the questions this depends on, inside a real `tmux
//! display-popup -E`, and the answers are worth keeping written down:
//!
//! - **The alternate screen nests inside a popup** and unwinds cleanly. That was
//!   the one genuine unknown, since fzf runs `--height=100%` here and so says
//!   nothing about it.
//! - **Bracketed paste arrives as `Event::Paste`**, one event carrying its own
//!   text, embedded line break included. This is the structural fix for the bug
//!   that put `~/.tmux.conf.local` into live agent sessions: fzf reads a pasted
//!   line break as Enter, and every guard against that is a heuristic. Here there
//!   is nothing left to defeat. A pasted line break arrives as CR, not LF.
//! - **Resize is an event**, not a reload.
//! - **The window is sized by the POPUP**, 126x34 inside an 80% popup of a 160x45
//!   terminal, so the rows are fitted to what they are actually drawn in rather
//!   than to `tput cols` less a guess at fzf's chrome.
//!
//! Drawing goes to `/dev/tty` and input comes from there too (crossterm's
//! use-dev-tty), which leaves stdout carrying exactly one line, the chosen pane
//! id. atuin swaps file descriptors in its shell widget to get the same effect.
//!
//! Owning the state is most of what this buys. fzf has no state store, so the
//! bash picker keeps its mode in the BORDER LABEL and reads it back out by
//! matching words in it, carries the mode and the search flag through every
//! reload as quoted arguments because a child spawned by a reload cannot be
//! relied on to see the new label yet, and needs `--track --id-nth=2` so a reload
//! does not drop the cursor. All of that is a field here.

use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::process::Command;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyCode, KeyEventKind, KeyModifiers, KeyboardEnhancementFlags, MouseButton, MouseEvent,
    MouseEventKind, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::{execute, terminal};
use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Terminal;

use crate::{ansi, rows};
use taimux_core::{env, index};

/// How close two clicks on one row have to be to read as a double-click, which
/// is what accepts it. Long enough to be reachable without hurrying, short
/// enough that two deliberate single clicks on the same row do not switch panes
/// by accident. Claude Code's own stray-click guard sits in the same range.
const DOUBLE_CLICK: Duration = Duration::from_millis(400);

/// Which list is on screen. Tab steps round the cycle.
///
/// The first four are the same question asked of the same list, and the last two
/// are not states at all: Outdated asks a different question of it (what is this
/// session RUNNING, rather than what is it doing), and Dead changes what the list
/// IS. So they sit at the far end, in that order, rather than between two states
/// of a running session.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    All,
    Input,
    Run,
    Idle,
    Outdated,
    Dead,
}

impl Mode {
    /// The state filter this mode passes to the layout; empty means every row.
    ///
    /// Outdated is empty because being behind is not a state: a session waiting,
    /// working or idle can each be running code a self-update has replaced, and
    /// that filter rides on `Input::outdated` instead.
    fn filter(self) -> &'static str {
        match self {
            Mode::All => "",
            Mode::Input => "input",
            Mode::Run => "run",
            Mode::Idle => "idle",
            Mode::Outdated => "",
            Mode::Dead => "dead",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Mode::All => "agent sessions",
            Mode::Input => "waiting for an answer",
            Mode::Run => "working",
            Mode::Idle => "idle at the prompt",
            Mode::Outdated => "running outdated code",
            Mode::Dead => "past sessions",
        }
    }

    /// The name this mode is carried across a reopen by. Its own word rather
    /// than the state filter, since Outdated and All share that.
    pub fn key(self) -> &'static str {
        match self {
            Mode::All => "all",
            Mode::Input => "input",
            Mode::Run => "run",
            Mode::Idle => "idle",
            Mode::Outdated => "outdated",
            Mode::Dead => "dead",
        }
    }

    pub fn from_key(k: &str) -> Mode {
        match k {
            "input" => Mode::Input,
            "run" => Mode::Run,
            "idle" => Mode::Idle,
            "outdated" => Mode::Outdated,
            "dead" => Mode::Dead,
            _ => Mode::All,
        }
    }

    /// The ring Tab walks, and which stops are worth landing on.
    ///
    /// A stop with nothing that could ever be in it is left OUT rather than
    /// reached and found empty: no cache of past sessions, no past stop, and
    /// nothing installed to compare a version against, no outdated stop. An
    /// empty list you can still land on is one you have to press Tab past every
    /// time round.
    fn cycle(ended: bool, outdated: bool) -> [(Mode, bool); 6] {
        [
            (Mode::All, true),
            (Mode::Input, true),
            (Mode::Run, true),
            (Mode::Idle, true),
            (Mode::Outdated, outdated),
            (Mode::Dead, ended),
        ]
    }

    /// One step round the ring, forwards or back, skipping the stops that are
    /// not on.
    fn step(self, back: bool, ended: bool, outdated: bool) -> Mode {
        let cycle = Mode::cycle(ended, outdated);
        let n = cycle.len();
        let at = cycle.iter().position(|(m, _)| *m == self).unwrap_or(0);
        // `at + n - k` rather than `at - k`, so going backwards past the first
        // stop stays in usize and wraps on the modulo like every other step.
        (1..=n)
            .map(|k| if back { at + n - k } else { at + k })
            .map(|i| cycle[i % n])
            .find(|(_, on)| *on)
            .map(|(m, _)| m)
            .unwrap_or(Mode::All)
    }

    /// One step round: all, waiting, working, idle, outdated, ended, all.
    fn next(self, ended: bool, outdated: bool) -> Mode {
        self.step(false, ended, outdated)
    }

    /// The same ring the other way, which is what shift-tab walks.
    ///
    /// Worth binding because the ring is short and the stop you want is as
    /// often the one behind you as the one ahead: overshooting "waiting for an
    /// answer" by one used to cost four more presses to come back to.
    fn prev(self, ended: bool, outdated: bool) -> Mode {
        self.step(true, ended, outdated)
    }
}

/// Where rows come from, and what does not change while the picker is open.
///
/// `fetch` is a closure rather than a string so ctrl-r and the refresh timer can
/// ask again. `ended` is separate because the ended list is not the pane list
/// filtered: it comes off the sessions cache and nothing in it has a pane at all.
pub struct Source {
    /// Shared and thread-safe because a refresh runs OFF the input loop: see
    /// `start_refresh`. It was a plain closure until a sweep froze the picker
    /// for the 85 seconds its restarts took, with no key accepted and nothing
    /// on screen to say why.
    pub fetch: Arc<dyn Fn() -> String + Send + Sync>,
    /// The ended list stays synchronous: it is a read of one cache file, with no
    /// fork in it, and it is what Tab's last stop shows the instant you land on
    /// it. Nothing here has ever been slow, and making it async would mean
    /// showing pane rows under the "past sessions" label while it arrived.
    pub ended: Option<Box<dyn Fn() -> String>>,
    pub cur: String,
    /// Where the pane the picker was opened from IS, for the case where that
    /// pane is not an agent session: `cur` then matches no row at all and these
    /// are what the cursor is placed by instead. See `nearest`.
    ///
    /// Empty means "not known", which is what every entry point but `pick` hands
    /// over, and the cursor then opens at the top of the list as it always did.
    pub cur_cwd: String,
    pub cur_target: String,
    pub home: String,
    pub newver: String,
    /// The taimux script, for the two keys that act rather than navigate. Unset
    /// means they are not bound, and the header then does not advertise them:
    /// the header only ever says what is really there.
    pub script: Option<String>,
    /// Set only when the binding said so with `-e TAIMUX_POPUP=1`. It is what
    /// allows the picker to close and reopen itself at a new size, which would
    /// be wrong for a picker running inline in a pane: tmux resizes a PANE with
    /// the client already, so there is nothing to do there and everything to
    /// lose by guessing.
    pub popup: bool,
    /// What a previous instance was doing when the terminal grew under it.
    pub state: State,
}

/// Which row to open on when the pane the picker was opened from is not an agent
/// session, and so is not in the list at all.
///
/// Pressed from a shell, `cur` matches nothing, and the cursor used to land on
/// the top of the list: a row chosen by whichever session tmux happens to list
/// first, which is to say by nothing. The question it should answer is "which of
/// these sessions is the one I am working on", and the best evidence for that is
/// the DIRECTORY. A shell in `~/projects/web` and an agent in `~/projects/web`
/// are the same piece of work; one in `~/projects/web/docs` very nearly is; one
/// in `~/notes` is not.
///
/// So the working directory is the primary key and the tmux list only breaks its
/// ties, which is the case where two sessions are equally close to the directory
/// and the nearer pane is the likelier one. Ties in BOTH keep the list's own
/// order, since `min_by_key` takes the first of equal minimums.
fn nearest(rows: &[&rows::Row], cwd: &str, target: &str) -> Option<usize> {
    rows.iter()
        .enumerate()
        .min_by_key(|(_, r)| {
            let (shared, apart) = cwd_near(cwd, &r.cwd);
            (Reverse(shared), apart, tmux_near(target, r))
        })
        .map(|(i, _)| i)
}

/// Path components, ignoring the empties a leading, doubled or trailing slash
/// leaves, so `/a/b`, `/a/b/` and `//a/b` are one directory rather than three.
fn comps(p: &str) -> Vec<&str> {
    p.split('/').filter(|c| !c.is_empty()).collect()
}

/// How near two directories are: how much of the path they share from the root,
/// then how many steps apart they are through the deepest directory they have in
/// common. The same directory is `(n, 0)`, a subdirectory of it `(n, 1)`, a
/// sibling `(n-1, 2)`.
///
/// Both halves are load-bearing, and in that order. Shared components first, so
/// a session one level DOWN from the directory you are in beats one a level up:
/// the deeper of the two is the more specific answer, and the parent is often
/// just where several unrelated projects happen to live. Then the distance, so
/// the directory itself beats a subdirectory of it.
///
/// Nothing in common answers `(0, 0)` rather than `(0, distance)`: the paths
/// diverge at their first component, so the directory says nothing about which
/// row is nearer, and ranking on the distance alone would put whichever session
/// sits closest to the root in front for a reason nobody could read off the
/// screen. The tie then falls through to the tmux list, which is the honest
/// answer. Same for an unknown directory on either side, which is empty and so
/// shares nothing with anything.
fn cwd_near(cur: &str, row: &str) -> (usize, usize) {
    let (a, b) = (comps(cur), comps(row));
    let shared = a.iter().zip(b.iter()).take_while(|(x, y)| x == y).count();
    if shared == 0 {
        return (0, 0);
    }
    (shared, (a.len() - shared) + (b.len() - shared))
}

/// How near a row's pane is to the pane the picker was opened from, in the list
/// tmux itself keeps: the same session first, nearest window and then nearest
/// pane inside it, then anything else on this server, then another host, whose
/// panes are not in this server's list at all and whose window numbers mean
/// nothing here.
fn tmux_near(cur: &str, r: &rows::Row) -> (u8, usize, usize) {
    /// Somewhere on this server, but not near anything: a different session, or
    /// a row that names no pane (an ended session's label is its age).
    const ELSEWHERE: (u8, usize, usize) = (1, 0, 0);
    if !r.host.is_empty() {
        return (2, 0, 0);
    }
    let (Some((sess, win, pane)), Some((rsess, rwin, rpane))) =
        (target_parts(cur), target_parts(&r.target))
    else {
        return ELSEWHERE;
    };
    if sess != rsess {
        return ELSEWHERE;
    }
    (0, win.abs_diff(rwin), pane.abs_diff(rpane))
}

/// `session:window.pane`, split into the three things it names, or `None` for
/// anything that is not one.
fn target_parts(t: &str) -> Option<(&str, usize, usize)> {
    let (sess, rest) = t.split_once(':')?;
    let (win, pane) = rest.split_once('.')?;
    Some((sess, win.parse().ok()?, pane.parse().ok()?))
}

/// Throw away what ratatui thinks is on the terminal, so the next draw repaints
/// in full.
///
/// `resize` and NOT `Terminal::clear`, which is the obvious call and is a trap
/// here: clear snapshots the cursor first, and the crossterm backend does that
/// with `crossterm::cursor::position()`, which writes ESC[6n to the PROCESS's
/// stdout rather than to the backend's writer. Stdout carries exactly one thing
/// in this program, the chosen pane id, so anything using clear puts `[6n` where
/// the caller reads the answer.
///
/// This was fixed once for ctrl-l and left in place for the two keys that hand
/// the terminal to a child, which need it MORE: they always repaint, so they
/// always leaked, and the list came back blank after every restart.
fn repaint<B: ratatui::backend::Backend>(term: &mut Terminal<B>) {
    if let Ok(size) = term.size() {
        let _ = term.resize(size.into());
    }
}

/// Run one of the two keys that act, with the terminal handed over.
///
/// Its output goes to **/dev/tty**, not to the picker's stdout. Inherited, the
/// child's whole screen ends up in the one thing this program writes to stdout,
/// the chosen pane id: measured, the caller of `taimux tui` got the sweep's
/// plan, its prompt and its closing message, and then the pane id on the end.
/// In a popup stdout happens to BE the tty, which is why it looked right there
/// and was wrong everywhere else.
/// Run a child that owns the terminal while it runs.
///
/// All THREE streams are pointed at the terminal, stdin included. The picker's
/// own stdin is not the terminal (its stdout carries the chosen pane id, and it
/// draws to /dev/tty for exactly that reason), so a child left to inherit it
/// gets a stdin that is not where the person is typing, while its output goes
/// somewhere else entirely. The child then reads its own /dev/tty to get around
/// that, which works but means the parent hands over a terminal it has only
/// half set up.
fn act_child(script: &str, args: &[&str]) -> std::io::Result<std::process::ExitStatus> {
    let mut c = Command::new(script);
    c.args(args);
    if let Ok(tty) = OpenOptions::new().write(true).open("/dev/tty") {
        if let Ok(err) = tty.try_clone() {
            c.stdout(tty).stderr(err);
        }
    }
    if let Ok(inp) = OpenOptions::new().read(true).open("/dev/tty") {
        c.stdin(inp);
    }
    c.status()
}

/// Raw mode, the alternate screen and bracketed paste, undone on the way out.
///
/// A guard rather than a pair of calls because every early return, `?` and panic
/// has to restore the terminal: the failure mode is a shell left in raw mode with
/// no echo, which is indistinguishable from a hung machine to whoever is looking
/// at it. This is the part bash could never do properly, since a trap does not
/// survive a kill.
struct Guard {
    out: File,
    kitty: bool,
    mouse: bool,
}

impl Guard {
    fn new(kitty: bool) -> std::io::Result<Guard> {
        let mut out = OpenOptions::new().write(true).open("/dev/tty")?;
        terminal::enable_raw_mode()?;
        // Mouse capture goes everywhere bracketed paste goes, including the
        // suspend/resume pair below, or handing the terminal to a child would
        // leave the picker with a dead wheel when it came back.
        //
        // fzf had this on by default and the port never asked for it, which is
        // the same way Page Up and Page Down went missing: nothing referenced
        // the behaviour, so nothing pointed at its absence. TAIMUX_MOUSE=0
        // turns it off, for a terminal where capture costs more than it gives
        // (it takes over drag-to-select, and tmux's own copy mode with it).
        execute!(out, terminal::EnterAlternateScreen, EnableBracketedPaste)?;
        let mouse = env::var("TAIMUX_MOUSE").is_none_or(|v| v != "0");
        if mouse {
            let _ = execute!(out, EnableMouseCapture);
        }
        if kitty {
            // Makes a bare ESC arrive on its own rather than as the head of a
            // possible chord. Only some terminals answer; the flags are harmless
            // where they are ignored, and they also turn on key-release events,
            // which is why the loop filters on KeyEventKind::Press.
            let _ = execute!(
                out,
                PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
            );
        }
        Ok(Guard { out, kitty, mouse })
    }

    /// Hand the terminal back so a child can own it, as fzf's `execute()` does.
    fn suspend(&mut self) {
        if self.mouse {
            let _ = execute!(self.out, DisableMouseCapture);
        }
        let _ = execute!(
            self.out,
            DisableBracketedPaste,
            terminal::LeaveAlternateScreen
        );
        let _ = terminal::disable_raw_mode();
    }

    fn resume(&mut self) {
        let _ = terminal::enable_raw_mode();
        // Wipe what the child drew, BEFORE going back to the alternate screen.
        //
        // The child owned the NORMAL screen while it had the terminal, so its
        // last frame is still sitting there under the picker. Leaving the
        // alternate screen on the way out then reveals it, and what you get,
        // seconds after picking a row, is the sweep's "restart every outdated
        // session" screen back on your terminal as if it had run again.
        // Reported that way, and it is only ever a leftover.
        //
        // Nothing of the caller's is lost: this runs only after a child that
        // cleared the screen for itself.
        let _ = execute!(
            self.out,
            terminal::Clear(terminal::ClearType::All),
            crossterm::cursor::MoveTo(0, 0),
            terminal::EnterAlternateScreen,
            EnableBracketedPaste
        );
        if self.mouse {
            let _ = execute!(self.out, EnableMouseCapture);
        }
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        if self.kitty {
            let _ = execute!(self.out, PopKeyboardEnhancementFlags);
        }
        self.suspend();
    }
}

/// The columns a row is laid out for: the drawn area less the border and less the
/// two the pointer takes. fzf reserves the same two and reports the rest in
/// FZF_COLUMNS, a figure that only exists once fzf is already up, which is why
/// the bash picker has to guess a width for its first render.
fn row_width(area_width: u16) -> usize {
    (area_width as usize).saturating_sub(4)
}

/// Under this many characters a term is in every transcript and a match would
/// say nothing, so a short query filters on the row alone.
fn search_min() -> usize {
    env::var("TAIMUX_SEARCH_MIN")
        .and_then(|v| v.parse().ok())
        .unwrap_or(3)
}

/// Text search is on unless it is turned off, the same knob the bash picker
/// reads. Note that it still starts OFF at the ctrl-t toggle; this only says
/// whether the key does anything.
fn search_enabled() -> bool {
    env::on("TAIMUX_SEARCH")
}

fn sessions_enabled() -> bool {
    env::on("TAIMUX_SESSIONS")
}

/// Following the terminal is on unless it is turned off. `0` leaves a popup at
/// whatever size it opened with, which is what every version before this did.
fn resize_enabled() -> bool {
    env::on("TAIMUX_RESIZE")
}

/// The ended-sessions list, or nothing where the sessions cache is turned off.
/// `Source.ended` being None is what takes that mode out of the Tab cycle, so
/// the decision is made once, here.
pub fn ended_source() -> Option<Box<dyn Fn() -> String>> {
    sessions_enabled().then(|| Box::new(|| index::dead_rows(now())) as Box<dyn Fn() -> String>)
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// The rows the query keeps: best match first, or `by_date` in the order the
/// list already has, which on the past list is newest first.
///
/// Terms are ANDed and their scores summed, which is fzf's extended-search
/// default rather than one fuzzy match over the whole query. With no query the
/// list keeps its own order; the sort is stable, so ties do too.
///
/// By date is NOT fzf's `--no-sort`, which leaves every match where it stands.
/// That was tried against the real history and is the version nobody wants: the
/// letters of `ha-bert`, in order but scattered, are in a dozen rows that have
/// nothing to do with it, and the ranking was all that kept those under the rows
/// that actually SAY it. With it gone, the top of the list was all noise. So the
/// rows holding every term as typed come first, newest first, and the loose
/// matches follow, newest first too. The rows are the same either way; only
/// their order changes.
///
/// The haystack is the row's PLAIN text: fzf is handed `--ansi` and has to parse
/// our own colours back out to match on them, which is work this does not do.
fn filter(list: &[rows::Row], query: &str, matcher: &SkimMatcherV2, by_date: bool) -> Vec<usize> {
    let terms: Vec<&str> = query.split_whitespace().collect();
    if terms.is_empty() {
        return (0..list.len()).collect();
    }
    // Folded, as the matcher ignores case too.
    let folded: Vec<String> = terms.iter().map(|t| t.to_lowercase()).collect();
    let mut kept: Vec<(i64, bool, usize)> = Vec::new();
    for (i, r) in list.iter().enumerate() {
        let hay = r.plain();
        let mut total = 0i64;
        let mut all = true;
        for t in &terms {
            match matcher.fuzzy_match(&hay, t) {
                Some(s) => total += s,
                None => {
                    all = false;
                    break;
                }
            }
        }
        if all {
            let loose = by_date && {
                let low = hay.to_lowercase();
                !folded.iter().all(|t| low.contains(t.as_str()))
            };
            kept.push((total, loose, i));
        }
    }
    if by_date {
        kept.sort_by_key(|(_, loose, _)| *loose);
    } else {
        kept.sort_by_key(|(score, _, _)| Reverse(*score));
    }
    kept.into_iter().map(|(_, _, i)| i).collect()
}

/// What the picker says it can do. Only what is really bound: a header promising
/// a key that does nothing is worse than a shorter one.
///
/// `by_date` is None on every list but the past one, which is the only list with
/// a date to sort by, and so the only one ctrl-s is bound on.
fn header(
    script: bool,
    ended: bool,
    search_key: bool,
    search_on: bool,
    by_date: Option<bool>,
) -> String {
    let mut h = String::from("enter: switch");
    if ended {
        h.push_str("/resume");
    }
    h.push_str("   tab: filter   ctrl-r: refresh   ctrl-/: preview");
    if search_key {
        h.push_str(if search_on {
            "   ctrl-t: search text (on)"
        } else {
            "   ctrl-t: search text"
        });
    }
    match by_date {
        Some(true) => h.push_str("   ctrl-s: sort by date (on)"),
        Some(false) => h.push_str("   ctrl-s: sort by date"),
        None => {}
    }
    if script {
        // "outdated" and not "stale", which it said until the list of those rows
        // got a Tab stop of its own: two words for one thing on the same screen
        // reads as two different things.
        h.push_str("   ctrl-x: restart   ctrl-o: hand off   f8: restart all outdated");
    }
    h
}

/// The bottom-right stamp: which taimux drew this list.
///
/// Worth a permanent corner of the chrome because the answer is not obvious from
/// anywhere else. The picker is a popup launched by a tmux binding, one binary
/// per host, and a self-update swaps the launcher under a running tmux server
/// without touching the panes: the same keypress can therefore draw a different
/// version tomorrow, and until now nothing on screen said which. It is the crate
/// version, the same string `taimux version` prints, so a row's `claude 2.1.229`
/// and this cannot be confused for each other: this one is named.
fn version_tag() -> String {
    format!(" taimux {} ", env!("CARGO_PKG_VERSION"))
}

/// …but only where the bottom border can carry it AND the count.
///
/// ratatui gives a right-aligned title precedence over a left-aligned one, so
/// without this the stamp eats the count on a narrow window: measured at 20
/// columns it left ` 5/`, and at 16 the count was gone altogether. That is the
/// priority backwards. The count is live and read constantly, the stamp is
/// reference read once after an update, so the stamp is what gives way.
///
/// `+ 2` is the two corner characters the border spends whatever else happens.
fn room_for_tag(width: u16, count: &str) -> bool {
    width as usize >= count.chars().count() + version_tag().chars().count() + 2
}

/// What to say where the rows would be, when there are none.
///
/// Four different silences, and they mean different things: nothing running at
/// all, nothing in the state you are filtering on, nothing matching what you
/// typed, and no ended sessions recorded yet. Saying which is the whole point,
/// since the picker used to say nothing and simply close.
fn empty_note(
    mode: Mode,
    query: &str,
    scanning: bool,
    nothing_scanned: bool,
    ended: bool,
) -> Vec<Line<'static>> {
    let mut lines: Vec<String> = Vec::new();
    if scanning {
        // The first scan is off the loop like every other, so this is what a
        // popup shows for the ~90ms it usually takes, and what it keeps showing
        // instead of going blank when something makes it slow.
        lines.push("Looking for agent sessions…".into());
        lines.push(String::new());
        lines.push("Esc closes this.".into());
        return lines
            .into_iter()
            .map(|l| Line::from(format!("  {}", l)))
            .collect();
    }
    if !query.is_empty() {
        lines.push(format!("Nothing matches {}", query));
        lines.push("ctrl-u clears it.".into());
    } else if mode == Mode::Dead {
        lines.push("No past conversations have been found here yet.".into());
        lines.push("They are remembered as sessions come and go.".into());
    } else if nothing_scanned {
        lines.push("No agent sessions on this machine.".into());
        lines.push(
            if ended {
                "Nothing is running one. Tab reaches the conversations that ended."
            } else {
                "Nothing is running one."
            }
            .into(),
        );
    } else {
        lines.push(format!("Nothing is {} right now.", mode.label()));
        lines.push("Tab moves on to the next list.".into());
    }
    lines.push(String::new());
    lines.push("Esc closes this.".into());
    lines
        .into_iter()
        .map(|l| Line::from(format!("  {}", l)))
        .collect()
}

/// The border label: which list and in what order, whether the timer and text
/// search are on, and whether a refresh is taking long enough to be worth
/// mentioning.
fn label(mode: Mode, by_date: bool, live: bool, search: bool, refreshing: bool) -> String {
    let mut s = format!(" {}", mode.label());
    // Straight after the name, since it says what order that list is in. Shown
    // with no query too, where the order is the same either way: it is a
    // setting, like ⌕, and a label that came and went as you typed would read
    // as the order changing under you.
    if by_date {
        s.push_str(" · by date");
    }
    if live {
        s.push_str(" · live");
    }
    if search {
        s.push_str(" · ⌕");
    }
    // Last, and only after a second: on a healthy machine the answer is back
    // before the next draw, so a label that flashed on every tick would be noise
    // about nothing. It is here for the case where the list is NOT arriving, so
    // that a picker waiting on a slow scan reads as busy rather than as dead.
    if refreshing {
        s.push_str(" · refreshing");
    }
    s.push(' ');
    s
}

/// A captured screen and when it was taken.
///
/// The preview is redrawn on every tick and every keypress, and capturing a pane
/// per redraw would be a fork per keystroke. Cached by pane, so it costs one
/// capture per row the cursor lands on, per TTL. Short on purpose: a preview is
/// read to see what a session is doing NOW, and a stale screen is worse than a
/// slow one.
const PREVIEW_TTL: Duration = Duration::from_millis(750);

/// …and longer for one that costs an ssh. The remote screen is no fresher for
/// being asked more often, since the fetch itself is the slow part.
const REMOTE_TTL: Duration = Duration::from_secs(3);

struct App {
    src: Source,
    matcher: SkimMatcherV2,
    mode: Mode,
    /// Typing searches what sessions SAID, not only what their rows show. Off by
    /// default, as in bash. The reason it HAD to be off is gone (a paste can no
    /// longer be read as Enter, see the module comment), but the port does not
    /// change behaviour; the rest of it lands in step 5.
    search: bool,
    /// Keep the past list newest first while a query filters it, rather than
    /// ranking the matches best first (loose matches still go last, see
    /// `filter`). Only that list reads it, being the only one with a date to go
    /// by, and it holds across Tab as `search` does.
    by_date: bool,
    preview: bool,
    query: String,
    width: usize,
    tsv: String,
    all: Vec<rows::Row>,
    view: Vec<usize>,
    sel: usize,
    shot: Option<(String, Instant, String)>,
    /// How far the preview is scrolled from its default view, in rows, negative
    /// towards the start of the body.
    poff: i32,
    /// The row `poff` was measured against. Comparing it in `preview()` resets
    /// the offset on every way the cursor can move (a key, the wheel, a click, a
    /// rebuild, the refresh timer) from ONE place, rather than needing each of
    /// those to remember to do it. An offset carried onto another row is a lie:
    /// it was measured against a different session's screen.
    poff_for: String,
    /// A refresh running on a worker thread, and when it started.
    ///
    /// The picker used to call the row source straight from the input loop, so
    /// for as long as that took there was no draw and no key: a refresh that
    /// normally costs 90ms froze the whole picker for 85 SECONDS after an F8
    /// sweep, showing the sweep's last screen the entire time, with Esc, ctrl-c
    /// and even tmux's own F1 all apparently dead. Nothing about that told its
    /// owner it was alive.
    ///
    /// One at a time: the timer must not stack refreshes on a machine where they
    /// take longer than the interval, which is exactly the machine this matters
    /// on.
    pending: Option<Receiver<Refresh>>,
    pending_since: Instant,
    /// The client as of the last refresh, for the resize check.
    client: Option<(String, (u16, u16))>,
    /// Panes with a restart in flight: when it was fired, the row the pane had
    /// at the time, and where that row sat in the list.
    ///
    /// A restart is detached and takes seconds: it asks the session to exit,
    /// waits, and starts a new one. For that whole window the pane has no agent
    /// in its foreground group, so the scan does not see it and the row simply
    /// VANISHES from under the cursor, which then falls back to the top of the
    /// list. You press ctrl-x on a session and lose both the row and your place.
    /// So the row is held: reinserted where it was, with the marker column saying
    /// what is happening, until the session comes back or the hold runs out.
    /// …and whether the pane has been observed GONE yet, which is what makes
    /// "it is in the scan again" mean the session came back rather than the
    /// restart not having happened yet.
    restarting: HashMap<String, (Instant, String, usize, bool)>,
}

/// How long a restarting row is held.
///
/// `restart` waits up to 12s for a session to exit and then polls up to 20s for
/// it to come back, so anything shorter than that drops the row exactly when its
/// owner is watching to see whether it worked. The hold is a backstop, not the
/// normal path: a row stops being held the moment the pane is scanned again.
const RESTART_HOLD: Duration = Duration::from_secs(40);

impl App {
    /// The preview for the row under the cursor, in two parts: a header saying
    /// exactly where the session is, and the body to show under it.
    ///
    /// Split rather than concatenated so the header can be PINNED while the body
    /// scrolls. The third value says where the body's default view sits: a live
    /// pane is anchored at the BOTTOM, because what a session is doing is the
    /// last thing on its screen, while an ended conversation reads from the top.
    /// One offset then means the same thing for both, "rows towards the start",
    /// and the clamp does the rest.
    ///
    /// The header comes off the row rather than out of a `tmux display-message`,
    /// which is a fork the bash preview pays every time the cursor moves.
    fn preview(&mut self) -> (Vec<Line<'static>>, Vec<Line<'static>>, bool) {
        let Some(r) = self.view.get(self.sel).map(|&i| &self.all[i]) else {
            return (Vec::new(), Vec::new(), false);
        };
        let (id, target, cwd, host) = (
            r.pane_id.clone(),
            r.target.clone(),
            r.cwd.clone(),
            r.host.clone(),
        );
        if self.poff_for != id {
            self.poff = 0;
            self.poff_for = id.clone();
        }
        // Where the words you typed turn up in what this session actually SAID.
        // The row has room for one window of context; this has room for several,
        // so the preview is where you find out whether the hit is the one you
        // were after before jumping to it.
        let mut out: Vec<Line<'static>> = Vec::new();
        let mut body: Vec<Line<'static>> = Vec::new();
        if self.search && self.query.chars().count() >= search_min() {
            let hits = index::preview_match(
                &id,
                &index::Query::new(&self.query),
                env::var("TAIMUX_SEARCH_PREVIEW")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(4),
            );
            for h in hits {
                out.push(Line::from(vec![
                    Span::styled("⌕ ", Style::default().fg(Color::Yellow)),
                    Span::raw(h),
                ]));
            }
            if !out.is_empty() {
                out.push(Line::from(""));
            }
        }
        out.push(Line::from(vec![
            Span::styled(
                target,
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("   "),
            Span::styled(cwd, Style::default().add_modifier(Modifier::DIM)),
        ]));
        out.push(Line::from(Span::styled(
            "─".repeat(44),
            Style::default().fg(Color::DarkGray),
        )));
        out.push(Line::from(""));

        // A past conversation has no screen to capture: what it has is the
        // last things that were said in it.
        if id.starts_with("dead:") {
            if id == "dead:!" {
                body.push(Line::from(Span::styled(
                    "the list is still being built",
                    Style::default().fg(Color::DarkGray),
                )));
                return (out, body, false);
            }
            let Some((agent, key)) = taimux_core::index::split_past_id(&id) else {
                body.push(Line::from(Span::styled(
                    "that row does not name a conversation",
                    Style::default().fg(Color::Red),
                )));
                return (out, body, false);
            };
            // A conversation kept in a database has no file to be missing, and
            // its store answered when the list was built.
            if key.starts_with('/') && !std::path::Path::new(key).is_file() {
                body.push(Line::from(Span::styled(
                    "this conversation is no longer on disk",
                    Style::default().fg(Color::Red),
                )));
                return (out, body, false);
            }
            let want = env::var("TAIMUX_DEAD_TURNS")
                .and_then(|v| v.parse().ok())
                .unwrap_or(6);
            let turns = taimux_core::agents::turns(agent, key, want);
            if turns.is_empty() {
                body.push(Line::from(Span::styled(
                    "(nothing was said in this one)",
                    Style::default().fg(Color::DarkGray),
                )));
            }
            for t in turns {
                // Two lines a turn is enough to recognise one, and the preview
                // pane is short.
                let cap = self.width.max(20) * 2;
                let what = if t.text.chars().count() > cap {
                    format!("{}…", t.text.chars().take(cap).collect::<String>())
                } else {
                    t.text
                };
                let (mark, st) = if t.you {
                    (
                        "❯ ",
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    )
                } else {
                    ("  ", Style::default().add_modifier(Modifier::DIM))
                };
                body.push(Line::from(vec![
                    Span::styled(mark, st),
                    Span::styled(what, st),
                ]));
                body.push(Line::from(""));
            }
            // From the top: a conversation reads forwards, and its opening is
            // already on the row as the title, so what you want first is what
            // came after it.
            return (out, body, false);
        }
        // capture-pane only works where the pane IS, so a session on another host
        // renders its own. That is an ssh, and the script already knows how to
        // make it: which taimux is over there, the bound it runs under, and what
        // to say when a host stops answering between the list and the cursor
        // landing on its row. Shelling out to it is one fork per cursor landing,
        // which is what fzf's preview cost anyway, and it beats keeping a second
        // copy of that knowledge here.
        if !host.is_empty() {
            let Some(script) = self.src.script.clone() else {
                body.push(Line::from(Span::styled(
                    format!("on {}: no taimux to ask", host),
                    Style::default().fg(Color::DarkGray),
                )));
                return (out, body, false);
            };
            let fresh = matches!(&self.shot, Some((k, at, _))
                                 if *k == id && at.elapsed() < REMOTE_TTL);
            if !fresh {
                let text = Command::new(&script)
                    .args(["preview", &id])
                    .output()
                    .ok()
                    .filter(|o| o.status.success())
                    .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
                    .unwrap_or_default();
                self.shot = Some((id.clone(), Instant::now(), text));
            }
            let text = self
                .shot
                .as_ref()
                .map(|(_, _, s)| s.clone())
                .unwrap_or_default();
            body.extend(tail(&text, usize::MAX));
            return (out, body, true);
        }
        let fresh = matches!(&self.shot, Some((k, at, _))
                             if *k == id && at.elapsed() < PREVIEW_TTL);
        if !fresh {
            self.shot = Some((
                id.clone(),
                Instant::now(),
                taimux_core::tmux::capture_coloured(&id).unwrap_or_default(),
            ));
        }
        let screen = self
            .shot
            .as_ref()
            .map(|(_, _, s)| s.clone())
            .unwrap_or_default();
        // The WHOLE screen, and the renderer takes the last screenful of it, so
        // there is something above the default view to scroll into. The padding
        // to the pane height is trimmed here either way, or the tail would be
        // all padding: the same trap that made every waiting session read as
        // idle when the state reader was ported.
        body.extend(tail(&screen, usize::MAX));
        // Anchored at the bottom: what a session is doing is the last thing on
        // its screen.
        (out, body, true)
    }
}

/// The last `room` lines of a captured screen, which is where what a session is
/// doing lives.
///
/// The trailing blanks come off first. `capture-pane` pads its output to the pane
/// height, so a tail taken without trimming is all padding: that is the exact
/// trap that made every session waiting for an answer read as idle when the state
/// reader was ported, and it is the same capture being read here.
fn tail(screen: &str, room: usize) -> Vec<Line<'static>> {
    let mut lines = ansi::to_lines(screen);
    while lines
        .last()
        .is_some_and(|l| l.spans.iter().all(|s| s.content.trim().is_empty()))
    {
        lines.pop();
    }
    let over = lines.len().saturating_sub(room);
    lines.drain(..over);
    lines
}

/// What a refresh brings back: the rows, how long they took, and what the time
/// went on.
struct Refresh {
    tsv: String,
    took: Duration,
    /// Empty unless something forked; see `stat`.
    spent: String,
    /// The client this popup is on, asked for only when we are in a popup that
    /// may reopen itself, and only when tmux can name it without guessing. It
    /// rides along with the refresh because that already runs off the input
    /// loop: a resize check of its own would be another fork on it.
    client: Option<(String, (u16, u16))>,
}

/// Our own session, out of `$TMUX`: socket, server pid, session id.
fn own_session() -> Option<String> {
    let tmux = std::env::var("TMUX").ok()?;
    let id = tmux.split(',').nth(2)?.trim();
    (!id.is_empty()).then(|| format!("${}", id))
}

/// The client this popup is drawn on, and its size, or None when that cannot be
/// answered without guessing.
///
/// **Asking tmux for `#{client_width}` with no target is the bug this exists to
/// avoid.** An untargeted query answers for whichever client tmux considers
/// current, and Patrick routinely has two attached to one session: a 213-column
/// desktop and a 46-column phone. A picker opened on the PHONE then measured
/// itself against the desktop, decided a 44-column popup had been outgrown,
/// closed itself, and reopened on the desktop over whatever pane was there. That
/// is what "stuck after selecting another session" turned out to be: a popup
/// arriving unbidden on the other client.
///
/// So the client has to be unambiguous. One client on our session is our client.
/// Two, and nothing here can tell which of them the popup belongs to (tmux
/// exposes no format for it, and `display-popup -e` does not expand formats, so
/// the binding cannot pass it either), which is exactly when this must do
/// nothing at all.
fn own_client() -> Option<(String, (u16, u16))> {
    let session = own_session()?;
    let out = taimux_core::tmux::ask(&[
        "list-clients",
        "-t",
        &session,
        "-F",
        "#{client_tty} #{client_width} #{client_height}",
    ])?;
    let mut lines = out.lines().filter(|l| !l.trim().is_empty());
    let only = lines.next()?;
    if lines.next().is_some() {
        return None; // more than one client: whose popup is this?
    }
    let mut f = only.split_whitespace();
    let (tty, w, h) = (f.next()?, f.next()?.parse().ok()?, f.next()?.parse().ok()?);
    (w > 0 && h > 0).then(|| (tty.to_string(), (w, h)))
}

/// Could this popup usefully be bigger than it is?
///
/// tmux SHRINKS a popup to fit a client that got smaller and grows it back up to
/// the size it was asked for, so the only case left over is a terminal that grew
/// PAST that: a popup opened on a phone in portrait stays portrait-width after
/// the rotation, at 63% of a screen it was told to take 80% of. Measured, both
/// directions, before any of this was written.
///
/// `slack` is what keeps it from firing on a rounding difference of a column or
/// two, which would close and reopen the popup for nothing.
fn outgrown(ours: (u16, u16), client: (u16, u16), slack: u16) -> bool {
    let (pw, ph) = crate::install::popup_geometry(client.0 as usize);
    // A popup's usable area is its geometry less the border it draws.
    let want_w = (client.0 as u32 * pw as u32 / 100).saturating_sub(2) as u16;
    let want_h = (client.1 as u32 * ph as u32 / 100).saturating_sub(2) as u16;
    want_w > ours.0.saturating_add(slack) || want_h > ours.1.saturating_add(slack)
}

/// Everything the picker has to carry across a reopen, so a resize costs you
/// your popup's geometry and nothing else.
///
/// Its Default is an ORDINARY open, not an empty struct: `preview` is on unless
/// something turned it off, and deriving Default silently opened every picker
/// with the preview hidden, which showed up as the list being twice as tall as
/// the page keys expected.
#[derive(Debug, PartialEq, Eq)]
pub struct State {
    pub query: String,
    pub mode: &'static str,
    pub search: bool,
    pub by_date: bool,
    pub preview: bool,
    /// The row the cursor was on, by pane id.
    pub on: String,
    /// The client whose popup this was, so the reopen goes to THAT one rather
    /// than to whichever tmux considers current a moment later.
    pub client: String,
}

impl Default for State {
    fn default() -> Self {
        State {
            query: String::new(),
            mode: "all",
            search: false,
            by_date: false,
            preview: true,
            on: String::new(),
            client: String::new(),
        }
    }
}

/// How the picker finished.
pub enum Outcome {
    Chosen(String),
    Aborted,
    /// The terminal grew: reopen at the geometry the binding would use now,
    /// with this state. Only ever returned from a popup that was told it is one.
    Resize(State),
}

/// How slow a refresh has to be before it is written down.
///
/// Two seconds is well past anything healthy here (a full scan of 171 panes is
/// 90ms) and well short of the freeze that prompted this, so the log stays empty
/// on a normal day and names the culprit on a bad one.
fn slow_after() -> Duration {
    Duration::from_secs_f32(
        env::var("TAIMUX_SLOW_REFRESH")
            .and_then(|v| v.parse().ok())
            .unwrap_or(2.0),
    )
}

/// A refresh that took too long, written where the sweep already sends you.
///
/// Appended rather than printed: the picker owns the screen, and the whole point
/// is that this happens while nobody can see anything.
fn log_slow(r: &Refresh) {
    let line = format!(
        "--- {} picker refresh took {:.1}s{}{}\n",
        taimux_core::log::stamp(),
        r.took.as_secs_f32(),
        if r.spent.is_empty() { "" } else { ": " },
        r.spent
    );
    let path = taimux_core::paths::runtime_dir().join("restart.log");
    if let Some(d) = path.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&path) {
        let _ = f.write_all(line.as_bytes());
    }
}

impl App {
    /// Rows now, on this thread. Startup and the ended list only: everything the
    /// loop does goes through `start_refresh` instead.
    fn fetch(&mut self) {
        self.tsv = match self.mode {
            Mode::Dead => self.src.ended.as_ref().map(|f| f()).unwrap_or_default(),
            _ => (self.src.fetch)(),
        };
        self.hold_restarting();
    }

    /// Ask for rows on a worker thread, leaving the loop free to draw and to
    /// read keys while the answer is on its way.
    ///
    /// The ended list is fetched inline, since it is a cache read with no fork in
    /// it and swapping it in late would mean drawing pane rows under the "ended
    /// sessions" label.
    fn start_refresh(&mut self) {
        if self.mode == Mode::Dead {
            self.fetch();
            self.rebuild();
            return;
        }
        if self.pending.is_some() {
            return;
        }
        let f = self.src.fetch.clone();
        let watch = self.src.popup;
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            taimux_core::stat::reset();
            let at = Instant::now();
            let tsv = f();
            let _ = tx.send(Refresh {
                tsv,
                took: at.elapsed(),
                spent: taimux_core::stat::report(),
                client: watch.then(own_client).flatten(),
            });
        });
        self.pending = Some(rx);
        self.pending_since = Instant::now();
    }

    /// Take a refresh that has landed. True when the list changed, which is what
    /// tells the loop to rebuild.
    ///
    /// A thread that died without sending (a panic in the row source) drops the
    /// sender, and that arrives here as Disconnected: the refresh is simply
    /// forgotten and the next tick tries again, rather than the picker waiting
    /// on it forever.
    fn take_refresh(&mut self) -> bool {
        let Some(rx) = &self.pending else {
            return false;
        };
        match rx.try_recv() {
            Ok(r) => {
                if r.took >= slow_after() {
                    log_slow(&r);
                }
                self.client = r.client;
                self.tsv = r.tsv;
                self.hold_restarting();
                self.pending = None;
                true
            }
            Err(TryRecvError::Empty) => false,
            Err(TryRecvError::Disconnected) => {
                self.pending = None;
                false
            }
        }
    }

    /// Has a refresh been out long enough to be worth saying so on the border?
    ///
    /// Not from the first millisecond: every tick would flicker the label on a
    /// healthy machine, where the answer is back before the next draw.
    fn refreshing(&self) -> bool {
        self.pending.is_some() && self.pending_since.elapsed() > Duration::from_secs(1)
    }

    /// Put back the rows of panes whose restart is still in flight.
    ///
    /// Reinserted at the index each one had rather than appended, because the
    /// list is otherwise unchanged and appending would move the row to the bottom
    /// just as its owner is watching it. Holding stops as soon as the pane is
    /// scanned again, which is the session coming back, or after RESTART_HOLD,
    /// which is the restart having failed. Either way the row stops lying.
    fn hold_restarting(&mut self) {
        if self.restarting.is_empty() {
            return;
        }
        let present: HashSet<String> = self
            .tsv
            .lines()
            .filter_map(|l| l.split('\t').next())
            .map(str::to_string)
            .collect();
        let now = Instant::now();
        self.restarting.retain(|id, (at, _, _, seen_gone)| {
            // The timeout is the backstop either way: a restart that never
            // took effect must not hold a row for ever.
            if now.duration_since(*at) >= RESTART_HOLD {
                return false;
            }
            if present.contains(id) {
                // Being in the scan only means "the session came back" if it
                // was ever seen to LEAVE. Before that it means the restart has
                // simply not taken effect yet, and treating the two the same is
                // what dropped the hold on the very first refresh after ctrl-x:
                // the agent had not exited yet, so the row was released, and
                // when it did exit a moment later there was nothing holding it.
                // The row vanished from under the cursor, which fell to the top.
                !*seen_gone
            } else {
                *seen_gone = true;
                true
            }
        });
        if self.restarting.is_empty() {
            return;
        }
        // Ascending, so each index still means the position it meant when the
        // row was taken out.
        // Only the ones actually MISSING are put back. An entry still held
        // because its pane has not gone yet is already in the list, and
        // reinserting it would show the row twice.
        let mut held: Vec<(usize, String)> = self
            .restarting
            .iter()
            .filter(|(id, _)| !present.contains(*id))
            .map(|(_, (_, line, idx, _))| (*idx, line.clone()))
            .collect();
        held.sort_by_key(|(idx, _)| *idx);
        let mut lines: Vec<String> = self.tsv.lines().map(str::to_string).collect();
        for (idx, line) in held {
            let at = idx.min(lines.len());
            lines.insert(at, line);
        }
        self.tsv = lines.join("\n");
        self.tsv.push('\n');
    }

    /// Start holding a pane's row, before the restart takes its session away.
    ///
    /// Called BEFORE the restart is fired, because afterwards the row it needs to
    /// remember may already be gone.
    fn hold(&mut self, id: &str) {
        if let Some((idx, line)) = self
            .tsv
            .lines()
            .enumerate()
            .find(|(_, l)| l.split('\t').next() == Some(id))
        {
            self.restarting.insert(
                id.to_string(),
                (Instant::now(), line.to_string(), idx, false),
            );
        }
    }

    /// The snippets the query earns, or none.
    ///
    /// **The "at least TAIMUX_SEARCH_MIN characters" gate lives here, not in the
    /// layout**, exactly as it does in bash: under three characters a term is in
    /// every transcript and a match would say nothing. Handing the layout a
    /// snippet map for a one-letter query turns every row into a search hit.
    fn snippets(&self) -> HashMap<String, String> {
        if !self.search || self.query.chars().count() < search_min() {
            return HashMap::new();
        }
        index::snippets(&index::Query::new(&self.query))
    }

    /// Whether the list on screen is in date order, newest first, rather than
    /// ranked best match first: the past list with ctrl-s on, and nothing else.
    fn dated(&self) -> bool {
        self.by_date && self.mode == Mode::Dead
    }

    /// Re-lay the rows out and re-apply the query, putting the cursor back on the
    /// same SESSION rather than the same index. That is what `--track --id-nth=2`
    /// buys fzf, and owning the state makes it a lookup.
    fn rebuild(&mut self) {
        let on = self.selected().map(|r| r.pane_id.clone());
        // The ended list is a different list, not this one filtered, so its own
        // rows are already only ended ones and asking for the filter as well
        // would be asking twice.
        let only = if self.mode == Mode::Dead {
            ""
        } else {
            self.mode.filter()
        };
        self.all = rows::build(
            &self.tsv,
            &rows::Input {
                cur: &self.src.cur,
                width: self.width,
                home: &self.src.home,
                newver: &self.src.newver,
                only,
                // A row held through a restart keeps the version it had, so the
                // one you just pressed ctrl-x on stays in this list, marked ↻,
                // until it comes back on the installed one and drops out of it.
                outdated: self.mode == Mode::Outdated,
                query: &self.query,
                snips: self.snippets(),
                // A live pane can publish no title at all: claude sets one at a
                // turn boundary, so one restored by tmux-resurrect and not
                // prompted since has nothing there.
                ptitles: index::pane_titles(),
                restarting: self.restarting.keys().cloned().collect(),
            },
        );
        self.view = filter(&self.all, &self.query, &self.matcher, self.dated());
        self.sel = on
            .and_then(|id| self.view.iter().position(|&i| self.all[i].pane_id == id))
            .unwrap_or(0);
        self.clamp();
    }

    /// The query changed.
    ///
    /// With text search on this is a full rebuild, not just a re-filter: a row
    /// that is in the list because of what its session SAID carries the snippet
    /// where its path would be, which is what puts the typed words ON the row so
    /// the matcher can keep working in the ordinary way. Re-filtering alone
    /// leaves the old rows in place, nothing carries the words, and every row
    /// disappears the moment you type something only a transcript holds.
    ///
    /// With search off it is only a filter, which is what makes typing into a
    /// picker you merely opened to jump as cheap as it always was.
    fn query_changed(&mut self) {
        if self.search {
            self.rebuild();
        } else {
            self.view = filter(&self.all, &self.query, &self.matcher, self.dated());
            self.clamp();
        }
    }

    /// Move to the next list, or with `back` to the one before it.
    ///
    /// Re-filtered from the rows already in hand, so the new list is on screen
    /// at once; the scan behind it lands when it lands.
    fn step_mode(&mut self, back: bool) {
        // Nothing installed to compare against and the outdated stop is not in
        // the ring at all: every row there would be judged against an empty
        // version, so the list could only ever be empty.
        let (ended, outdated) = (self.src.ended.is_some(), !self.src.newver.is_empty());
        self.mode = if back {
            self.mode.prev(ended, outdated)
        } else {
            self.mode.next(ended, outdated)
        };
        self.rebuild();
        self.start_refresh();
    }

    fn clamp(&mut self) {
        if self.view.is_empty() {
            self.sel = 0;
        } else if self.sel >= self.view.len() {
            self.sel = self.view.len() - 1;
        }
    }

    /// Drop the last word of the query, which is fzf's `unix-word-rubout` and
    /// `backward-kill-word`. The trailing space goes with it, so a query ending
    /// in one loses a whole word rather than just the gap.
    fn kill_word(&mut self) {
        while self.query.ends_with(char::is_whitespace) {
            self.query.pop();
        }
        while !self.query.is_empty() && !self.query.ends_with(char::is_whitespace) {
            self.query.pop();
        }
        self.query_changed();
    }

    /// Put the cursor on a pane, if it is in the list. Silent when it is not,
    /// which is the case where there is nothing to put it on, and `false` so the
    /// one caller that HAS somewhere else to put it can.
    fn focus(&mut self, id: &str) -> bool {
        match self.view.iter().position(|&i| self.all[i].pane_id == id) {
            Some(i) => {
                self.sel = i;
                true
            }
            None => false,
        }
    }

    /// Put the cursor on the row nearest the pane the picker was opened from,
    /// for when that pane is not an agent session and so is not a row itself.
    /// See `nearest` for what "nearest" is.
    ///
    /// Not knowing where that pane is means not knowing, so the cursor is left
    /// where the rebuild put it (the top of the list) rather than moved on a
    /// guess. That is what every entry point but `pick` gets.
    fn focus_nearest(&mut self) {
        if self.src.cur_cwd.is_empty() && self.src.cur_target.is_empty() {
            return;
        }
        let rows: Vec<&rows::Row> = self.view.iter().map(|&i| &self.all[i]).collect();
        if let Some(i) = nearest(&rows, &self.src.cur_cwd, &self.src.cur_target) {
            self.sel = i;
        }
    }

    fn selected(&self) -> Option<&rows::Row> {
        self.view.get(self.sel).map(|&i| &self.all[i])
    }

    fn move_by(&mut self, d: isize) {
        if self.view.is_empty() {
            return;
        }
        let n = self.view.len() as isize;
        self.sel = (((self.sel as isize + d) % n + n) % n) as usize; // --cycle
    }

    /// A screenful, and it CLAMPS where `move_by` cycles.
    ///
    /// fzf's page-up and page-down do not cycle even under `--cycle`, and that
    /// is the right behaviour rather than an inconsistency: a page that wrapped
    /// would be unusable for what paging is for. Holding Page Down to reach the
    /// bottom of a list would sail past the end and land back at the top, and
    /// nothing on the row tells you it happened.
    ///
    /// `page` is the list's drawn height, so it follows the popup's size and the
    /// preview being open. Zero is possible on a pane too short to draw a row,
    /// and would make the key do nothing.
    fn move_page(&mut self, pages: isize, page: usize) {
        if self.view.is_empty() {
            return;
        }
        let step = page.max(1) as isize;
        let last = self.view.len() as isize - 1;
        self.sel = (self.sel as isize + pages * step).clamp(0, last) as usize;
    }
}

/// Returns the row that was chosen, an abort, or a request to be reopened at a
/// new size.
pub fn run(src: Source) -> std::io::Result<Outcome> {
    let kitty = env::var("TAIMUX_TUI_KITTY").is_some_and(|v| v == "1");
    let mut guard = Guard::new(kitty)?;
    let backend = CrosstermBackend::new(guard.out.try_clone()?);
    let mut term = Terminal::new(backend)?;

    // 0 turns the timer off, as TAIMUX_REFRESH does for the fzf picker. There is
    // no idle gate here: fzf needs one because a reload blocks its input loop and
    // swallows keystrokes, and a tick in this loop is just a redraw.
    //
    // One second, down from three on 2026-09-20. Three was inherited from the
    // bash picker, where a refresh re-execed the script and blocked fzf's input
    // loop while it did; here it runs on a thread, the rows already on screen
    // stay up while it is out, and what the interval buys is how fast a session
    // that starts waiting for you turns `✳` while you are looking at the list.
    // Measured on 35 panes a scan is 85 ms, so a second apart it is under a tenth
    // of one core, and only while a picker is open.
    //
    // Not lower, and this is the floor rather than a preference: the screen
    // capture a daemon serves is cached for 750 ms, so two refreshes closer than
    // that would be answered from one capture, and the second would report a
    // state that had already been read rather than looking again.
    let refresh: f32 = env::var("TAIMUX_REFRESH")
        .and_then(|v| v.parse().ok())
        .unwrap_or(1.0);
    let live = refresh > 0.0;

    let mut app = App {
        // Not `smart_case`, which is fzf's default and was this picker's: see
        // `index::Query`. The two matchers have to agree about what a capital
        // means, and the answer both give now is "nothing".
        matcher: SkimMatcherV2::default().ignore_case(),
        mode: Mode::All,
        search: false,
        by_date: false,
        preview: true,
        query: String::new(),
        width: row_width(term.size()?.width),
        tsv: String::new(),
        all: Vec::new(),
        view: Vec::new(),
        sel: 0,
        shot: None,
        poff: 0,
        poff_for: String::new(),
        pending: None,
        pending_since: Instant::now(),
        client: None,
        restarting: HashMap::new(),
        src,
    };
    // Whatever a previous instance was doing when the terminal grew under it.
    // Default-empty otherwise, which is an ordinary open.
    app.query = std::mem::take(&mut app.src.state.query);
    app.mode = Mode::from_key(app.src.state.mode);
    app.search = app.src.state.search;
    app.by_date = app.src.state.by_date;
    app.preview = app.src.state.preview;
    // Even the FIRST scan runs off the loop. It used to be synchronous, on the
    // reasoning that there is nothing to draw until it lands, and what that
    // produced was a POPUP WITH NOTHING IN IT for as long as the scan took:
    // reported as another stuck picker, an empty box over a session, no keys.
    // There is something to draw, and it is "looking for them".
    //
    // An empty answer used to close the picker too. It says one thing, and it is
    // the thing you need: F1 on a machine with no agent sessions was
    // indistinguishable from F1 not being bound, from taimux not being
    // installed, and from the popup failing to start.
    app.start_refresh();
    // The cursor opens on the pane the picker was opened from, which is the row
    // marked ●, and on the row nearest to that pane when it is not an agent
    // session and so has no row of its own (see `nearest`). After a reopen it
    // goes back where it was instead, since that is the row you were looking at
    // when the terminal changed shape under you. Applied when the rows arrive,
    // since there is nothing to put it on before that.
    let opening_on = if app.src.state.on.is_empty() {
        app.src.cur.clone()
    } else {
        app.src.state.on.clone()
    };
    let mut opened = false;

    let mut state = ListState::default();
    let mut chosen: Option<String> = None;
    // Set when the terminal has grown past what this popup can use.
    let mut outgrew = false;
    let mut ticked = Instant::now();
    // How tall the list came out, written by the draw below and read by Page
    // Up / Page Down. Taken from the drawn area rather than recomputed from the
    // terminal size, because the layout it would have to reproduce (a border,
    // two fixed lines, and a preview that takes 60% only when there is room for
    // it) is exactly the sort of arithmetic that drifts from the real thing.
    let mut page: usize = 1;
    // Where the list starts on screen, for turning a click's row into a row of
    // the list. Same reasoning as `page`: measured, not recomputed.
    let mut list_y: u16 = 0;
    // The last left click, so a second one on the same row reads as a
    // double-click. crossterm reports presses, never double-clicks, so the only
    // way to have the gesture fzf had is to time it.
    let mut clicked: Option<(u16, Instant)> = None;

    loop {
        state.select(if app.view.is_empty() {
            None
        } else {
            Some(app.sel)
        });
        term.draw(|f| {
            let count = format!(" {}/{} ", app.view.len(), app.all.len());
            let mut block = Block::bordered()
                .title(label(
                    app.mode,
                    app.dated(),
                    live,
                    app.search,
                    app.refreshing(),
                ))
                .title_bottom(Line::from(count.clone()));
            // Dim, and in the corner furthest from the cursor: it is reference,
            // read once after an update and never again, so it must not compete
            // with the count beside it or the list above.
            if room_for_tag(f.area().width, &count) {
                block = block.title_bottom(
                    Line::from(Span::styled(
                        version_tag(),
                        Style::default().fg(Color::DarkGray),
                    ))
                    .right_aligned(),
                );
            }
            let inner = block.inner(f.area());
            f.render_widget(block, f.area());

            let [prompt, head, body] = Layout::vertical([
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Min(1),
            ])
            .areas(inner);

            f.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled("pick ❯ ", Style::default().fg(Color::Cyan)),
                    Span::raw(app.query.clone()),
                ])),
                prompt,
            );
            f.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    header(
                        app.src.script.is_some(),
                        app.src.ended.is_some(),
                        search_enabled(),
                        app.search,
                        (app.mode == Mode::Dead).then_some(app.by_date),
                    ),
                    Style::default().fg(Color::DarkGray),
                ))),
                head,
            );

            // The preview takes the bottom 60%, as --preview-window=down,60% does.
            let (body, prev) = if app.preview && body.height >= 8 {
                let [a, b] =
                    Layout::vertical([Constraint::Percentage(40), Constraint::Percentage(60)])
                        .areas(body);
                (a, Some(b))
            } else {
                (body, None)
            };
            page = body.height as usize;
            list_y = body.y;

            let items: Vec<ListItem> = app
                .view
                .iter()
                .map(|&i| {
                    ListItem::new(Line::from(
                        app.all[i]
                            .cells
                            .iter()
                            .map(|c| Span::styled(c.text.clone(), c.paint.style()))
                            .collect::<Vec<_>>(),
                    ))
                })
                .collect();
            if app.view.is_empty() {
                // Where the rows would be, in the same place your eye already
                // is, rather than a line tucked under the header.
                f.render_widget(
                    Paragraph::new(empty_note(
                        app.mode,
                        &app.query,
                        // Nothing has come back yet, which is not the same as
                        // nothing being there.
                        !opened,
                        // The RAW scan, not the laid-out rows: those already
                        // have the mode filter applied, so one idle session
                        // viewed through the waiting list read as a machine
                        // with nothing running on it at all.
                        app.tsv.trim().is_empty(),
                        app.src.ended.is_some(),
                    ))
                    .style(Style::default().fg(Color::DarkGray))
                    .wrap(Wrap { trim: false }),
                    body,
                );
            } else {
                f.render_stateful_widget(
                    List::new(items)
                        .highlight_symbol("▶ ")
                        .highlight_style(Style::default().add_modifier(Modifier::REVERSED)),
                    body,
                    &mut state,
                );
            }

            if let Some(area) = prev {
                let block = Block::default()
                    .borders(Borders::TOP)
                    .border_style(Style::default().fg(Color::DarkGray));
                let inner = block.inner(area);
                f.render_widget(block, area);
                let (head, text, at_bottom) = app.preview();
                // The header stays put and the body scrolls under it. Pinning it
                // is the whole reason the two are built separately: it says
                // WHICH session this is, and scrolling that off the top would
                // leave a screenful of text belonging to nothing in particular.
                let hh = (head.len() as u16).min(inner.height);
                let [hrect, brect] =
                    Layout::vertical([Constraint::Length(hh), Constraint::Min(0)]).areas(inner);
                f.render_widget(Paragraph::new(head).wrap(Wrap { trim: false }), hrect);

                // Where the window sits in the body. `poff` is rows away from
                // the default view, negative towards the start, and the clamp is
                // what lets one offset mean the same thing for a live pane
                // (anchored at the bottom) and an ended conversation (anchored
                // at the top): at either extreme it simply stops.
                //
                // The body is NOT wrapped, and that is what makes the arithmetic
                // exact. `Paragraph::scroll` counts WRAPPED rows while this
                // counts lines, so with wrapping on a capture padded to a wider
                // pane every line became two rows: each keypress moved half a
                // line and the clamp stopped a third of the way up. Unwrapped, a
                // line is a row. It also suits what this is, a viewport onto a
                // pane: a line too long for the preview reads better clipped
                // than re-flowed, since that is what the pane looks like. The
                // header keeps its wrap, being prose.
                let most = text.len().saturating_sub(brect.height as usize) as i32;
                let base = if at_bottom { most } else { 0 };
                let start = (base + app.poff).clamp(0, most.max(0));
                // Write the clamped offset BACK, or it accumulates past the end
                // of the body: hold shift-up at the top for a second and coming
                // back down takes as many presses as went in, with nothing on
                // screen moving for any of them. The limits are only known here,
                // where the body and the area both are, which is why the field
                // cannot clamp itself.
                app.poff = start - base;
                f.render_widget(Paragraph::new(text).scroll((start as u16, 0)), brect);
            }
        })?;

        // A refresh that has landed is taken here, between two draws, so the
        // rebuild it costs is the only work the loop ever does off the input
        // path. The ASK for one is free: it hands the row source to a thread.
        if app.take_refresh() {
            app.rebuild();
            if !opened {
                opened = true;
                // …and when that pane is not an agent session at all, which is
                // what F1 from a shell is, the row nearest to where it is.
                if !app.focus(&opening_on) {
                    app.focus_nearest();
                }
            }
            // The terminal has grown past what this popup was asked for, and
            // tmux will not grow a popup on its own. Leaving the loop is how the
            // picker asks to be reopened: the popup closes with it, and what it
            // was doing goes out in the Outcome.
            if let Some((_, size)) = app.client.clone() {
                if resize_enabled() && outgrown(term.size().map(|s| (s.width, s.height))?, size, 2)
                {
                    outgrew = true;
                    break;
                }
            }
        }
        if live && ticked.elapsed().as_secs_f32() >= refresh {
            ticked = Instant::now();
            app.start_refresh();
        }
        if !event::poll(Duration::from_millis(120))? {
            continue;
        }
        match event::read()? {
            // One event, carrying its own text, with no way to mistake it for
            // Enter. A pasted line break arrives as CR, so both are split on.
            Event::Paste(text) => {
                let first = text.split(['\r', '\n']).next().unwrap_or_default();
                app.query.push_str(first);
                app.query_changed();
            }
            // The wheel is the arrow keys, and a click is the cursor, which is
            // what fzf's default mouse handling did. Restored because the port
            // simply never asked the terminal for mouse events.
            Event::Mouse(MouseEvent { kind, row: my, .. }) => match kind {
                // Wrapping, because these ARE Up and Down: a wheel that stopped
                // where the arrow key it stands in for cycles would be the odd
                // one out. Over the preview too, since that pane has no scroll
                // of its own to offer instead.
                MouseEventKind::ScrollUp => app.move_by(-1),
                MouseEventKind::ScrollDown => app.move_by(1),
                // A click at or below where the list starts. Above it is the
                // prompt or the header, which are not rows.
                MouseEventKind::Down(MouseButton::Left) if my >= list_y => {
                    // Which row was under the pointer. `offset` is what the List
                    // widget has scrolled to, so this stays right on a list
                    // longer than the window, and a click past the last row
                    // lands on nothing rather than off the end.
                    let i = state.offset() + (my - list_y) as usize;
                    if i < app.view.len() {
                        app.sel = i;
                        // A second click on the row already under the cursor,
                        // soon enough, accepts it. fzf's double-click, with the
                        // clock this has to keep because crossterm reports
                        // presses and never the gesture.
                        let again =
                            clicked.is_some_and(|(r, t)| r == my && t.elapsed() < DOUBLE_CLICK);
                        clicked = Some((my, Instant::now()));
                        if again {
                            if let Some(r) = app.selected() {
                                chosen = Some(r.pane_id.clone());
                            }
                            break;
                        }
                    }
                }
                _ => {}
            },
            Event::Resize(w, _) => {
                app.width = row_width(w);
                app.rebuild();
            }
            Event::Key(k) => {
                // A terminal with the kitty flags pushed reports releases too, and
                // acting on both double-counts every key.
                if k.kind != KeyEventKind::Press {
                    continue;
                }
                let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
                let alt = k.modifiers.contains(KeyModifiers::ALT);
                let shift = k.modifiers.contains(KeyModifiers::SHIFT);
                match k.code {
                    // fzf aborts on all four of these, and abort is the one
                    // action worth having several ways to reach.
                    KeyCode::Esc => break,
                    KeyCode::Char('c') | KeyCode::Char('g') | KeyCode::Char('q') if ctrl => break,
                    KeyCode::Enter => {
                        if let Some(r) = app.selected() {
                            chosen = Some(r.pane_id.clone());
                        }
                        break;
                    }
                    // Scroll the PREVIEW, not the list, which is what fzf points
                    // these at. A live pane's preview opens on the bottom of its
                    // screen, so shift-up is how you see what came before it; an
                    // ended conversation opens at the top, so shift-down is how
                    // you read forwards through it. The offset is clamped at both
                    // ends of the body and reset whenever the cursor moves.
                    KeyCode::Up if shift => app.poff -= 1,
                    KeyCode::Down if shift => app.poff += 1,
                    KeyCode::Down => app.move_by(1),
                    KeyCode::Up => app.move_by(-1),
                    // Both pairs, as fzf binds both. ctrl-j is safe to take
                    // here: a terminal sends LF for it and CR for Enter, and
                    // crossterm keeps them apart, so this does not shadow
                    // accept. Checked rather than assumed.
                    KeyCode::Char('n') | KeyCode::Char('j') if ctrl => app.move_by(1),
                    KeyCode::Char('p') | KeyCode::Char('k') if ctrl => app.move_by(-1),
                    // The ends of the LIST. fzf points these at the ends of the
                    // QUERY by default, which in a picker you rarely type into is
                    // a key that visibly does nothing at all.
                    KeyCode::Home => app.sel = 0,
                    KeyCode::End => app.sel = app.view.len().saturating_sub(1),
                    // A screenful, by the height the list was actually drawn at,
                    // so it tracks the popup's size and whether the preview is
                    // open. fzf bound these itself and the port simply dropped
                    // them: Home and End were ported and these were not, which is
                    // why one pair kept working and the other went quiet.
                    KeyCode::PageDown => app.move_page(1, page),
                    KeyCode::PageUp => app.move_page(-1, page),
                    // Shift-tab walks the ring the other way. A terminal sends
                    // it as BackTab, and one with the kitty flags pushed sends
                    // Tab carrying SHIFT instead, so both are taken. BackTab is
                    // matched on its own rather than or-ed into the guarded arm
                    // below, because the modifier it arrives with is the part
                    // that differs between terminals and the key is not.
                    KeyCode::BackTab => app.step_mode(true),
                    KeyCode::Tab if shift => app.step_mode(true),
                    KeyCode::Tab => app.step_mode(false),
                    KeyCode::Char('r') if ctrl => app.start_refresh(),
                    // Nothing is bound when search is turned off, and the
                    // picker then behaves exactly as it did before there was any.
                    KeyCode::Char('t') if ctrl && search_enabled() => {
                        app.search = !app.search;
                        app.rebuild();
                    }
                    // The past list by date: newest first, the rows that say what
                    // was typed ahead of the loose matches (see `filter`). Bound
                    // on that list alone, the only one with a date to sort by,
                    // and a rebuild rather than a re-filter so the cursor stays
                    // on the conversation it was on: a second press then lands
                    // exactly where the first one started. It arrives as a key
                    // and not as XOFF, since raw mode turns flow control off.
                    KeyCode::Char('s') if ctrl && app.mode == Mode::Dead => {
                        app.by_date = !app.by_date;
                        app.rebuild();
                    }
                    // ctrl-/ reaches a terminal as several different bytes, so
                    // all of them are taken rather than one.
                    KeyCode::Char('/') | KeyCode::Char('_') | KeyCode::Char('\u{1f}') if ctrl => {
                        app.preview = !app.preview;
                    }
                    KeyCode::Char('u') if ctrl => {
                        app.query.clear();
                        app.query_changed();
                    }
                    // A word back, which fzf gives both of these. Worth having
                    // even though the query has no cursor: deleting the last
                    // word of "claude renovate" is a thing you want, and the
                    // alternative is holding backspace.
                    KeyCode::Char('w') if ctrl => app.kill_word(),
                    // Before the bare Backspace below, or alt-backspace would
                    // take a single character. It took one until now: the arm
                    // ignored modifiers, so fzf's backward-kill-word quietly
                    // behaved as plain backspace.
                    KeyCode::Backspace if alt => app.kill_word(),
                    // ctrl-h is backspace as far as fzf is concerned, and some
                    // terminals send it for the key. It needs naming separately
                    // because it arrives as a ctrl-char, not as Backspace.
                    KeyCode::Backspace => {
                        app.query.pop();
                        app.query_changed();
                    }
                    KeyCode::Char('h') if ctrl => {
                        app.query.pop();
                        app.query_changed();
                    }
                    // A full repaint, for a screen something else has written
                    // over. Every loop redraws already, so this only has to
                    // throw away what ratatui thinks is on the terminal.
                    //
                    // `resize` and NOT `Terminal::clear`, which would be the
                    // obvious call and is a trap here: it snapshots the cursor
                    // first, and the crossterm backend does that with
                    // `crossterm::cursor::position()`, which writes ESC[6n to
                    // the PROCESS's stdout rather than to the backend's writer.
                    // Stdout carries exactly one thing in this program, the
                    // chosen pane id, so ctrl-l put `[6n` where the caller reads
                    // the answer. Measured, not theorised.
                    //
                    // resize() on a fullscreen viewport takes the same path
                    // minus that snapshot: it clears through the backend's own
                    // writer and resets the back buffer, so the next draw
                    // repaints in full.
                    KeyCode::Char('l') if ctrl => repaint(&mut term),
                    // The two keys that act rather than navigate. They run the
                    // script the way fzf's execute() does: hand the terminal over,
                    // let the child own it, take it back.
                    KeyCode::Char('x') if ctrl => {
                        if let (Some(s), Some(r)) = (app.src.script.clone(), app.selected()) {
                            let id = r.pane_id.clone();
                            // Held BEFORE the restart is fired. By the time it
                            // returns the session may already be gone, and with
                            // it the row this needs to remember.
                            app.hold(&id);
                            guard.suspend();
                            if let Err(e) = act_child(&s, &["_restart", &id]) {
                                crate::act::report_failed_child("the restart", &e);
                            }
                            guard.resume();
                            repaint(&mut term);
                            // Rebuilt from the rows already in hand and drawn on
                            // the next pass, so the list is back on screen at
                            // once. Asking for fresh rows here and WAITING for
                            // them is what left the picker showing the child's
                            // last screen, unable to draw or read a key, for as
                            // long as the scan took.
                            app.rebuild();
                            app.start_refresh();
                            // …and the cursor goes back on it explicitly. The
                            // rebuild re-pins by pane id on its own, but only
                            // when the row is in the list: a restart that was
                            // REFUSED (working, holding a dialog, unresolvable)
                            // holds nothing, so without this the cursor would
                            // still fall to the top on exactly the presses that
                            // did nothing.
                            app.focus(&id);
                        }
                    }
                    // ctrl-o: carry this conversation into a different agent.
                    // Same shape as ctrl-x, and for the same reason: it draws a
                    // menu, waits on a key and then opens a window, none of
                    // which the picker's own loop can do while it is drawing.
                    KeyCode::Char('o') if ctrl => {
                        if let (Some(s), Some(r)) = (app.src.script.clone(), app.selected()) {
                            let id = r.pane_id.clone();
                            guard.suspend();
                            if let Err(e) = act_child(&s, &["_handoff", &id]) {
                                crate::act::report_failed_child("the handoff", &e);
                            }
                            guard.resume();
                            repaint(&mut term);
                            // Nothing in the list changed: a handoff opens a NEW
                            // window and leaves the conversation it came from
                            // exactly where it was. So the cursor goes straight
                            // back on the row rather than the list being rebuilt.
                            app.focus(&id);
                        }
                    }
                    KeyCode::F(8) => {
                        if let Some(s) = app.src.script.clone() {
                            guard.suspend();
                            if let Err(e) = act_child(&s, &["_sweep"]) {
                                crate::act::report_failed_child("the sweep", &e);
                            }
                            guard.resume();
                            repaint(&mut term);
                            // Same as ctrl-x, and this is the press it was
                            // REPORTED on: a sweep restarts every outdated
                            // session at once, so the scan that follows it is the
                            // slowest one the picker ever runs.
                            app.rebuild();
                            app.start_refresh();
                        }
                    }
                    // Anything else printable joins the query. `!alt` matters as
                    // much as `!ctrl` and was missing: ALT is not CTRL, so every
                    // alt-chord fell in here and TYPED ITS LETTER. Holding alt
                    // and pressing b put a "b" in the query, which is fzf's
                    // backward-word, and any stray chord the terminal passed
                    // through corrupted the search with no way to tell.
                    KeyCode::Char(c) if !ctrl && !alt => {
                        app.query.push(c);
                        app.query_changed();
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    drop(term);
    drop(guard);
    Ok(match (chosen, outgrew) {
        (Some(id), _) => Outcome::Chosen(id),
        (None, true) => Outcome::Resize(State {
            query: app.query.clone(),
            mode: app.mode.key(),
            search: app.search,
            by_date: app.by_date,
            preview: app.preview,
            on: app
                .selected()
                .map(|r| r.pane_id.clone())
                .unwrap_or_default(),
            client: app.client.clone().map(|(tty, _)| tty).unwrap_or_default(),
        }),
        (None, false) => Outcome::Aborted,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn src(tsv: &str) -> Source {
        let t = tsv.to_string();
        Source {
            fetch: Arc::new(move || t.clone()),
            ended: None,
            cur: String::new(),
            cur_cwd: String::new(),
            cur_target: String::new(),
            home: "/h".into(),
            newver: String::new(),
            script: None,
            popup: false,
            state: Default::default(),
        }
    }

    fn app(tsv: &str) -> App {
        let mut a = App {
            src: src(tsv),
            matcher: SkimMatcherV2::default().ignore_case(),
            mode: Mode::All,
            search: false,
            by_date: false,
            preview: true,
            query: String::new(),
            width: 100,
            tsv: String::new(),
            all: Vec::new(),
            view: Vec::new(),
            sel: 0,
            shot: None,
            poff: 0,
            poff_for: String::new(),
            pending: None,
            pending_since: Instant::now(),
            client: None,
            restarting: HashMap::new(),
        };
        a.fetch();
        a.rebuild();
        a
    }

    /// An app whose scan can be changed under it, which is what a restart does:
    /// the pane is there, then it is not, then it is back.
    ///
    /// Arc/Mutex rather than Rc/RefCell because the row source is handed to a
    /// worker thread now, so it has to be Send and Sync like the real ones.
    fn app_live(cell: Arc<std::sync::Mutex<String>>) -> App {
        let c = cell.clone();
        let mut a = App {
            src: Source {
                fetch: Arc::new(move || c.lock().unwrap().clone()),
                ended: None,
                cur: String::new(),
                cur_cwd: String::new(),
                cur_target: String::new(),
                home: "/h".into(),
                newver: String::new(),
                script: None,
                popup: false,
                state: Default::default(),
            },
            matcher: SkimMatcherV2::default().ignore_case(),
            mode: Mode::All,
            search: false,
            by_date: false,
            preview: true,
            query: String::new(),
            width: 100,
            tsv: String::new(),
            all: Vec::new(),
            view: Vec::new(),
            sel: 0,
            shot: None,
            poff: 0,
            poff_for: String::new(),
            pending: None,
            pending_since: Instant::now(),
            client: None,
            restarting: HashMap::new(),
        };
        a.fetch();
        a.rebuild();
        a
    }

    fn ids(a: &App) -> Vec<String> {
        a.view.iter().map(|&i| a.all[i].pane_id.clone()).collect()
    }

    /// The bug this is all for: a restart takes the session away for seconds, so
    /// the pane has no agent, the scan does not see it, and the row disappears
    /// from under the cursor.
    #[test]
    fn a_restarting_row_stays_in_the_list_where_it_was() {
        let cell = Arc::new(std::sync::Mutex::new(THREE.to_string()));
        let mut a = app_live(cell.clone());
        a.sel = 1; // the middle one, %2
        assert_eq!(ids(&a), ["%1", "%2", "%3"]);

        a.hold("%2");
        // the restart has taken it away
        *cell.lock().unwrap() = "%1\ta:1.1\t/h\tclaude\t1\tidle\t-\tapple pie\n\
                              %3\tc:1.1\t/h\tclaude\t1\tinput\t-\tcherry tart"
            .to_string();
        a.fetch();
        a.rebuild();

        assert_eq!(ids(&a), ["%1", "%2", "%3"], "the row should still be there");
        assert_eq!(
            a.selected().map(|r| r.pane_id.as_str()),
            Some("%2"),
            "and the cursor should still be on it"
        );
    }

    /// The same, but through the sequence ctrl-x actually produces.
    ///
    /// The test above jumps straight to "the restart has taken it away", and
    /// that is the step the bug was hiding behind. A restart is fired and
    /// returns AT ONCE, so the first refresh after ctrl-x still sees the agent:
    /// it has been asked to exit and has not done so yet. Releasing the hold on
    /// that refresh meant nothing was holding the row when the session did go a
    /// moment later, and the cursor fell to the top of the list while its owner
    /// was watching the session they had just asked to upgrade.
    #[test]
    fn a_row_is_still_held_through_the_refresh_before_the_session_goes() {
        let cell = Arc::new(std::sync::Mutex::new(THREE.to_string()));
        let mut a = app_live(cell.clone());
        a.sel = 1;
        a.hold("%2");

        // Refresh ONE: the restart is in flight and the agent is still there.
        a.fetch();
        a.rebuild();
        assert_eq!(
            ids(&a),
            ["%1", "%2", "%3"],
            "no duplicate while it is present"
        );
        assert!(
            a.restarting.contains_key("%2"),
            "not yet gone, so still held"
        );
        assert_eq!(
            a.selected().map(|r| r.pane_id.as_str()),
            Some("%2"),
            "cursor stays put"
        );

        // Refresh TWO: now the session has actually gone.
        *cell.lock().unwrap() = "%1\ta:1.1\t/h\tclaude\t1\tidle\t-\tapple pie\n\
                              %3\tc:1.1\t/h\tclaude\t1\tinput\t-\tcherry tart"
            .to_string();
        a.fetch();
        a.rebuild();
        assert_eq!(
            ids(&a),
            ["%1", "%2", "%3"],
            "held in place while it is away"
        );
        assert_eq!(
            a.selected().map(|r| r.pane_id.as_str()),
            Some("%2"),
            "and the cursor is STILL on the session being upgraded"
        );

        // Refresh THREE: it comes back, and only now is the hold spent.
        *cell.lock().unwrap() = THREE.to_string();
        a.fetch();
        a.rebuild();
        assert!(a.restarting.is_empty(), "back for real, so no longer held");
        assert_eq!(ids(&a), ["%1", "%2", "%3"]);
        assert_eq!(a.selected().map(|r| r.pane_id.as_str()), Some("%2"));
    }

    /// Appending would have been easier and wrong: the row would jump to the
    /// bottom of the list at the moment its owner is watching it.
    #[test]
    fn a_held_row_is_not_moved_to_the_end() {
        let cell = Arc::new(std::sync::Mutex::new(THREE.to_string()));
        let mut a = app_live(cell.clone());
        a.hold("%1");
        *cell.lock().unwrap() = "%2\tb:1.1\t/h\tclaude\t1\trun\t-\tbanana bread\n\
                              %3\tc:1.1\t/h\tclaude\t1\tinput\t-\tcherry tart"
            .to_string();
        a.fetch();
        a.rebuild();
        assert_eq!(ids(&a), ["%1", "%2", "%3"], "%1 was first and stays first");
    }

    /// Holding stops the moment the session is back, or the row would go on
    /// claiming a restart is in flight for as long as the picker is open.
    #[test]
    fn the_hold_is_released_when_the_session_comes_back() {
        let cell = Arc::new(std::sync::Mutex::new(THREE.to_string()));
        let mut a = app_live(cell.clone());
        a.hold("%2");
        *cell.lock().unwrap() = "%1\ta:1.1\t/h\tclaude\t1\tidle\t-\tapple pie".to_string();
        a.fetch();
        assert!(a.restarting.contains_key("%2"), "still away, still held");
        // back, with a new title, which is what a fresh session looks like
        *cell.lock().unwrap() = THREE.to_string();
        a.fetch();
        a.rebuild();
        assert!(a.restarting.is_empty(), "back, so no longer held");
        assert_eq!(ids(&a), ["%1", "%2", "%3"]);
    }

    /// A restart that never comes back must not leave a row lying about forever.
    #[test]
    fn the_hold_expires() {
        let cell = Arc::new(std::sync::Mutex::new(THREE.to_string()));
        let mut a = app_live(cell.clone());
        a.hold("%2");
        // fired longer ago than the hold allows
        if let Some(e) = a.restarting.get_mut("%2") {
            e.0 = Instant::now() - RESTART_HOLD - Duration::from_secs(1);
        }
        *cell.lock().unwrap() = "%1\ta:1.1\t/h\tclaude\t1\tidle\t-\tapple pie".to_string();
        a.fetch();
        a.rebuild();
        assert!(a.restarting.is_empty());
        assert_eq!(
            ids(&a),
            ["%1"],
            "the row is gone, because the restart failed"
        );
    }

    /// The marker column says a restart is in flight. It goes there and not into
    /// the summary because the summary strips a leading marker glyph.
    #[test]
    fn a_held_row_is_marked_as_restarting() {
        let cell = Arc::new(std::sync::Mutex::new(THREE.to_string()));
        let mut a = app_live(cell.clone());
        a.hold("%2");
        *cell.lock().unwrap() = "%1\ta:1.1\t/h\tclaude\t1\tidle\t-\tapple pie".to_string();
        a.fetch();
        a.rebuild();
        let row = a.all.iter().find(|r| r.pane_id == "%2").unwrap();
        let text = row.to_ansi();
        assert!(
            text.contains('↻'),
            "expected the restart marker in {text:?}"
        );
        // …and it does not borrow the waiting star, which means something else
        assert!(!text.contains('✳'), "must not read as asking: {text:?}");
    }

    /// A row held while a filter is on keeps its state, so it stays in whichever
    /// mode was being watched. A synthetic state would have dropped it out of the
    /// list at exactly the wrong moment.
    #[test]
    fn a_held_row_survives_the_mode_it_was_watched_in() {
        let cell = Arc::new(std::sync::Mutex::new(THREE.to_string()));
        let mut a = app_live(cell.clone());
        a.mode = Mode::Run; // %2 is the running one
        a.rebuild();
        assert_eq!(ids(&a), ["%2"]);
        a.hold("%2");
        *cell.lock().unwrap() = "%1\ta:1.1\t/h\tclaude\t1\tidle\t-\tapple pie".to_string();
        a.fetch();
        a.rebuild();
        assert_eq!(ids(&a), ["%2"], "still listed under the filter it was in");
    }

    /// The bug this is all for: the picker used to call the row source from its
    /// input loop, so a scan that took 85 seconds after an F8 sweep was 85
    /// seconds with no draw and no key. Asking must return AT ONCE.
    #[test]
    fn asking_for_a_refresh_does_not_wait_for_it() {
        let mut a = app(THREE);
        a.src.fetch = Arc::new(|| {
            std::thread::sleep(Duration::from_millis(400));
            "%9\tz:1.1\t/h\tclaude\t1\tidle\t-\tlate arrival".to_string()
        });
        let at = Instant::now();
        a.start_refresh();
        assert!(
            at.elapsed() < Duration::from_millis(100),
            "start_refresh blocked for {:?}",
            at.elapsed()
        );
        assert!(!a.take_refresh(), "nothing has landed yet");
        assert_eq!(a.all.len(), 3, "and the old rows are still there to draw");

        // …and it lands later, without anything having waited on it.
        let mut got = false;
        for _ in 0..100 {
            if a.take_refresh() {
                got = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(got, "the refresh never arrived");
        a.rebuild();
        assert_eq!(ids(&a), ["%9"]);
    }

    /// One at a time. The timer must not stack refreshes on a machine where they
    /// take longer than the interval, which is the machine this matters on.
    #[test]
    fn a_second_refresh_is_not_started_while_one_is_out() {
        let mut a = app(THREE);
        let runs = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let r = runs.clone();
        a.src.fetch = Arc::new(move || {
            r.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(300));
            String::new()
        });
        a.start_refresh();
        a.start_refresh();
        a.start_refresh();
        std::thread::sleep(Duration::from_millis(500));
        assert_eq!(runs.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    /// A row source that panics drops its sender rather than answering. The
    /// picker has to forget that refresh and carry on, not wait on it forever.
    #[test]
    fn a_refresh_that_never_answers_is_forgotten() {
        let mut a = app(THREE);
        a.src.fetch = Arc::new(|| panic!("the scan blew up"));
        a.start_refresh();
        for _ in 0..100 {
            if a.pending.is_none() {
                break;
            }
            let _ = a.take_refresh();
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(a.pending.is_none(), "still waiting on a dead thread");
        assert_eq!(a.all.len(), 3, "and the list it had is untouched");
    }

    /// The border says so, but only once it has been a second: on a healthy
    /// machine the answer is back before the next draw, and a label that flashed
    /// every tick would be noise about nothing.
    #[test]
    fn the_border_says_refreshing_only_when_it_is_worth_saying() {
        let mut a = app(THREE);
        a.src.fetch = Arc::new(|| {
            std::thread::sleep(Duration::from_millis(1500));
            String::new()
        });
        a.start_refresh();
        assert!(!a.refreshing(), "not from the first millisecond");
        a.pending_since = Instant::now() - Duration::from_secs(2);
        assert!(a.refreshing());
        assert_eq!(
            label(Mode::All, false, true, false, true),
            " agent sessions · live · refreshing "
        );
    }

    /// The picker used to CLOSE itself when the list came out empty, and that
    /// is what was reported as "F1 no longer works": on a machine with no agent
    /// sessions the popup opened and closed too fast to see, which looks exactly
    /// like an unbound key, a missing binary, or a popup that failed to start.
    /// The four silences mean different things and it now says which.
    #[test]
    fn an_empty_list_says_which_kind_of_empty_it_is() {
        let text = |ls: Vec<Line<'static>>| -> String {
            ls.iter()
                .map(|l| {
                    l.spans
                        .iter()
                        .map(|s| s.content.to_string())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("\n")
        };

        // nothing running at all, which is the reported case
        let none = text(empty_note(Mode::All, "", false, true, false));
        assert!(none.contains("No agent sessions on this machine"), "{none}");
        assert!(none.contains("Esc closes this"), "{none}");
        // …and with an ended list to offer, it offers it
        let none_ended = text(empty_note(Mode::All, "", false, true, true));
        assert!(none_ended.contains("Tab reaches the conversations that ended"));

        // something IS running, just not in this state
        let filtered = text(empty_note(Mode::Input, "", false, false, true));
        assert!(
            filtered.contains("Nothing is waiting for an answer right now"),
            "{filtered}"
        );
        assert!(!filtered.contains("No agent sessions"), "{filtered}");

        // a query nobody matches, which says what to press to undo it
        let q = text(empty_note(Mode::All, "zzz", false, false, true));
        assert!(q.contains("Nothing matches zzz"), "{q}");
        assert!(q.contains("ctrl-u"), "{q}");

        // the ended list, before anything has ended
        let dead = text(empty_note(Mode::Dead, "", false, false, true));
        assert!(
            dead.contains("No past conversations have been found here yet"),
            "{dead}"
        );

        // …and before the first scan has come back at all, which is the state a
        // popup used to show as an empty box. It outranks every other case,
        // because none of them is known yet.
        let scanning = text(empty_note(Mode::All, "", true, true, true));
        assert!(
            scanning.contains("Looking for agent sessions"),
            "{scanning}"
        );
        assert!(!scanning.contains("No agent sessions"), "{scanning}");
        let scanning_q = text(empty_note(Mode::Input, "zzz", true, false, true));
        assert!(
            scanning_q.contains("Looking for agent sessions"),
            "{scanning_q}"
        );
    }

    /// The default state is an ORDINARY open, which above all means the preview
    /// is ON. Deriving Default gave `preview: false` and every picker opened
    /// with it hidden; the tell was the page keys moving twice as far, since the
    /// list had the preview's half of the window too.
    #[test]
    fn the_default_state_is_an_ordinary_open() {
        let d = State::default();
        assert!(d.preview);
        assert!(!d.search);
        assert!(!d.by_date);
        assert_eq!(Mode::from_key(d.mode), Mode::All);
        assert!(d.query.is_empty() && d.on.is_empty());
    }

    /// tmux shrinks a popup to fit a client that got smaller and grows it back
    /// up to the size it was ASKED for, so the only case the picker has to act
    /// on is a terminal that grew past that. Both directions were measured
    /// before this was written; these are the numbers that came back.
    #[test]
    fn only_a_terminal_that_grew_past_the_popup_counts() {
        // opened at 160x50, so 80% is 128x40 and the usable area 126x38
        assert!(
            !outgrown((126, 38), (160, 50), 2),
            "the size it was opened at is not a reason to reopen"
        );
        // the client grew to 200x60: 80% of that is 160x48, well past 126x38
        assert!(outgrown((126, 38), (200, 60), 2));
        // …and the same popup on a client that SHRANK is tmux's business, not
        // ours: it has already clamped the popup to fit.
        assert!(!outgrown((58, 18), (60, 20), 2));
    }

    /// A column or two of rounding must not close and reopen the popup, and the
    /// rule switches at 100 columns, so a phone rotating between portrait and
    /// landscape crosses it in both directions.
    #[test]
    fn the_slack_stops_a_reopen_over_rounding() {
        // 80 columns is "small", so the popup is 100% wide: 78 usable
        assert!(!outgrown((78, 19), (80, 24), 2));
        // one column of growth is not worth a flicker
        assert!(!outgrown((78, 19), (81, 24), 2));
        // portrait to landscape: 80 -> 140 crosses the rule, 80% of 140 is 112
        assert!(outgrown((78, 19), (140, 40), 2));
    }

    /// What a resize carries over. Losing the query or the cursor to a rotation
    /// would make the reopen worse than the stuck popup it replaces.
    #[test]
    fn a_resize_hands_over_what_the_picker_was_doing() {
        let mut a = app(THREE);
        a.query = "banana".into();
        a.mode = Mode::Run;
        a.search = true;
        a.by_date = true;
        a.preview = false;
        a.query_changed();
        let state = State {
            query: a.query.clone(),
            mode: a.mode.key(),
            search: a.search,
            by_date: a.by_date,
            preview: a.preview,
            on: a.selected().map(|r| r.pane_id.clone()).unwrap_or_default(),
            // The client the popup was on, which the reopen must target rather
            // than asking tmux which one is "current".
            client: "/dev/pts/7".into(),
        };
        assert_eq!(
            state,
            State {
                query: "banana".into(),
                mode: "run",
                search: true,
                by_date: true,
                preview: false,
                on: "%2".into(),
                client: "/dev/pts/7".into(),
            }
        );
        // …and it comes back as the same picker on the other side
        assert_eq!(Mode::from_key(state.mode), Mode::Run);
        assert_eq!(Mode::from_key("outdated"), Mode::Outdated);
        assert_eq!(Mode::from_key(""), Mode::All);
    }

    /// focus() is what covers a REFUSED restart: nothing is held, so the rebuild
    /// has nothing to re-pin to, and without it the cursor fell to the top on
    /// exactly the presses that did nothing.
    #[test]
    fn focus_puts_the_cursor_back_and_is_silent_when_it_cannot() {
        let mut a = app(THREE);
        a.sel = 0;
        assert!(a.focus("%3"));
        assert_eq!(a.selected().map(|r| r.pane_id.as_str()), Some("%3"));
        assert!(!a.focus("%404"));
        assert_eq!(
            a.selected().map(|r| r.pane_id.as_str()),
            Some("%3"),
            "a pane that is not listed leaves the cursor alone"
        );
    }

    /// The picker as `pick` opens it: a scan, plus where the pane it was opened
    /// from is, and no ● row because that pane runs no agent.
    fn app_at(tsv: &str, cwd: &str, target: &str) -> App {
        let mut a = app(tsv);
        a.src.cur_cwd = cwd.into();
        a.src.cur_target = target.into();
        a.focus_nearest();
        a
    }

    fn on(a: &App) -> &str {
        a.selected().map(|r| r.pane_id.as_str()).unwrap_or("")
    }

    /// Four sessions around one project tree, in a session the pane pressing F1
    /// is not in, so nothing but the directory can separate them.
    const TREE: &str = "%1\tw:1.1\t/h/notes\tclaude\t1\tidle\t-\tnotes\n\
                        %2\tw:2.1\t/h/proj/web\tclaude\t1\tidle\t-\tweb\n\
                        %3\tw:3.1\t/h/proj/web/docs\tclaude\t1\tidle\t-\tdocs\n\
                        %4\tw:4.1\t/h/proj\tclaude\t1\tidle\t-\tproj";

    /// F1 from a shell: the pane it was pressed in runs no agent, so there is no
    /// ● row to open on, and the cursor used to land on whichever row the scan
    /// listed first, which is to say on nothing.
    #[test]
    fn a_pane_with_no_agent_opens_on_the_session_in_its_own_directory() {
        assert_eq!(on(&app_at(TREE, "/h/proj/web", "z:1.1")), "%2");
        // the same directory wins however it is written
        assert_eq!(on(&app_at(TREE, "/h/proj/web/", "z:1.1")), "%2");
        assert_eq!(on(&app_at(TREE, "/h/notes", "z:1.1")), "%1");
    }

    /// Nothing is in the directory itself, so the nearest one is taken: DOWN the
    /// tree before up it, since the deeper session is the more specific answer
    /// and the parent is often just where several projects happen to live.
    #[test]
    fn a_subdirectory_beats_the_parent_directory() {
        let two = "%3\tw:3.1\t/h/proj/web/docs\tclaude\t1\tidle\t-\tdocs\n\
                   %4\tw:4.1\t/h/proj\tclaude\t1\tidle\t-\tproj";
        assert_eq!(on(&app_at(two, "/h/proj/web", "z:1.1")), "%3");
        // …and from deeper in, the session on the way back up
        assert_eq!(on(&app_at(TREE, "/h/proj/web/docs/api", "z:1.1")), "%3");
    }

    /// The directory outranks the list: a session next door in the wrong tree is
    /// not the one you are working on.
    #[test]
    fn the_directory_outranks_how_near_the_pane_is() {
        let two = "%1\tw:1.1\t/h/proj\tclaude\t1\tidle\t-\tright tree\n\
                   %2\tw:4.1\t/h/other\tclaude\t1\tidle\t-\tnext door";
        assert_eq!(on(&app_at(two, "/h/proj", "w:5.1")), "%1");
    }

    /// …and only then the list, which is what separates sessions the directory
    /// cannot: the same session first, nearest window in it, then anything else
    /// on this server, then another host, whose window numbers mean nothing here.
    #[test]
    fn equal_directories_are_separated_by_the_tmux_list() {
        let same = "%1\tother:1.1\t/h/proj\tclaude\t1\tidle\t-\tanother session\n\
                    %2\tw:9.1\t/h/proj\tclaude\t1\tidle\t-\tfar window\n\
                    %3\tw:2.1\t/h/proj\tclaude\t1\tidle\t-\tnext window";
        assert_eq!(on(&app_at(same, "/h/proj", "w:3.1")), "%3");

        let remote = "ha:%9\tw:1.1\t/h/proj\tclaude\t1\tidle\t-\tover there\n\
                      %1\tother:1.1\t/h/proj\tclaude\t1\tidle\t-\there";
        assert_eq!(on(&app_at(remote, "/h/proj", "w:3.1")), "%1");
    }

    /// A directory with nothing in common says nothing about which row is
    /// nearer, so the tie falls through to the list rather than to whichever
    /// session happens to sit closest to the root.
    #[test]
    fn a_directory_that_shares_nothing_falls_through_to_the_list() {
        let two = "%1\tw:1.1\t/h/a/b/c\tclaude\t1\tidle\t-\tdeep\n\
                   %2\tw:4.1\t/h/b\tclaude\t1\tidle\t-\tshallow";
        assert_eq!(on(&app_at(two, "/tmp/scratch", "w:5.1")), "%2");
    }

    /// Not knowing where the pane is means not knowing: the cursor stays at the
    /// top rather than moving on a guess. That is every entry point but `pick`.
    #[test]
    fn an_unknown_position_leaves_the_cursor_at_the_top() {
        assert_eq!(on(&app_at(TREE, "", "")), "%1");
    }

    const THREE: &str = "%1\ta:1.1\t/h\tclaude\t1\tidle\t-\tapple pie\n\
                         %2\tb:1.1\t/h\tclaude\t1\trun\t-\tbanana bread\n\
                         %3\tc:1.1\t/h\tclaude\t1\tinput\t-\tcherry tart";

    /// The rows are fitted to the area they are drawn in, less the border and the
    /// pointer. Getting this wrong is invisible until a row is one column too
    /// long and the right-hand columns fall off.
    #[test]
    fn the_row_width_excludes_the_border_and_the_pointer() {
        assert_eq!(row_width(130), 126);
        assert_eq!(row_width(2), 0); // narrower than its own chrome
        assert_eq!(row_width(0), 0);
    }

    #[test]
    fn tab_steps_round_the_cycle_and_starts_over() {
        let mut m = Mode::All;
        let seen: Vec<Mode> = (0..6)
            .map(|_| {
                m = m.next(true, true);
                m
            })
            .collect();
        assert_eq!(
            seen,
            vec![
                Mode::Input,
                Mode::Run,
                Mode::Idle,
                Mode::Outdated,
                Mode::Dead,
                Mode::All
            ]
        );
    }

    /// Shift-tab is the same ring read backwards, so a step each way from
    /// anywhere lands back where it started.
    #[test]
    fn shift_tab_steps_the_other_way_round_the_cycle() {
        let mut m = Mode::All;
        let seen: Vec<Mode> = (0..6)
            .map(|_| {
                m = m.prev(true, true);
                m
            })
            .collect();
        assert_eq!(
            seen,
            vec![
                Mode::Dead,
                Mode::Outdated,
                Mode::Idle,
                Mode::Run,
                Mode::Input,
                Mode::All
            ]
        );
        for m in [
            Mode::All,
            Mode::Input,
            Mode::Run,
            Mode::Idle,
            Mode::Outdated,
            Mode::Dead,
        ] {
            assert_eq!(m.next(true, true).prev(true, true), m);
            assert_eq!(m.prev(true, true).next(true, true), m);
        }
    }

    /// Ended is skipped where there is nothing to show, rather than trapping the
    /// picker in a mode with no rows in it.
    #[test]
    fn the_ended_mode_is_skipped_without_a_sessions_cache() {
        assert_eq!(Mode::Idle.next(false, false), Mode::All);
        assert_eq!(Mode::Idle.next(true, false), Mode::Dead);
        // and backwards it is skipped the same way
        assert_eq!(Mode::All.prev(false, false), Mode::Idle);
        assert_eq!(Mode::All.prev(true, false), Mode::Dead);
    }

    /// …and so is outdated, where nothing is installed to judge a version
    /// against: every row would be measured against an empty version, so the
    /// list could only ever be empty.
    #[test]
    fn the_outdated_mode_is_skipped_when_no_version_is_installed() {
        assert_eq!(Mode::Idle.next(false, true), Mode::Outdated);
        assert_eq!(Mode::Outdated.next(false, true), Mode::All);
        assert_eq!(Mode::Outdated.next(true, true), Mode::Dead);
        // both gates off: idle is the last stop
        assert_eq!(Mode::Idle.next(false, false), Mode::All);
        // …and going back from the first stop skips it just the same
        assert_eq!(Mode::All.prev(false, false), Mode::Idle);
        assert_eq!(Mode::All.prev(false, true), Mode::Outdated);
    }

    #[test]
    fn the_label_says_which_list_and_what_is_on() {
        assert_eq!(
            label(Mode::All, false, false, false, false),
            " agent sessions "
        );
        assert_eq!(
            label(Mode::Input, false, true, false, false),
            " waiting for an answer · live "
        );
        assert_eq!(
            label(Mode::Outdated, false, false, false, false),
            " running outdated code "
        );
        assert_eq!(
            label(Mode::Dead, false, true, true, false),
            " past sessions · live · ⌕ "
        );
        // the order goes straight after the name, since it describes that list
        assert_eq!(
            label(Mode::Dead, true, true, true, false),
            " past sessions · by date · live · ⌕ "
        );
    }

    /// Past conversations, newest first as the list arrives. Two SAY "tart", the
    /// older one where a word starts, which the matcher ranks higher; the newest
    /// of all only has its letters, scattered across four words.
    const PAST: &str =
        "dead:claude:/n.jsonl\t30m\t/h/a\tclaude\t1\tdead\t-\ttrial and error then tests\n\
         dead:claude:/r.jsonl\t1h\t/h/r\tclaude\t1\tdead\t-\trestart the ledger\n\
         dead:claude:/m.jsonl\t2d\t/h/b\tclaude\t1\tdead\t-\tapple pie\n\
         dead:claude:/o.jsonl\t9d\t/h/c\tclaude\t1\tdead\t-\tcherry tart";

    fn app_past(query: &str) -> App {
        let mut a = app(THREE);
        a.src.ended = Some(Box::new(|| PAST.to_string()));
        a.mode = Mode::Dead;
        a.query = query.into();
        a.fetch();
        a.rebuild();
        a
    }

    /// The reason for the key. A query ranks the past list best match first,
    /// which scatters what it keeps across the months; by date keeps the same
    /// matches, newest first, the order the list had before anything was typed.
    #[test]
    fn ctrl_s_keeps_the_past_list_newest_first_while_a_query_filters_it() {
        let mut a = app_past("tart");
        assert_eq!(
            ids(&a),
            [
                "dead:claude:/o.jsonl",
                "dead:claude:/r.jsonl",
                "dead:claude:/n.jsonl"
            ],
            "ranked, the word start wins, and the loose match comes last"
        );
        a.by_date = true;
        a.rebuild();
        assert_eq!(
            ids(&a),
            [
                "dead:claude:/r.jsonl",
                "dead:claude:/o.jsonl",
                "dead:claude:/n.jsonl"
            ],
            "by date, the two that say it newest first, THEN the loose one"
        );
        // …and typing more keeps that order rather than ranking again
        a.query = "tart e".into();
        a.query_changed();
        assert_eq!(
            ids(&a),
            [
                "dead:claude:/r.jsonl",
                "dead:claude:/o.jsonl",
                "dead:claude:/n.jsonl"
            ]
        );
    }

    /// Why by date is not simply "leave every match where it stands". The
    /// letters of a query turn up scattered in rows that have nothing to do with
    /// it, and the ranking was what kept those under the rows that say it: tried
    /// on the real history without this, the top of the list was all noise. So
    /// the loose match stays last even as the newest row there is.
    #[test]
    fn by_date_keeps_the_loose_matches_under_the_ones_that_say_it() {
        let mut a = app_past("tart");
        a.by_date = true;
        a.rebuild();
        assert_eq!(
            ids(&a).last().map(String::as_str),
            Some("dead:claude:/n.jsonl")
        );
        // With nothing but loose matches, they are simply newest first.
        a.query = "tlr".into();
        a.query_changed();
        assert_eq!(ids(&a), ["dead:claude:/n.jsonl", "dead:claude:/r.jsonl"]);
    }

    /// The cursor stays on the conversation it was on, so a second press lands
    /// exactly where the first one started.
    #[test]
    fn sorting_by_date_keeps_the_cursor_on_the_same_conversation() {
        let mut a = app_past("tart");
        assert_eq!(on(&a), "dead:claude:/o.jsonl");
        a.by_date = true;
        a.rebuild();
        assert_eq!(on(&a), "dead:claude:/o.jsonl");
        assert_eq!(a.sel, 1, "which is now below the newer one");
        a.by_date = false;
        a.rebuild();
        assert_eq!((on(&a), a.sel), ("dead:claude:/o.jsonl", 0));
    }

    /// Only the past list has a date to go by. Left on through a Tab, the flag
    /// must not quietly stop the live lists ranking what a query keeps.
    #[test]
    fn by_date_is_the_past_lists_alone() {
        let mut a = app("%1\ta:1.1\t/h\tclaude\t1\tidle\t-\trestart the ledger\n\
                         %2\tb:1.1\t/h\tclaude\t1\tidle\t-\tcherry tart");
        a.by_date = true;
        a.query = "tart".into();
        a.rebuild();
        assert!(!a.dated(), "a live list is never in date order");
        assert_eq!(ids(&a), ["%2", "%1"], "so a query still ranks it");
    }

    /// The list ctrl-x and F8 act on, gathered in one place. It crosses the four
    /// state modes, because being behind is not a state.
    #[test]
    fn the_outdated_mode_lists_the_rows_a_restart_would_act_on() {
        let mut a = app(VERSIONS);
        a.src.newver = "2.1.243".into();
        a.mode = Mode::Outdated;
        a.rebuild();
        assert_eq!(ids(&a), ["%1", "%2"], "behind, whatever they are doing");

        // …and with nothing installed to compare against, nothing is behind.
        a.src.newver = String::new();
        a.rebuild();
        assert!(a.view.is_empty());
    }

    const VERSIONS: &str = "%1\ta:1.1\t/h\tclaude\t2.1.229\tidle\t-\tbehind\n\
                            %2\tb:1.1\t/h\tclaude\t2.1.229\tinput\t-\tbehind and asking\n\
                            %3\tc:1.1\t/h\tclaude\t2.1.243\trun\t-\tcurrent\n\
                            ha:%4\td:1.1\t/h\tclaude\t2.1.229\tidle\t-\tover there";

    /// The stamp names the tool as well as the version, or a bare number in a
    /// corner would read as one more agent version like the ones down the right
    /// of every row. It is the crate version, which is what `taimux version`
    /// prints and what release-please bumps, so the three cannot drift.
    #[test]
    fn the_stamp_names_the_tool_and_carries_the_crate_version() {
        let tag = version_tag();
        assert!(tag.contains("taimux"));
        assert!(tag.contains(env!("CARGO_PKG_VERSION")));
        // padded both sides, so it does not touch the border corner
        assert!(tag.starts_with(' ') && tag.ends_with(' '));
    }

    /// The stamp gives way to the count, never the other way round: ratatui
    /// gives a right-aligned title precedence, so without the check the count
    /// is what gets eaten, and the count is the live half.
    #[test]
    fn the_stamp_yields_to_the_count_on_a_narrow_border() {
        let count = " 5/5 ";
        let need = count.len() + version_tag().chars().count() + 2;
        assert!(room_for_tag(need as u16, count));
        assert!(!room_for_tag(need as u16 - 1, count));
        // a four-figure list needs more room for the same window
        assert!(!room_for_tag(need as u16, " 1000/1000 "));
        // and a window narrower than the stamp alone never gets it
        assert!(!room_for_tag(16, count));
    }

    /// The header only ever advertises what is really bound: a key that does
    /// nothing is worse than a shorter header.
    #[test]
    fn the_header_advertises_only_bound_keys() {
        let bare = header(false, false, false, false, None);
        assert!(!bare.contains("ctrl-x"));
        assert!(!bare.contains("resume"));
        assert!(!bare.contains("ctrl-t"));
        assert!(!bare.contains("ctrl-s"));
        assert!(header(true, false, false, false, None).contains("ctrl-x"));
        assert!(header(false, true, false, false, None).contains("enter: switch/resume"));
        assert!(header(false, false, true, true, None).contains("(on)"));
        assert!(!header(false, false, true, false, None).contains("(on)"));
        // ctrl-s is bound on the past list, and only said there
        let past = header(false, true, false, false, Some(false));
        assert!(past.contains("ctrl-s: sort by date") && !past.contains("(on)"));
        assert!(header(false, true, false, false, Some(true)).contains("ctrl-s: sort by date (on)"));
    }

    #[test]
    fn a_mode_shows_only_that_state() {
        let mut a = app(THREE);
        assert_eq!(a.view.len(), 3);
        a.mode = Mode::Run;
        a.rebuild();
        assert_eq!(a.view.len(), 1);
        assert!(a.selected().unwrap().plain().contains("banana"));
    }

    #[test]
    fn the_query_filters_and_ranks() {
        let mut a = app(THREE);
        a.query = "banana".into();
        a.view = filter(&a.all, &a.query, &a.matcher, false);
        assert_eq!(a.view.len(), 1);

        // an AND of terms, as fzf's extended search does, not one fuzzy match
        a.query = "apple tart".into();
        a.view = filter(&a.all, &a.query, &a.matcher, false);
        assert!(a.view.is_empty());
    }

    #[test]
    fn no_query_keeps_the_lists_own_order() {
        let a = app(THREE);
        assert_eq!(a.view, vec![0, 1, 2]);
    }

    /// A rebuild puts the cursor back on the same SESSION, not the same index.
    /// That is what --track --id-nth=2 buys fzf, and it matters because the
    /// refresh timer rebuilds under you while you are moving.
    #[test]
    fn a_rebuild_keeps_the_cursor_on_the_same_session() {
        let mut a = app(THREE);
        a.sel = 2;
        let was = a.selected().unwrap().pane_id.clone();
        // a session vanishes from the top of the list
        a.src = src("%2\tb:1.1\t/h\tclaude\t1\trun\t-\tbanana bread\n\
                     %3\tc:1.1\t/h\tclaude\t1\tinput\t-\tcherry tart");
        a.fetch();
        a.rebuild();
        assert_eq!(a.selected().unwrap().pane_id, was);
        assert_eq!(a.sel, 1);
    }

    #[test]
    fn a_cursor_whose_row_is_gone_falls_back_to_the_top() {
        let mut a = app(THREE);
        a.sel = 2;
        a.src = src("%1\ta:1.1\t/h\tclaude\t1\tidle\t-\tapple pie");
        a.fetch();
        a.rebuild();
        assert_eq!(a.sel, 0);
    }

    #[test]
    fn the_cursor_wraps_both_ways() {
        let mut a = app(THREE);
        a.move_by(-1);
        assert_eq!(a.sel, 2);
        a.move_by(1);
        assert_eq!(a.sel, 0);
    }

    /// A page CLAMPS where a single step wraps, and the difference is the point:
    /// holding Page Down to reach the bottom of a long list must not sail past
    /// the end and land back at the top, with nothing on the row to say so.
    #[test]
    fn a_page_clamps_where_a_single_step_wraps() {
        let mut a = app(THREE);
        a.move_page(1, 2);
        assert_eq!(a.sel, 2);
        a.move_page(1, 2); // already at the end, and it stays there
        assert_eq!(a.sel, 2);
        a.move_page(-1, 2);
        assert_eq!(a.sel, 0);
        a.move_page(-1, 2);
        assert_eq!(a.sel, 0);
    }

    /// A list drawn zero rows tall (a pane too short for one) would otherwise
    /// make the key do nothing at all, which reads as the key being unbound.
    #[test]
    fn a_page_of_no_rows_still_moves_one() {
        let mut a = app(THREE);
        a.move_page(1, 0);
        assert_eq!(a.sel, 1);
    }

    /// Page Up and Page Down were the two keys the port dropped: fzf bound them
    /// itself, `Home` and `End` were ported by hand and these were not, so one
    /// pair kept working and the other went quiet. Nothing failed, which is why
    /// it took a report. The suite drives the real keys in a real terminal (see
    /// tests/run.sh); this is the arithmetic underneath.
    #[test]
    fn an_empty_view_pages_without_panicking() {
        let mut a = app(THREE);
        a.query = "zzzzz".into();
        a.query_changed();
        assert!(a.view.is_empty());
        a.move_page(1, 8);
        a.move_page(-1, 8);
        assert_eq!(a.sel, 0);
    }

    /// An empty list must not be indexed into, and every key still has to work on
    /// one: a query that matches nothing is the ordinary way to get here.
    #[test]
    fn an_empty_view_is_safe_to_navigate() {
        let mut a = app(THREE);
        a.query = "zzzzz".into();
        a.view = filter(&a.all, &a.query, &a.matcher, false);
        assert!(a.view.is_empty());
        a.move_by(1);
        a.move_by(-1);
        a.clamp();
        assert!(a.selected().is_none());
    }

    /// The padding `capture-pane` adds is what made every waiting session read as
    /// idle when the state reader was ported. Same capture, same trap, so the
    /// preview trims before it takes a tail.
    #[test]
    fn the_preview_tail_ignores_the_padding_capture_pane_adds() {
        let screen = "one\ntwo\nthree\n\n\n\n\n\n\n\n";
        let t = tail(screen, 2);
        let text: Vec<String> = t
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.to_string()).collect())
            .collect();
        assert_eq!(text, vec!["two", "three"]);
    }

    #[test]
    fn a_screen_shorter_than_the_room_is_shown_whole() {
        assert_eq!(tail("one\ntwo\n", 40).len(), 2);
        assert!(tail("", 40).is_empty());
        assert!(tail("\n\n\n", 40).is_empty());
    }

    /// The whole point of the exercise: a paste is text, never an Enter. Its
    /// first line joins the query and the rest is dropped, rather than being
    /// submitted into whatever is behind the picker.
    #[test]
    fn a_pasted_newline_stays_out_of_the_query() {
        let text = "set -g @plugin foo\rdo not write below this line";
        let first = text.split(['\r', '\n']).next().unwrap();
        assert_eq!(first, "set -g @plugin foo");
    }
}
