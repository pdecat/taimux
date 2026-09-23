//! The picker's rows, laid out here rather than in awk.
//!
//! Step 2 of removing fzf. The awk this replaces exists to hand fzf one TSV line
//! per row with the colours already burned in as escape sequences; that format
//! dies with fzf, so the layout is ported into structured cells and the ANSI is
//! reduced to one renderer used for testing.
//!
//! **The layout itself is not being redesigned.** Every rule here was arrived at
//! by looking at real lists and most of them fix a specific bent row, so they are
//! ported as they stand and the reasoning is kept with them:
//!
//! - The summary follows the label directly, because it is what the list is read
//!   for. Everything that only says WHERE a session is (path, agent, version) is
//!   pinned to the right edge, in fixed columns, so it costs the summary no width
//!   and is what a too-long row truncates away.
//! - Every column width is measured over the WHOLE list, never per row. Sizing
//!   the trailing block per row moved the path column by however long that row's
//!   agent happened to be, up to 9 columns apart on a mixed list.
//! - The label column has a floor of 15 in a roomy window and a cap of 15 in a
//!   narrow one, and the floor only applies when the list holds real pane labels.
//!
//! The one deliberate improvement is `vlen`: the awk counts characters, this
//! counts display columns, which is the same answer for everything on an ordinary
//! list and the right one for a double-width character.

use std::collections::{HashMap, HashSet};

use ratatui::style::{Color, Modifier, Style};
use unicode_width::UnicodeWidthStr;

/// The paint a cell carries, stored as the SGR prefix the awk emits so the ANSI
/// renderer is exact, with the ratatui mapping beside it so the TUI never parses
/// its own escape sequences back.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Paint(pub &'static str);

pub const PLAIN: Paint = Paint("");
/// The label of the pane the picker was opened from.
pub const LABEL_CUR: Paint = Paint("\x1b[33m");
pub const LABEL_OTHER: Paint = Paint("\x1b[36m");
/// A session asking for you: the one glyph here worth a colour of its own.
pub const MARK_INPUT: Paint = Paint("\x1b[1;33m");
/// Working, dimmed, because it wants nothing from you.
pub const MARK_RUN: Paint = Paint("\x1b[2m");
/// A restart in flight. Cyan rather than the waiting star's bold yellow: it
/// wants nothing from you, it is just not finished, and it should not compete
/// with the one glyph that means "this row is asking".
pub const MARK_RESTART: Paint = Paint("\x1b[36m");
pub const PATH: Paint = Paint("\x1b[90m");
/// The permission mode rides on the agent name as brightness rather than taking
/// a column: louder is less supervised.
pub const MODE_ASK: Paint = Paint("\x1b[35m");
pub const MODE_EDIT: Paint = Paint("\x1b[95m");
pub const MODE_AUTO: Paint = Paint("\x1b[1;95m");
/// A session running code a self-update has already replaced. Plain yellow, not
/// the bold yellow of the star, since nothing is being asked of you.
pub const VER_STALE: Paint = Paint("\x1b[33m");
pub const VER_OK: Paint = Paint("\x1b[2;35m");

impl Paint {
    pub fn style(self) -> Style {
        match self.0 {
            "\x1b[33m" => Style::default().fg(Color::Yellow),
            "\x1b[36m" => Style::default().fg(Color::Cyan),
            "\x1b[1;33m" => Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
            "\x1b[2m" => Style::default().add_modifier(Modifier::DIM),
            "\x1b[90m" => Style::default().fg(Color::DarkGray),
            "\x1b[35m" => Style::default().fg(Color::Magenta),
            "\x1b[95m" => Style::default().fg(Color::LightMagenta),
            "\x1b[1;95m" => Style::default()
                .fg(Color::LightMagenta)
                .add_modifier(Modifier::BOLD),
            "\x1b[2;35m" => Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::DIM),
            _ => Style::default(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cell {
    pub text: String,
    pub paint: Paint,
}

fn cell(text: impl Into<String>, paint: Paint) -> Cell {
    Cell {
        text: text.into(),
        paint,
    }
}

#[derive(Clone, Debug)]
pub struct Row {
    pub cells: Vec<Cell>,
    pub pane_id: String,
    /// Kept unabbreviated and unshortened for the preview header, which has the
    /// room the row does not and wants to say exactly where the session is.
    pub target: String,
    pub cwd: String,
    /// Set for a session on another machine. capture-pane only works where the
    /// pane is, so the preview has to ask over there rather than locally.
    pub host: String,
    /// When the session last said something, in epoch milliseconds, which is
    /// what the idle and past lists sort on by date. None where nothing tells:
    /// an agent other than claude, a claude with no hook line, or another host
    /// whose taimux predates the field.
    pub since: Option<i64>,
}

impl Row {
    /// The exact line the awk prints, for diffing one implementation against the
    /// other. Nothing in the TUI calls this: it renders the cells directly.
    pub fn to_ansi(&self) -> String {
        let mut s = String::new();
        for c in &self.cells {
            if c.paint == PLAIN {
                s.push_str(&c.text);
            } else {
                s.push_str(c.paint.0);
                s.push_str(&c.text);
                s.push_str("\x1b[0m");
            }
        }
        s.push('\t');
        s.push_str(&self.pane_id);
        s
    }

    /// The row with every escape sequence gone, which is what a width assertion
    /// and a fuzzy match both want. fzf has to be handed `--ansi` and parse our
    /// colours back out to get at this; the TUI has it for free.
    #[allow(dead_code)] // the in-memory filter is step 3
    pub fn plain(&self) -> String {
        self.cells.iter().map(|c| c.text.as_str()).collect()
    }
}

/// Display width. The awk counts characters (gawk in a UTF-8 locale) and hand
/// strips UTF-8 continuation bytes where it cannot; this counts columns, which
/// agrees for everything an ordinary list holds and is right where the awk was
/// quietly wrong.
fn vlen(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

fn spaces(n: usize) -> String {
    " ".repeat(n)
}

/// Pad on the right to a column width, never truncating: a column measured over
/// the whole list is already wide enough, and a row that overruns it is a bug
/// worth seeing rather than hiding.
fn pad(s: &str, w: usize) -> String {
    let mut out = s.to_string();
    out.push_str(&spaces(w.saturating_sub(vlen(s))));
    out
}

/// Whatever marker the title leads with, taken off so the column can be filled
/// from the STATE instead. The title shows the same star for all three states
/// (and sometimes a spinner frame that means nothing in particular), so the glyph
/// it carries is dropped rather than shown.
///
/// A marker is "a leading run of non-printable-ASCII, then a space or the end",
/// not a list of the known frames: this only has to find where the summary
/// starts, so a glyph nobody has seen yet is still stripped cleanly. A title that
/// merely opens on a non-ASCII WORD is left whole, since the space has to follow
/// the run directly.
pub fn summary_of(title: &str) -> &str {
    let run: usize = title
        .chars()
        .take_while(|c| !(' '..='~').contains(c))
        .map(|c| c.len_utf8())
        .sum();
    if run == 0 {
        return title;
    }
    let rest = &title[run..];
    match rest.strip_prefix(' ') {
        Some(r) => r,
        None if rest.is_empty() => rest,
        None => title,
    }
}

/// The permission mode, in brightness. Ordinary magenta for a session that will
/// stop and ask (default, plan, or nothing known), bright for one applying edits
/// on its own, bold bright for one that asks nothing at all.
fn mode_paint(mode: &str) -> Paint {
    match mode {
        "acceptEdits" => MODE_EDIT,
        "bypassPermissions" | "auto" => MODE_AUTO,
        _ => MODE_ASK,
    }
}

/// Is this row running code a self-update has already replaced, i.e. one ctrl-x
/// applies to?
///
/// Never on another host: `newver` is what THIS box would start, and a session on
/// one host being behind another is not a fact about anything. Never on an ENDED
/// session either, however far behind it last ran: there is no process to put
/// back.
///
/// One predicate, two uses: the yellow version on a row, and the outdated list
/// Tab stops on. They have to agree, or the list would hold rows whose colour
/// says nothing is wrong with them, or leave out ones it paints yellow.
fn outdated(host: &str, agent: &str, v: &str, state: &str, newver: &str) -> bool {
    state != "dead"
        && host.is_empty()
        && agent == "claude"
        && !newver.is_empty()
        && !v.is_empty()
        && v != newver
}

/// Yellow means "ctrl-x applies to this row".
fn version_paint(host: &str, agent: &str, v: &str, state: &str, newver: &str) -> Paint {
    if outdated(host, agent, v, state, newver) {
        VER_STALE
    } else {
        VER_OK
    }
}

/// Each name cut to the fewest letters that still tell it apart from every other
/// name in the list: one where nothing else starts with it, more only against the
/// names it actually collides with. A name that is a whole other name plus
/// something (main, main2) can only be told apart in full.
///
/// `minlen` is a floor. Sessions take 1, since you picked those names and they
/// are on screen constantly. Hosts take 2, because a host is the part of a row
/// you are least likely to have in your head, and "l" for laptop-two saves nine
/// columns by giving up the whole point of the column.
fn abbrev(names: &[String], minlen: usize) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for s in names {
        let chars: Vec<char> = s.chars().collect();
        let mut n = chars.len() + 1; // nothing distinguished it: keep it whole
        for k in 1..=chars.len() {
            let head: String = chars.iter().take(k).collect();
            let clash = names
                .iter()
                .any(|t| t != s && t.chars().take(k).collect::<String>() == head);
            if !clash {
                n = k;
                break;
            }
        }
        let n = n.max(minlen);
        out.insert(s.clone(), chars.iter().take(n).collect());
    }
    out
}

/// The last two components of the working directory, with $HOME folded to `~`
/// and a cap of 30 characters, elided from the left because the tail is the part
/// that says which project it is.
fn path_display(cwd: &str, home: &str) -> String {
    let cwd = if !home.is_empty() && cwd.starts_with(home) {
        format!("~{}", &cwd[home.len()..])
    } else {
        cwd.to_string()
    };
    let parts: Vec<&str> = cwd.split('/').collect();
    let disp = if parts.len() >= 2 {
        format!("{}/{}", parts[parts.len() - 2], parts[parts.len() - 1])
    } else {
        cwd.clone()
    };
    let n = disp.chars().count();
    if n > 30 {
        // the awk takes from length-28 to the end, i.e. the last 29 characters
        let tail: String = disp.chars().skip(n - 29).collect();
        format!("…{}", tail)
    } else {
        disp
    }
}

/// Does the row already show everything that was typed? If it does it needs no
/// explaining, and keeping its path, agent and version is worth more than quoting
/// the query back at it.
///
/// Case-insensitive, as the content search now is: `terms` arrive folded. The
/// two have to agree, or a row kept because of a capital the transcript spells
/// differently would still be told it says nothing.
fn says_it(s: &str, terms: &[String]) -> bool {
    if terms.is_empty() {
        return false;
    }
    let hay = s.to_lowercase();
    terms
        .iter()
        .all(|t| t.is_empty() || hay.contains(t.as_str()))
}

/// Trailing spaces align nothing, so the all-blank remainder a row with no agent
/// or version leaves is trimmed off. Done over the cells rather than the rendered
/// string, since a painted cell ends in a reset and would block the trim.
fn trim_trailing(cells: &mut Vec<Cell>) {
    while let Some(last) = cells.last_mut() {
        if last.paint != PLAIN {
            break;
        }
        let trimmed = last.text.trim_end_matches(' ');
        if trimmed.len() == last.text.len() {
            break; // ended in something other than a space: nothing to trim
        }
        last.text.truncate(trimmed.len());
        if !last.text.is_empty() {
            break; // the run of spaces ended inside this cell
        }
        cells.pop();
    }
}

/// One input line, before anything is measured.
struct Item {
    id: String,
    target: String,
    agent: String,
    version: String,
    state: String,
    mode: String,
    title: String,
    host: String,
    /// The one row a host that could not answer keeps in the list. Left out of
    /// the shortening and printed whole: the host name IS the message.
    note: bool,
    /// The last two components, capped, as the row shows it.
    path: String,
    /// The whole thing, as the preview header shows it.
    cwd: String,
    session: String,
    since: Option<i64>,
}

#[derive(Default)]
pub struct Input<'a> {
    pub cur: &'a str,
    /// 0 means "unknown", which reads as "do not right-align".
    pub width: usize,
    pub home: &'a str,
    /// What a session started right now would run, so a pane left behind by a
    /// self-update can be told apart from a current one.
    pub newver: &'a str,
    /// One state only, or empty for all.
    pub only: &'a str,
    /// Keep only the rows a restart would act on, i.e. the ones the version
    /// column paints yellow.
    ///
    /// Separate from `only` because being behind is not a STATE: it is the row's
    /// version against the one installed here, and a session waiting, working or
    /// idle can each be behind. Which is also why this list crosses the four
    /// state modes rather than sitting inside one of them.
    pub outdated: bool,
    pub query: &'a str,
    /// pane id to the snippet of what that session said, when searching.
    ///
    /// The layout applies whatever it is given. **The "at least
    /// TAIMUX_SEARCH_MIN characters" gate lives in the CALLER**, exactly as it
    /// does in bash: under three characters a term is in every transcript and a
    /// match would say nothing, so no snippets are looked up at all. Handing this
    /// a snippet map for a one-letter query would quietly turn every row into a
    /// search hit.
    pub snips: HashMap<String, String>,
    /// pane id to the title its conversation last recorded, for a pane that
    /// publishes none of its own.
    pub ptitles: HashMap<String, String>,
    /// Panes with a restart in flight.
    ///
    /// This paints the marker column and nothing else. In particular it does NOT
    /// touch the state field, which is what keeps such a row where it was: the
    /// state drives both the Tab filter and the row's place in the list, so a
    /// synthetic "restarting" state would drop the row out of whichever mode it
    /// was being watched in, at the exact moment its owner is watching it.
    pub restarting: HashSet<String>,
}

pub fn build(lines: &str, input: &Input) -> Vec<Row> {
    // Folded, because the search behind them is: see `says_it`.
    let terms: Vec<String> = input
        .query
        .split([' ', '\t'])
        .filter(|t| !t.is_empty())
        .map(|t| t.to_lowercase())
        .collect();

    let mut items: Vec<Item> = Vec::new();
    for line in lines.lines() {
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() < 7 {
            continue;
        }
        let state = f[5];
        if !input.only.is_empty() && state != input.only {
            continue;
        }
        let (id, target, cwd) = (f[0], f[1], f[2]);

        // A pane id that does not open on "%" names another host, and the label
        // leads with it: "ha/main:1.7". What follows the colon says which kind of
        // row it is, a pane id for a session over there, anything else for a
        // host that could not answer.
        let (mut host, mut note) = (String::new(), false);
        if !id.starts_with('%') {
            if let Some(c) = id.find(':') {
                if c > 0 {
                    if id[c + 1..].starts_with('%') {
                        host = id[..c].to_string();
                    } else {
                        note = true;
                    }
                }
            }
        }
        // Applied here rather than beside `only` because it needs the host, and
        // the host is what the id above has just been read for.
        if input.outdated && !outdated(&host, f[3], f[4], state, input.newver) {
            continue;
        }
        let session = match target.find(':') {
            Some(c) if c > 0 => target[..c].to_string(),
            _ => target.to_string(),
        };
        items.push(Item {
            id: id.to_string(),
            target: target.to_string(),
            agent: f[3].to_string(),
            version: f[4].to_string(),
            state: state.to_string(),
            mode: f[6].to_string(),
            title: f.get(7).copied().unwrap_or("").to_string(),
            host,
            note,
            path: path_display(cwd, input.home),
            cwd: cwd.to_string(),
            session,
            // A ninth field, where the row has one: `-` or nothing at all is
            // "not known", and so is anything that is not a number.
            since: f.get(8).and_then(|s| s.parse().ok()),
        });
    }

    // Narrow window: the session name is the first thing asked to give columns
    // back. Of everything on the row it is the most recognisable from a few
    // letters, and window.pane stays whole since two digits are no use truncated.
    // The threshold is the one the tmux binding already uses to switch the popup
    // to full width.
    let compact = input.width > 0 && input.width < 100;
    let mut names: Vec<String> = Vec::new();
    let mut hnames: Vec<String> = Vec::new();
    for it in &items {
        if !it.note && !names.contains(&it.session) {
            names.push(it.session.clone());
        }
        if !it.host.is_empty() && !hnames.contains(&it.host) {
            hnames.push(it.host.clone());
        }
    }
    let (short, shorth) = if compact {
        (abbrev(&names, 1), abbrev(&hnames, 2))
    } else {
        (HashMap::new(), HashMap::new())
    };

    // The label column is as wide as the widest label actually in THIS list,
    // never a guess. A flat 15 broke the moment a target needed more:
    // "platform:14.11" is 14 plus the 2-column marker, so that one row started
    // its summary a column right of every other and the whole list looked bent.
    let mut labels: Vec<String> = Vec::new();
    let mut labelw = 0;
    for it in &items {
        let lbl = if it.note {
            it.target.clone()
        } else {
            let pfx = if it.host.is_empty() {
                String::new()
            } else if compact {
                format!("{}/", shorth.get(&it.host).unwrap_or(&it.host))
            } else {
                format!("{}/", it.host)
            };
            let body = if compact {
                let s = short.get(&it.session).cloned().unwrap_or_default();
                format!("{}{}", s, &it.target[it.session.len()..])
            } else {
                it.target.clone()
            };
            format!("{}{}", pfx, body)
        };
        labelw = labelw.max(vlen(&lbl) + 2);
        labels.push(lbl);
    }
    // A narrow window CAPS it: there the label is the column asked to give width
    // back to the summary, and a shortened name that still overruns is worth a
    // bent row. A roomy window gets a FLOOR instead, so the column stops
    // jittering as sessions with longer names come and go. The old code applied
    // 15 as a ceiling in BOTH, which is what bent the row.
    //
    // The floor only holds up a column of PANE labels, which is what it is for. A
    // list of ended sessions labels no pane (the column holds an age, three
    // characters of it), so the floor there would spend twelve columns of summary
    // on nothing at all.
    let panes = items.iter().filter(|i| !i.note).count();
    if compact {
        labelw = labelw.min(15);
    } else if panes > 0 {
        labelw = labelw.max(15);
    }

    // The trailing columns are a TABLE, so their widths come from the whole list
    // too. Right-aligning "<path> <agent> <version>" as ONE string moves the path
    // column by however long the agent and version on THAT row happen to be, and
    // no two agents are the same length.
    let mut agw = 0;
    let mut verw = 0;
    let mut pathw = 0;
    for it in &items {
        agw = agw.max(vlen(&it.agent));
        verw = verw.max(vlen(&it.version));
        pathw = pathw.max(vlen(&it.path));
    }
    pathw = pathw.min(30);
    let tailw = pathw + 1 + agw + if verw > 0 { 1 + verw } else { 0 };

    let mut out = Vec::new();
    for (i, it) in items.iter().enumerate() {
        let is_cur = it.id == input.cur;
        let mark = if is_cur { "● " } else { "  " };
        let plabel = pad(&format!("{}{}", mark, labels[i]), labelw);

        let mut sum = summary_of(&it.title).to_string();
        // Nothing on the pane: fall back to what its conversation calls itself.
        // claude sets a title at a turn boundary, so a session restored by
        // tmux-resurrect and not prompted since has nothing there.
        if sum.is_empty() {
            if let Some(t) = input.ptitles.get(&it.id) {
                sum = t.clone();
            }
        }
        // A summary that will not fit gives way, rather than pushing the table
        // off the right edge. Nothing needed this while every summary came from
        // a pane title, which is a handful of words; a PAST session's summary is
        // whatever it was asked to do, up to eighty characters of it, and those
        // rows arrived shoving the directory, the agent and the version out of
        // the window. The columns beside it are the ones you read down the list,
        // so the summary is the one that can afford to end in an ellipsis.
        sum = fit(&sum, summary_room(input.width, vlen(&plabel), tailw));

        let mut cells = vec![
            cell(plabel.clone(), if is_cur { LABEL_CUR } else { LABEL_OTHER }),
            cell(" ", PLAIN),
        ];
        // A restart in flight outranks the state, because during one the state is
        // whatever the screen happened to show as the old session went away, and
        // that is the least useful thing the column could say. The glyph goes
        // here rather than into the summary because the summary strips a leading
        // marker (see summary_of), so one put there would be silently eaten.
        if input.restarting.contains(&it.id) {
            cells.push(cell("↻", MARK_RESTART));
            cells.push(cell(" ", PLAIN));
        } else {
            match it.state.as_str() {
                "input" => {
                    cells.push(cell("✳", MARK_INPUT));
                    cells.push(cell(" ", PLAIN));
                }
                "run" => {
                    cells.push(cell("◐", MARK_RUN));
                    cells.push(cell(" ", PLAIN));
                }
                _ => cells.push(cell("  ", PLAIN)),
            }
        }
        cells.push(cell(sum.clone(), PLAIN));

        // A row that is here because of what its session SAID takes the snippet
        // where its path would be. Two things at once: the row stops being a
        // mystery, and the words typed are now ON it, which is what lets the
        // matcher keep working in the ordinary way rather than being handed a
        // blob it would match everything against.
        let snip = input.snips.get(&it.id).filter(|_| {
            !says_it(
                &format!("{} {} {} {} {}", plabel, sum, it.path, it.agent, it.version),
                &terms,
            )
        });
        if let Some(s) = snip {
            let stail = format!("⌕ {}", s);
            let gap = gap_of(input.width, &plabel, &sum, vlen(&stail));
            cells.push(cell(spaces(gap), PLAIN));
            cells.push(cell(stail, PATH));
        } else {
            let gap = gap_of(input.width, &plabel, &sum, tailw);
            cells.push(cell(spaces(gap), PLAIN));
            cells.push(cell(pad(&it.path, pathw), PATH));
            cells.push(cell(" ", PLAIN));
            // Agent and version are right-aligned inside their columns, so the
            // row stays flush with the right edge and the version numbers read
            // down the list. A row missing either keeps the column: what it must
            // not do is pull the ones beside it out of line.
            if it.agent.is_empty() {
                cells.push(cell(spaces(agw), PLAIN));
            } else {
                cells.push(cell(spaces(agw - vlen(&it.agent)), PLAIN));
                cells.push(cell(it.agent.clone(), mode_paint(&it.mode)));
            }
            if verw > 0 {
                if it.version.is_empty() {
                    cells.push(cell(format!(" {}", spaces(verw)), PLAIN));
                } else {
                    cells.push(cell(
                        format!(" {}", spaces(verw - vlen(&it.version))),
                        PLAIN,
                    ));
                    cells.push(cell(
                        it.version.clone(),
                        version_paint(&it.host, &it.agent, &it.version, &it.state, input.newver),
                    ));
                }
            }
        }
        trim_trailing(&mut cells);
        cells.retain(|c| !c.text.is_empty());
        out.push(Row {
            cells,
            pane_id: it.id.clone(),
            target: it.target.clone(),
            cwd: it.cwd.clone(),
            host: it.host.clone(),
            since: it.since,
        });
    }
    out
}

/// What is left between the summary and the right-hand block. Two columns
/// minimum: a window with no room just trails the tail behind the summary
/// instead of overlapping it.
fn gap_of(width: usize, plabel: &str, sum: &str, tailw: usize) -> usize {
    let used = vlen(plabel) + 1 + 2 + vlen(sum) + tailw;
    width.saturating_sub(used).max(2)
}

/// How much width a summary may have before it starts costing the table.
///
/// `0` means "as much as it likes", which is what an unmeasured window (width 0,
/// the machine-readable path) and one too narrow to hold a table both get: in
/// the first nothing is being drawn, and in the second there is no arrangement
/// that fits, so a long summary is more use than a stump.
fn summary_room(width: usize, labelw: usize, tailw: usize) -> usize {
    if width == 0 {
        return 0;
    }
    let chrome = labelw + 1 + 2 + 2 + tailw; // label, space, marker, gap, table
    let room = width.saturating_sub(chrome);
    if room < MIN_SUMMARY {
        0
    } else {
        room
    }
}

/// Below this a summary says nothing, so the table gives way instead.
const MIN_SUMMARY: usize = 20;

/// Cut to a display width, with an ellipsis where it was cut.
///
/// `0` is no limit. Character-wise and width-aware, so a double-width character
/// counts for two and none is ever cut in half.
fn fit(s: &str, room: usize) -> String {
    if room == 0 || vlen(s) <= room {
        return s.to_string();
    }
    let mut out = String::new();
    let mut w = 0;
    for c in s.chars() {
        let cw = UnicodeWidthStr::width(c.to_string().as_str());
        if w + cw > room.saturating_sub(1) {
            break;
        }
        out.push(c);
        w += cw;
    }
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(lines: &str, cur: &str, width: usize) -> Vec<String> {
        build(
            lines,
            &Input {
                cur,
                width,
                home: "/home/p",
                ..Default::default()
            },
        )
        .iter()
        .map(|r| r.to_ansi())
        .collect()
    }

    const THREE: &str = "%10\twork:1.1\t/home/p/proj/web\tclaude\t2.1.229\trun\t-\t◐ Refactor auth\n\
                         %11\tops:2.1\t/home/p\tgemini\t0.41.2\trun\t-\t⠂ tests\n\
                         %12\tops:3.1\t/home/p/longdir/another-very-long-project-name-here\tcodex\t\trun\t-\t◐ X";

    #[test]
    fn the_pane_id_is_the_hidden_last_field() {
        let r = rows(THREE, "%11", 100);
        assert!(r[0].ends_with("\t%10"));
        assert!(r[1].ends_with("\t%11"));
    }

    #[test]
    fn only_the_current_pane_is_marked() {
        let r = rows(THREE, "%11", 100);
        assert!(!r[0].contains('●'));
        assert!(r[1].contains('●'));
    }

    /// The glyph the title leads with is dropped and the column filled from the
    /// state, so a title's own spinner frame never leaks into the summary.
    #[test]
    fn the_title_marker_is_replaced_by_the_state_marker() {
        let r = rows(THREE, "%11", 100);
        assert!(r[0].contains("Refactor auth"));
        assert!(!r[0].contains("◐ Refactor")); // the title's own glyph is gone
        assert!(r[0].contains("\x1b[2m◐\x1b[0m ")); // …and the state's is there
    }

    #[test]
    fn summary_of_strips_only_a_leading_glyph_run() {
        assert_eq!(summary_of("◐ Refactor auth"), "Refactor auth");
        assert_eq!(summary_of("⠂ tests"), "tests");
        assert_eq!(summary_of("plain title"), "plain title");
        // a title that merely OPENS on a non-ASCII word keeps it: the space has
        // to follow the run directly
        assert_eq!(summary_of("étude du code"), "étude du code");
        // a bare glyph with nothing after it leaves an empty summary
        assert_eq!(summary_of("✳"), "");
    }

    #[test]
    fn paths_fold_home_and_keep_the_last_two_components() {
        assert_eq!(path_display("/home/p/proj/web", "/home/p"), "proj/web");
        assert_eq!(path_display("/home/p", "/home/p"), "~");
        assert_eq!(path_display("/var/log", "/home/p"), "var/log");
    }

    #[test]
    fn a_long_path_is_elided_from_the_left_to_thirty() {
        let d = path_display(
            "/home/p/longdir/another-very-long-project-name-here",
            "/home/p",
        );
        assert_eq!(d.chars().count(), 30);
        assert!(d.starts_with('…'));
        assert!(d.ends_with("name-here"));
    }

    /// Every trailing column is measured over the whole list, so the path column
    /// starts in the same place on every row. Sizing them per row is what bent
    /// the list: no two agent names are the same length.
    /// Columns, not bytes: `●` and `◐` are three bytes each, so a byte offset
    /// reports two rows as misaligned that are in fact flush.
    fn col_of(line: &str, needle: &str) -> usize {
        let s = strip(line);
        let b = s.find(needle).expect("needle on the row");
        vlen(&s[..b])
    }

    #[test]
    fn the_trailing_columns_line_up_down_the_list() {
        let r = rows(THREE, "%11", 100);
        // all three paths start at the same column
        let a = col_of(&r[0], "proj/web");
        assert_eq!(col_of(&r[1], "~"), a);
        assert_eq!(col_of(&r[2], "…"), a);
    }

    fn strip(s: &str) -> String {
        let mut out = String::new();
        let mut it = s.chars();
        while let Some(c) = it.next() {
            if c == '\x1b' {
                for c in it.by_ref() {
                    if c == 'm' {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    /// A row with no version keeps the column rather than pulling the ones beside
    /// it out of line, and the blank remainder that leaves is trimmed.
    #[test]
    fn a_missing_version_keeps_its_column_but_leaves_no_trailing_space() {
        let r = rows(THREE, "%11", 100);
        assert!(!strip(&r[2]).split('\t').next().unwrap().ends_with(' '));
        assert!(strip(&r[2]).contains("codex"));
    }

    #[test]
    fn abbreviates_to_the_shortest_prefix_that_still_tells_names_apart() {
        let n: Vec<String> = ["main", "master", "ops"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let a = abbrev(&n, 1);
        assert_eq!(a["ops"], "o");
        assert_eq!(a["main"], "mai");
        assert_eq!(a["master"], "mas");
    }

    /// A name that is a whole other name plus something can only be told apart in
    /// full, which the awk gets by running its loop off the end.
    #[test]
    fn a_name_that_contains_another_is_kept_whole() {
        let n: Vec<String> = ["main", "main2"].iter().map(|s| s.to_string()).collect();
        let a = abbrev(&n, 1);
        assert_eq!(a["main"], "main");
        assert_eq!(a["main2"], "main2");
    }

    #[test]
    fn the_host_floor_is_two_letters() {
        let n: Vec<String> = ["laptop-two", "ha"].iter().map(|s| s.to_string()).collect();
        let a = abbrev(&n, 2);
        assert_eq!(a["laptop-two"], "la");
        assert_eq!(a["ha"], "ha");
    }

    /// The floor holds up a column of pane labels and nothing else. A list of
    /// ended sessions labels no pane, so it would spend twelve columns of summary
    /// on nothing.
    #[test]
    fn the_label_floor_applies_to_pane_rows_and_the_cap_to_narrow_windows() {
        let short = "%1\tw:1.1\t/home/p\tclaude\t1.0\tidle\t-\thi";
        let wide = rows(short, "", 200);
        let label_end = strip(&wide[0]).find("hi").unwrap();
        assert_eq!(label_end, 15 + 1 + 2); // floored at 15, a space, the marker

        // narrow caps rather than floors, so the summary gets the width back
        let narrow = rows(short, "", 60);
        assert!(strip(&narrow[0]).find("hi").unwrap() < 15);
    }

    #[test]
    fn a_row_with_no_room_still_leaves_two_columns_of_gap() {
        let r = rows(THREE, "%11", 0);
        for line in &r {
            assert!(strip(line).contains("  "));
        }
    }

    #[test]
    fn trims_only_a_trailing_run_of_unpainted_spaces() {
        let mut c = vec![cell("a", PLAIN), cell("b  ", PLAIN)];
        trim_trailing(&mut c);
        assert_eq!(c, vec![cell("a", PLAIN), cell("b", PLAIN)]);

        // the run crosses a cell boundary, exactly as it would in the awk's
        // concatenated string
        let mut c = vec![cell("a", PLAIN), cell("  ", PLAIN), cell("   ", PLAIN)];
        trim_trailing(&mut c);
        assert_eq!(c, vec![cell("a", PLAIN)]);

        // a painted cell ends in a reset, so nothing is trimmed past it
        let mut c = vec![cell("x  ", PATH), cell("", PLAIN)];
        trim_trailing(&mut c);
        assert_eq!(c[0].text, "x  ");
    }

    #[test]
    fn the_permission_mode_rides_on_the_agent_name() {
        let base = "%1\tw:1.1\t/home/p\tclaude\t1.0\tidle\t";
        let ask = rows(&format!("{}default\thi", base), "", 100);
        let edit = rows(&format!("{}acceptEdits\thi", base), "", 100);
        let auto = rows(&format!("{}bypassPermissions\thi", base), "", 100);
        assert!(ask[0].contains("\x1b[35mclaude"));
        assert!(edit[0].contains("\x1b[95mclaude"));
        assert!(auto[0].contains("\x1b[1;95mclaude"));
    }

    #[test]
    fn a_stale_version_goes_yellow_only_where_ctrl_x_could_act() {
        let line = "%1\tw:1.1\t/home/p\tclaude\t1.0\tidle\t-\thi";
        let stale = build(
            line,
            &Input {
                newver: "2.0",
                width: 100,
                ..Default::default()
            },
        );
        assert!(stale[0].to_ansi().contains("\x1b[33m1.0"));

        // same version installed: nothing to act on
        let current = build(
            line,
            &Input {
                newver: "1.0",
                width: 100,
                ..Default::default()
            },
        );
        assert!(current[0].to_ansi().contains("\x1b[2;35m1.0"));

        // another host: newver is what THIS box would start, so it says nothing
        let remote = build(
            "ha:%1\tw:1.1\t/home/p\tclaude\t1.0\tidle\t-\thi",
            &Input {
                newver: "2.0",
                width: 100,
                ..Default::default()
            },
        );
        assert!(remote[0].to_ansi().contains("\x1b[2;35m1.0"));

        // an ended session has no process to put back
        let dead = build(
            "%1\tw:1.1\t/home/p\tclaude\t1.0\tdead\t-\thi",
            &Input {
                newver: "2.0",
                width: 100,
                ..Default::default()
            },
        );
        assert!(dead[0].to_ansi().contains("\x1b[2;35m1.0"));
    }

    #[test]
    fn a_host_that_could_not_answer_keeps_its_name_whole() {
        // the second field is the message, not a target, and the row is left out
        // of the shortening
        let r = build(
            "laptop-two:unreachable\tlaptop-two: no answer\t\t\t\tnote\t\t",
            &Input {
                width: 60,
                only: "note",
                ..Default::default()
            },
        );
        assert!(r[0].to_ansi().contains("laptop-two: no answer"));
    }

    /// The outdated list holds exactly the rows the version column paints
    /// yellow, which is what makes it the list of rows ctrl-x and F8 act on.
    /// Same predicate for both, so the two can never disagree.
    #[test]
    fn the_outdated_list_holds_exactly_the_rows_painted_yellow() {
        let lines = "%1\ta:1.1\t/home/p\tclaude\t2.1.229\tidle\t-\tbehind\n\
                     %2\tb:1.1\t/home/p\tclaude\t2.1.243\trun\t-\tcurrent\n\
                     %3\tc:1.1\t/home/p\tgemini\t0.41.2\tinput\t-\tanother agent\n\
                     ha:%4\td:1.1\t/home/p\tclaude\t2.1.229\tidle\t-\tover there";
        let r = build(
            lines,
            &Input {
                newver: "2.1.243",
                width: 100,
                outdated: true,
                ..Default::default()
            },
        );
        let ids: Vec<&str> = r.iter().map(|r| r.pane_id.as_str()).collect();
        assert_eq!(ids, ["%1"]);
        assert!(r[0].to_ansi().contains("\x1b[33m2.1.229"));

        // …and the same list with nothing installed to compare against is
        // empty rather than everything: the mode is skipped there.
        let none = build(
            lines,
            &Input {
                width: 100,
                outdated: true,
                ..Default::default()
            },
        );
        assert!(none.is_empty());
    }

    /// Being behind is not a state, so the list crosses all four of them: a
    /// session waiting for an answer is as behind as an idle one.
    #[test]
    fn the_outdated_list_is_not_one_state() {
        let lines = "%1\ta:1.1\t/home/p\tclaude\t1.0\tinput\t-\tasking\n\
                     %2\tb:1.1\t/home/p\tclaude\t1.0\trun\t-\tworking\n\
                     %3\tc:1.1\t/home/p\tclaude\t1.0\tidle\t-\tidle";
        let r = build(
            lines,
            &Input {
                newver: "2.0",
                width: 100,
                outdated: true,
                ..Default::default()
            },
        );
        assert_eq!(r.len(), 3);
    }

    #[test]
    fn one_state_only_when_asked() {
        let r = build(
            THREE,
            &Input {
                only: "run",
                width: 100,
                ..Default::default()
            },
        );
        assert_eq!(r.len(), 3);
        let r = build(
            THREE,
            &Input {
                only: "input",
                width: 100,
                ..Default::default()
            },
        );
        assert!(r.is_empty());
    }

    #[test]
    fn a_blank_pane_title_borrows_the_one_its_conversation_recorded() {
        let mut ptitles = HashMap::new();
        ptitles.insert("%1".to_string(), "what it called itself".to_string());
        let r = build(
            "%1\tw:1.1\t/home/p\tclaude\t1.0\tidle\t-\t",
            &Input {
                width: 100,
                ptitles,
                ..Default::default()
            },
        );
        assert!(r[0].plain().contains("what it called itself"));
    }

    /// The snippet takes the path column, but only on a row that does not already
    /// show what was typed: there is nothing to explain then, and the path is
    /// worth more.
    #[test]
    fn a_search_snippet_replaces_the_tail_unless_the_row_already_says_it() {
        let mut snips = HashMap::new();
        snips.insert("%10".to_string(), "…the words it said…".to_string());
        snips.insert("%11".to_string(), "…other words…".to_string());
        let r = build(
            THREE,
            &Input {
                cur: "%11",
                width: 100,
                home: "/home/p",
                query: "refactor",
                snips,
                ..Default::default()
            },
        );
        // %10's summary is "Refactor auth", so it already says it
        assert!(r[0].plain().contains("proj/web"));
        assert!(!r[0].plain().contains('⌕'));
        // %11's does not
        assert!(r[1].plain().contains("⌕ …other words…"));
    }

    /// Whichever side carries the capital. `build` folds the terms, and a row
    /// showing `Refactor` answers a query for `REFACTOR` as well as one for
    /// `refactor`.
    #[test]
    fn says_it_ignores_case_both_ways() {
        assert!(says_it("Refactor auth", &["refactor".to_string()]));
        assert!(says_it("refactor auth", &["refactor".to_string()]));
        assert!(!says_it("refactor auth", &["rewrite".to_string()]));
    }

    /// …and the row built from a shouted query keeps its path rather than being
    /// handed a snippet to explain a match it visibly already shows.
    #[test]
    fn a_shouted_query_still_counts_as_said_on_the_row() {
        let mut snips = HashMap::new();
        snips.insert("%10".to_string(), "…the words it said…".to_string());
        let r = build(
            THREE,
            &Input {
                width: 100,
                home: "/home/p",
                query: "REFACTOR",
                snips,
                ..Default::default()
            },
        );
        assert!(r[0].plain().contains("proj/web"));
        assert!(!r[0].plain().contains('⌕'));
    }

    /// When a session last said something rides in a ninth field, and a row
    /// without one, from an older host or an agent that cannot say, is read as
    /// not knowing rather than refused. Nothing about it is drawn.
    #[test]
    fn the_last_message_is_read_off_a_ninth_field_when_there_is_one() {
        let rows = "%1\tw:1.1\t/a\tclaude\t2.1\tidle\t-\tone\t1700000000000\n\
                    %2\tw:2.1\t/b\tcodex\t0.9\tidle\t-\ttwo\t-\n\
                    %3\tw:3.1\t/c\tclaude\t2.1\tidle\t-\tthree\n";
        let r = build(rows, &Input::default());
        let since: Vec<Option<i64>> = r.iter().map(|r| r.since).collect();
        assert_eq!(since, [Some(1_700_000_000_000), None, None]);
        let eight = build(&taimux_core::panes::wire(rows, false), &Input::default());
        assert_eq!(
            r.iter().map(Row::plain).collect::<Vec<_>>(),
            eight.iter().map(Row::plain).collect::<Vec<_>>(),
            "the field changes nothing on screen"
        );
    }
}
