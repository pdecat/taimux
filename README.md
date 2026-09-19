# tAImux

*t**AI**mux, said "thai-mux": the **AI** sessions in your **tmux**. Written
`taimux` everywhere you type it.*

List, select and **jump** between live AI coding-agent sessions running across
your tmux panes, windows and sessions, from a single picker, bound to
`prefix + a` and to `F1`.

**taimux runs on the tmux you already have.** It is not a multiplexer and does
not want to become one: your config, your plugins and your bindings are
untouched, and removing it leaves nothing behind. See
[Prior art](#prior-art) for what to use instead if you want the opposite.

![taimux: the picker listing agent sessions across local panes and a remote
host, filtering them by typing, and cycling with Tab through the sessions that
are waiting, working, running outdated code, and have already ended](demo/demo.gif)

*Recorded from [`demo/demo.sh`](demo/demo.sh), which builds an isolated tmux
server full of synthetic sessions: every project, task and agent above is
fabricated. Run it yourself with `just demo`.*

```
  work:1.1     ✳ Refactor auth middleware      webapp/api        claude 2.1.229
  work:2.1     ◐ Write tests for the parser    webapp/frontend    gemini 0.41.2
  work:3.1       Add pagination to results     services/search     codex 0.20.3
● ops:1.1      ◐ Fix the flaky CI pipeline     infra/terraform  opencode 1.18.10
  ops:2.1      ✳ Update README and changelog   docs/site              pi 0.9.4
  ops:3.1        deploy: cut the release       infra/deploy       claude 2.1.235
```

Each row is a tmux pane whose **foreground process is a coding agent**, labelled
with its `session:window.pane`, then what that session is doing and the task
summary it publishes as its pane title (or, for a pane that publishes none yet,
[the title its conversation last recorded](#past-sessions)), and pinned to the right edge the working
directory (last two components), the agent and the version it is running. The
summary comes first because it is what you read the list for; everything that only
says *where* the session is sits behind it, costing it no width, aligned in columns
of its own, and is the first thing a row too long for the window truncates away.

A pane that is an `ssh` into another box's tmux brings **that** server's sessions
into the same list, labelled by host, with no configuration at all: see
[Sessions on other hosts](#sessions-on-other-hosts).

Everything above is about *finding* a session. For Claude Code specifically,
taimux also puts one **back**: `restart` relaunches the panes a self-update has
left behind, on the conversation each was already having, and `resurrect` makes a
tmux-resurrect restore bring those conversations back rather than opening new
ones. Same pane scan, same reading of what each session is doing. See
[Putting a Claude Code session back](#putting-a-claude-code-session-back).

## Contents

- [What a session is doing](#what-a-session-is-doing)
- [Told, rather than guessed](#told-rather-than-guessed)
- [Narrow windows](#narrow-windows)
- [Supported agents](#supported-agents)
- [Agent version](#agent-version)
- [Requirements](#requirements)
- [Install](#install)
  - [From a release, with no toolchain](#from-a-release-with-no-toolchain)
  - [From source](#from-source)
  - [As a tmux plugin (tpm / tpack)](#as-a-tmux-plugin-tpm--tpack)
  - [Oh My Tmux](#oh-my-tmux)
- [Usage](#usage)
- [A terminal that changes size](#a-terminal-that-changes-size)
- [Searching what a session said](#searching-what-a-session-said)
- [Sessions running outdated code](#sessions-running-outdated-code)
- [Past sessions](#past-sessions)
- [Carrying a conversation into another agent](#carrying-a-conversation-into-another-agent)
- [Sessions on other hosts](#sessions-on-other-hosts)
- [Putting a Claude Code session back](#putting-a-claude-code-session-back)
  - [`taimux restart`](#taimux-restart)
  - [`taimux resurrect`](#taimux-resurrect)
  - [`taimux print-cmds`](#taimux-print-cmds)
- [How detection works](#how-detection-works)
- [Demo](#demo)
- [Notes](#notes)
- [The daemon](#the-daemon)
- [Not a shell script](#not-a-shell-script)
- [The picker](#the-picker)
- [Development](#development)
- [History](#history)
- [Prior art](#prior-art)
- [Uninstall](#uninstall)
- [License](#license)

## What a session is doing

The column before the summary answers it, in three states:

| | |
|---|---|
| `✳` | **waiting for an answer**, the one worth looking for (and the only one in colour) |
| `◐` | working: a turn is in flight |
| blank | idle at the prompt, nothing to do |

The column is two wide whichever it is, so every summary starts in the same place.

That marker is taimux's own reading rather than anything the agent publishes,
because the pane title cannot carry it. Claude shows the same `✳` there whether
it is sitting idle, running a tool or holding a permission dialog, and shows it
**mid-turn** as well: a session was caught with a plain `✳ <project>: <title>`
title while its screen read `Twisting… (35s · ↓ 1.6k tokens)`. Its long
`<project>: <title> ⑂ <last prompt>` form leads with nothing at all. Whatever
glyph a title does carry is therefore stripped, not shown.

So the pane's own **screen** decides both, off a single capture per pane (about
2 ms each):

- **waiting**: a dialog draws a numbered choice list, and the lowest prompt line
  on screen is the one that owns it, with the footer read over the last few lines
  as well (on a narrow pane the list wraps and pushes itself off the bottom,
  leaving the footer as the only sign). Those lines are read **joined**, because
  narrower still the footer wraps too, and `Esc to` on one line with `cancel` on
  the next is the same footer;
- **working**: the turn line Claude keeps above the prompt box. There is one per
  turn and it is rewritten in place, from `✽ Twisting… (35s · ↓ 1.6k tokens)`
  while the turn runs to `✻ Crunched for 9m 55s · done 11:07 AM` once it ends, so
  the **lowest** one on screen belongs to the most recent turn and says whether
  that turn is still going. It is read over the last sixteen lines of content,
  because it is nowhere near the bottom of the screen: Claude tucks a tip row and
  a token count under it, and then come the title rule, the prompt box, its own
  rule and two status rows. Measured across 39 live panes it sat 2 to 9 lines up.

A dialog wins over a turn line, since it is the row that wants you.

**Tab** cycles the list through those states, one at a time and back, with two
further stops that are not states at all. One asks a different question of the
same list, what each session is *running* rather than what it is doing (see
[Sessions running outdated code](#sessions-running-outdated-code)); the other is
a different list entirely, the conversations nothing is running any more (see
[Past sessions](#past-sessions)).

```
all sessions  →  waiting  →  working  →  idle  →  outdated  →  past sessions  →  all sessions
```

The border label names the list you are looking at, and the mode is kept across a
refresh (by hand or on the timer), so a picker left open on the waiting list is a
live list of the sessions that want an answer, one that empties itself as you deal
with them.

Every Tab puts the cursor back on **the session you are in**, whenever the mode it
lands on still lists it, so cycling round never loses your place even though the
modes in between may list nothing at all. It is left alone in the two cases where
moving it would be a guess: when the session is not in that state, and while you
have a query typed, since the row numbers then count what matches rather than what
the list was given.

## Told, rather than guessed

Reading a pane from the outside has limits, and two of them matter:

- **the permission mode** is on the screen, one line under the prompt box, but a
  dialog or a redraw hides it, and a dialog is up exactly when a session is asking
  you something. Measured on 22 real panes: readable on 20;
- **working** is inferred from the *shape* of the turn line, which is the most
  fragile thing in this tool and has broken twice now.

So a session can report itself instead, through its own hooks:

```sh
taimux install-hooks      # registers `taimux hook` in ~/.claude/settings.json
```

That registers one command for seven events. Five are turn boundaries:
`SessionStart`, `UserPromptSubmit` (a turn started), `Stop` (it ended),
`PermissionRequest` (about to ask you) and `SessionEnd`. Two more, `PostToolUse`
and `PostToolUseFailure`, say a tool just ran and so the turn is still going.
Those two fire per tool call rather than per turn, which the bash version could
not have afforded and this one can: 685 µs an invocation, so a thirty-call turn
spends 20 ms of CPU over the minutes it takes. Each writes one line under
`$XDG_RUNTIME_DIR/taimux/`, keyed by the pane:

```
<agent pid>   <state>   <permission mode>
```

The pane comes from `$TMUX_PANE`, which every agent started in a tmux pane carries
in its environment and hands to the hooks it spawns. The **pid** is what makes the
line worth reading: it names the process the line is about, so a line left by an
earlier session in that pane, or one written by a nested `claude -p` that
inherited `$TMUX_PANE` from the session that launched it, does not match the
pane's live agent and is ignored. A nested session cannot write over the line, nor
delete it on its way out.

A tool call is also the only **repair** the line has. A turn whose opening
`UserPromptSubmit` never reached the hook leaves the line reading whatever the
previous turn closed with, and nothing else in the turn would touch it. Its first
tool call puts it back to `run`. Found in the wild on a pane prompted twice one
morning and two minutes into a turn, whose line had not moved in two days.

A dialog **on screen** still outranks the line, because that is the state which
must never be wrong: granting a permission fires no event of its own, so the line
reads `input` until the tool actually runs and `PostToolUse` lands. Three more
readings overrule it, the same argument in all three directions: an idle prompt
box under a line reading `input`, a turn line with a live counter under one
reading `idle`, and a **finished** turn line with an idle prompt box under it and
no later turn line below, under one reading `run`. Each time the screen says
positively what the line has stopped saying, and a line that stopped being written
is what a missed event leaves behind. That last one is the worst of the three
while it lasts, because nothing ever clears it: the pane sits in the working list
for good and `restart` refuses it. Found on a session Claude had put in the
background (`sessionKind: "bg"`), whose own transcript recorded `taimux hook`
running on every `Stop` with no error while the line it should have written never
appeared, six and a half hours of it.

The **absence** of a turn line is deliberately not read as an answer, and that is
what keeps the last rule honest: a session part way through a long reply can show
no turn line at all, and then `run` is the only thing that knows. Everything else
the line says is taken as it stands, which is why it is written at all: `run` and
`input` tell the list what a screen often cannot show.

The permission mode has nowhere else to come from, and it is shown by how brightly
the agent name is painted, so it costs the summary no width:

| the agent name | the session |
|---|---|
| magenta | will stop and ask you: manual or plan mode, or no hook installed |
| **bright** | accepts edits on its own, and still asks for the rest |
| **bold bright** | asks nothing at all: auto, or bypassed permissions |

Two things to expect: a mode flipped mid-turn with `shift+tab` shows up at the end
of that turn (no event announces the flip, and scraping the footer is what this
exists to stop doing), and sessions already running when the hook is installed
report nothing until they restart. None of it is required: with no hook installed
every row still comes off `ps` and the screen, exactly as before. Claude Code is
the only agent wired up so far.

## Narrow windows

On a narrow window (under 100 columns, the same threshold at which the binding
switches the popup to full width) session names are cut to the fewest letters
that still tell them apart: a single one where nothing else in the list starts
with it, three for `main` next to `master`, the whole thing for `main` next to
`main2`. `window.pane` is never touched, and the label column shrinks with the
names, so the summary gets the width back.

**Resizing the terminal re-lays the list out.** Each row is built with its
trailing columns right-aligned by a measured gap, so the padding belongs to one
particular width. Left alone, shrinking the window would leave every row too long
and truncate from the right, cutting off exactly the directory, agent and version
that were pinned to the edge, while growing it would strand that tail in the
middle of the row. Crossing the 100-column threshold changes the shape again,
since that is where session names start being shortened. So a resize rebuilds the
rows at the new width, keeping whatever state `Tab` is filtering on and your place
in the list, by session rather than by row number.

The pane you're in is marked `●` and is selected by
default when the picker opens. A live preview of the highlighted pane is shown
below the list. Hit **Enter** to
jump straight to that pane, across windows and sessions, and it's automatically
zoomed to fill its window. The list refreshes itself while the picker is open (a
beat after you pause, so it never interrupts your navigation), so sessions that
start or finish appear and disappear on their own (the border reads `· live`).

## Supported agents

`claude` · `codex` · `opencode` · `gemini` · `antigravity` · `agy` · `pi`

Native binaries (`claude`, `codex`, `opencode`, `agy`) are matched by name.
Node-wrapped CLIs whose process shows up as `node` (e.g. `gemini`) are matched on
the foreground process's **full argv**, so they're detected too. To add or change
agents, edit the `agent_of()` patterns near the top of `taimux`.

`claude` has a second shape, because Claude Code self-updates by writing a whole
new binary to `~/.local/share/claude/versions/<version>` and repointing the
launcher symlink. A session started from one of those files directly has no
`claude` word in its argv at all: the version *is* the filename, and `comm` reads
`2.1.239`. That install layout is matched as well, and the headless exclusion
still applies to it. Anchoring on the layout rather than on a bare version number
is deliberate, since the latter would match nearly any argv.

## Agent version

The version at the right of a row is the version that pane is **actually running**,
not the one `<agent> --version` prints today. Agents update themselves in place,
so a long-lived session routinely sits a release or two behind the binary that
name now resolves to, and seeing which sessions those are is the whole point of
showing it. It is read off the live process, cheapest source first:

1. the running binary's own path, which carries the version both for the
   self-updating native installers (`~/.local/share/claude/versions/2.1.229`) and
   for version-managed installs (`…/mise/installs/opencode/1.18.10/opencode`);
2. the `package.json` above the entry point, for Node-wrapped CLIs like `gemini`
   whose running binary is `node`;
3. failing both, `--version` on that same binary (the file the pane is running,
   never whatever `$PATH` points at now), cached under `$XDG_CACHE_HOME/taimux`
   and keyed by the binary's path and mtime, since the picker rebuilds the list
   every few seconds.

When none of them can tell, nothing is shown rather than a version the pane may
not be running. Sources 1 and 3 read `/proc`, so on a system without it only
source 2 can answer. `TAIMUX_VERSIONS=0` turns the whole thing off, and with it
the [outdated](#sessions-running-outdated-code) stop, which has nothing left to
compare against.

## Requirements

- `tmux` (≥ 3.2 for `display-popup`; tested on 3.6)
- **Rust stable, but only to build from source.** A release needs none: it
  carries one statically linked artefact that runs on any x86_64 Linux with no
  glibc version to match, which is what lets one file serve every machine you
  put it on
- `curl`, only to install from a release
- `bash`, for the tmux plugin entry point, the test harness and the demo. Nothing
  taimux does at runtime goes through a shell
- `/proc` (Linux) for the version column, and for the hook to tell which agent
  it is running under; without it versions are mostly blank and `install-hooks`
  has nothing to key on
- `jq`, for `install-hooks`: `settings.json` is a file the agent rewrites for
  itself, and hand-rolled JSON edits over someone's real configuration is how a
  config gets mangled. `restart` uses it too, but only to *name* the background
  agents in its report, and skips that if it is missing. Nothing else needs it:
  the transcripts are read with `awk`, since the fields wanted are flat strings
- `tac`, `find` and `date`, for `restart` / `resurrect` / `print-cmds` only
- `ssh`, only for [sessions on other hosts](#sessions-on-other-hosts), plus a
  `taimux` on each of those hosts. With no ssh panes open none of it runs
- **nothing at all** for [past sessions](#past-sessions): every agent's history is
  read by taimux itself, including the two that keep theirs in SQLite. There is
  no `sqlite3` to install and no agent that has to be present for the others to
  be listed

## Install

### From a release, with no toolchain

```sh
./scripts/install-release.sh
taimux install
```

The first line downloads the statically linked x86_64 binary attached to the
latest release, installs it as `~/.local/bin/taimux`, and points
`~/.local/bin/taimux` at it. It runs the binary before installing it and
refuses one that does not execute here or whose version disagrees with the
release tag, because a picker that silently does nothing is a poor way to find
out. Already on that version, it downloads nothing and says so, which makes it
safe to call from a configuration-management run; `--force` re-downloads and
`--tag vX.Y.Z` takes a specific release.

Releases are cut by [release-please](https://github.com/googleapis/release-please)
from the commit messages, so the version in `taimux version` is the release tag.

### From source

```sh
just build
just install
```

Either way, `install`:

- symlinks `taimux` into `~/.local/bin`, and
- binds **`prefix + a`** and **`F1`** (no prefix) to a centered popup that runs
  the bare `taimux` launcher (no checkout path baked into your tmux config),
  live in the running server and persisted to your tmux config.

`taimux install-hooks` is separate and optional: `install` touches your tmux
config and is what the picker needs, that one touches an agent's own settings and
only makes the rows better informed (see [Told, rather than
guessed](#told-rather-than-guessed)).

### As a tmux plugin (tpm / tpack)

The other way in, if you already manage plugins with
[tpm](https://github.com/tmux-plugins/tpm) or
[tpack](https://github.com/tmuxpack/tpack). Add it to your tmux config and press
**`prefix + I`**:

```tmux
set -g @plugin 'pdecat/taimux'
```

The manager clones the checkout and runs `taimux.tmux` from it on every tmux
launch and reload, which binds the same two keys in the running server, calling
the picker by **that checkout's own absolute path** so nothing has to be on
`PATH`. Nothing is written to your tmux config, and removing the plugin removes
all of it.

Two options, resolved at bind time, so they belong in the same config that loads
the plugin (set them before the manager's `run` line). An option set to the empty
string means "don't bind that one":

```tmux
set -g @taimux-key      'a'     # prefix binding
set -g @taimux-root-key 'F1'    # no prefix, easy to send from a phone
```

Changing a key binds the new one. Like any tmux plugin it knows nothing about
what it bound last time, so the old key keeps working until the next fresh
server.

**A plugin can bind a key, and that is all it can do.** The rest of taimux is a
CLI: `restart`, `resurrect`, `print-cmds`, `install-hooks`, and the `list` that
[another host's picker](#sessions-on-other-hosts) asks for over ssh. Run
`taimux install` from the plugin checkout to get those: from a directory a
plugin manager owns it symlinks `~/.local/bin/taimux` and leaves your tmux
config alone, since the bindings are already the plugin's job.

A host that has taimux *only* as a plugin is still listed by another host's
picker: with no symlink to find, the remote lookup falls back to
`~/.tmux/plugins/taimux*/taimux` (and the XDG path), a real install still
winning where there is one.

`taimux bind` is that same bind-in-the-running-server step on demand, and the
repair for a binding that outlived its checkout: tpack 2.x installs a plugin into
`<name>-<hash>`, so a re-install moves the directory the old binding names, and a
long-running server goes on calling a path that is gone (the key answers
`returned 127`). Run it from the new checkout and the live server is fixed in
place, with no restart and no reload.

### Oh My Tmux

Install writes the bindings to **`~/.tmux.conf.local`** when that file exists
(the override file used by [Oh My Tmux](https://github.com/gpakosz/.tmux) and
similar setups), inserting them under its `-- user customizations --` section.
Otherwise it falls back to `~/.tmux.conf`.

Why: on Oh My Tmux, `~/.tmux.conf` is a dual-purpose file that tmux also runs as
a shell program via `cut -c3- | sh`; appending raw `bind-key` lines to it breaks
every reload with `Syntax error: "(" unexpected`. The `-- user customizations --`
section of `~/.tmux.conf.local` lives inside that file's heredoc and is only
`tmux source`d, so it's safe. As a safety net, if `~/.tmux.conf.local` is absent
but the main config is detected as Oh My Tmux (resolves under `~/.tmux/`, or has
an `_apply_configuration` marker), install creates `~/.tmux.conf.local` rather
than touching the dual-purpose file. Re-running `install` is idempotent.

To add it by hand instead, put this under `-- user customizations --` in
`~/.tmux.conf.local` (Oh My Tmux) or anywhere in `~/.tmux.conf` (plain tmux). The
`if-shell` makes the popup near-full-width on small screens (e.g. a phone over
SSH) and 80% on a roomy terminal. tmux's `#{<:}` is a string compare, so the
numeric width test is done in the shell:

```tmux
bind-key a   if-shell '[ "#{client_width}" -lt 100 ]' 'display-popup -E -e TAIMUX_POPUP=1 -w 100% -h 90% "taimux pick #{pane_id}"' 'display-popup -E -e TAIMUX_POPUP=1 -w 80% -h 80% "taimux pick #{pane_id}"'
bind-key -n F1 if-shell '[ "#{client_width}" -lt 100 ]' 'display-popup -E -e TAIMUX_POPUP=1 -w 100% -h 90% "taimux pick #{pane_id}"' 'display-popup -E -e TAIMUX_POPUP=1 -w 80% -h 80% "taimux pick #{pane_id}"'
```

`-e TAIMUX_POPUP=1` is how the picker knows it is in a popup rather than inline
in a pane, which is what lets it [follow a terminal that
grows](#a-terminal-that-changes-size). A binding without it still works; it just
never resizes itself. `taimux install` writes the current form, so re-running it
is how an older binding catches up.

## Usage

- **`prefix + a`**: open the picker in a tmux popup. The popup is
  near-full-width on small screens (< 100 cols) and 80% on larger ones.
- **`F1`**: same, but with no prefix, handy from phone SSH clients like
  ConnectBot where the `C-a` chord is awkward. (Root binding, so `F1` is
  intercepted by tmux instead of reaching the focused program.)
- **`taimux`**: open the picker inline in the current pane.

Inside the picker:

| key            | action                          |
|----------------|---------------------------------|
| type           | filter on what each row shows |
| `Ctrl-t`       | …and on [what was said inside each session](#searching-what-a-session-said) |
| `↑` / `↓`      | move (wraps around at the ends) |
| `Ctrl-j` / `Ctrl-k` | same, and `Ctrl-n` / `Ctrl-p` too |
| `PgUp` / `PgDn`| move a screenful (stops at the ends, it does not wrap) |
| `Home` / `End` | jump to the first / last row    |
| wheel          | move, one row per notch (wraps, like the arrows) |
| click          | put the cursor on that row       |
| double-click   | …and switch to it, as `Enter` would |
| `Enter`        | switch to the pane (and zoom it), or [reopen a past session in its own tool](#past-sessions) |
| `Tab`          | cycle the list: all → waiting → working → idle → [outdated](#sessions-running-outdated-code) → [past](#past-sessions) → all |
| `Ctrl-r`       | refresh the list now            |
| `Ctrl-/`       | toggle the preview              |
| `Ctrl-x`       | restart the highlighted claude session onto the installed version |
| `Ctrl-o`       | [carry this conversation into a different agent](#carrying-a-conversation-into-another-agent) |
| `F8`           | restart every [outdated](#sessions-running-outdated-code) claude session, after showing the plan and asking |
| `Ctrl-w`       | delete the last word of the query (`Alt-Backspace` too) |
| `Ctrl-h`       | delete a character, as `Backspace` does |
| `Ctrl-u`       | clear the query                 |
| `Ctrl-l`       | repaint, for a screen something else wrote over |
| `Shift-↑` / `Shift-↓` | scroll the preview |
| `Esc`          | cancel (`Ctrl-c`, `Ctrl-g` and `Ctrl-q` too) |

`Ctrl-x` and `F8` are the picker's front end to [`taimux restart`](#taimux-restart),
and a **yellow version** on a row is what says one applies: that session is
running code a self-update has already replaced. `Tab` gathers those rows into a
[list of their own](#sessions-running-outdated-code), so they can be dealt with
without hunting for the yellow. They inherit every refusal the
command makes on its own, so a pane that is working, holding a permission dialog,
already current, running another agent, or whose conversation cannot be
identified is left alone. A mistyped chord is a no-op, not a lost turn.

`Ctrl-x` restarts the highlighted row straight away when it can. When it **can't**,
it says why and offers to force it, because a silently declined keypress is
indistinguishable from a key that does nothing. Forcing is `--include-busy`: it
accepts losing an in-flight turn, and it still cannot skip the dialog check, so a
pane holding a permission prompt is refused either way and nothing is ever
answered on your behalf. Where forcing would not help at all (an unresolved
conversation, a transcript already claimed by another pane) it is not offered. The
offer takes `Y` as its default too, on the same terms as `F8` below.

That escalation exists because of a failure mode with no other way out. A line
that stops being rewritten at `run` or `input` folds into "working" either way:
the pane then reads as working forever and a plain `Ctrl-x` declines it forever.
Found in the wild on a line **38 hours stale**, against a session that was plainly
idle (0.8% CPU, 35 minutes of CPU across nearly three days), in a pane too small to
render its prompt box and so beyond rescue by reading the screen.

That one was a granted permission, which `PostToolUse` closes now. What has no
closing event at all is an **interrupted** turn: `Esc` fires nothing, checked with
all thirteen of Claude Code's hook events subscribed at once, so the line stays at
`run` until some later turn in that pane completes. The screen cannot settle it
either, since a session streaming a long answer shows the same prompt box an
abandoned turn does.

**`F8` asks first**, because a sweep touches panes you
are not looking at and is not a decision you can take back one row at a time: it
draws the plan over the picker, the panes it would restart and a count of the ones
it would leave alone. `Y` is the default, so `Enter` goes ahead with the plan you
just pressed `F8` to see. Anything that is not `Enter` or `y` cancels, which is
stricter than `[Y/n]` usually reads: a cursor key reaches that prompt as an escape
sequence, and "anything but `n`" would let a stray one start a sweep.

The restart itself runs **detached** either way. It waits up to 12s for a session
to exit and then polls for it to come back, and the picker hands a command the
terminal for as long as it runs, so doing that inline would freeze the popup for
half a minute. Instead the version column stops being yellow on the next
auto-refresh, which is the real progress indicator. What happened is logged to
`$XDG_RUNTIME_DIR/taimux/restart.log`, since a detached job has nowhere to
print. Both keys are absent unless the picker was launched by the script, since
it is the script that carries them out.

While either key runs, the command it started **owns the terminal**, and it is
handed `/dev/tty` rather than the picker's stdout: the picker writes exactly one
thing to stdout, the pane you chose, and a child inheriting it put its whole
screen there in front of that. The picker also clears what the child drew before
taking the screen back, since that frame otherwise sits on the normal screen
under the picker and gets revealed again the moment you pick a row, which reads
as the sweep having run a second time.

**Why a function key and not `Alt-x`.** Alt+X reaches a terminal as `ESC`
followed by `x`, and that only reads as `alt-x` when both bytes arrive in the same
read. A phone keyboard with no Alt key sends them as two separate presses seconds
apart, which is a bare `ESC` (cancel) followed by a bare `x`. Measured on fzf at
the time: `ESC` then `x` exited with 130, while `ESC`+`x` in one write fired the
binding. Nothing about that is fzf's fault and nothing about it changed with the
picker, since it is the terminal doing the sending. A function key is the one
thing that survives every client, `F1` being already taken by the binding that
opens the picker.

`Home` and `End` are bound to the ends of the **list**. A text-editing default
would point them at the ends of the query, which in a picker you rarely type into
is a key that visibly does nothing.

`PgUp` and `PgDn` move by the height the list is actually drawn at, so they
follow the popup's size and whether the preview is open. They **stop** at the
ends where the arrows wrap, because holding one to reach the bottom of a long
list should not sail past the end and land back at the top with nothing on the
row to say so.

`Shift-↑` and `Shift-↓` scroll the **preview**, which is a window onto
something longer: a live pane's whole screen, or the last turns of a past
conversation. A pane's preview opens on the **bottom** of its screen, since what
a session is doing is the last thing on it, so `Shift-↑` is how you see what came
before; a past conversation opens at the top and reads forwards. It stops at
both ends, and the offset resets when the cursor moves, because an offset
measured against one session's screen means nothing on the next.

The **mouse** is on by default. The wheel is the arrow keys (one row per notch,
wrapping with them), a click moves the cursor, and a double-click within 400 ms
accepts the row the way `Enter` does. A click outside the list, above it or
below the last row, does nothing rather than guessing. Set `TAIMUX_MOUSE=0`
where capture costs more than it gives: while it is on, the terminal hands drag
selection and tmux's own copy mode to the picker instead.

On selection the target pane is **zoomed** to fill its window. Set
`TAIMUX_ZOOM=0` (in the environment, or the keybinding) to disable that.

Even the **first** scan runs off the loop, so a popup is never blank: it reads
`Looking for agent sessions…` until the rows arrive, and takes keys throughout.
That one was also reported as a stuck picker, an empty box over a session, from
back when the first scan was the one thing the picker still waited on.

With **nothing to list**, the picker says so rather than closing again: a machine
with no agent sessions running, a `Tab` filter nothing is in, a query nobody
matches and an empty past-sessions list each read differently, and each says which key
gets you out. It used to close itself on an empty list, which was reported as
"F1 no longer works" by someone whose machine simply had nothing running: an
empty popup that opens and shuts is indistinguishable from an unbound key, a
missing binary, or a popup that failed to start.

The list **auto-refreshes** so sessions that start or exit show up on their own;
the cursor stays pinned to the same session across refreshes, and so does the
state `Tab` last filtered on. Tune it with `TAIMUX_REFRESH` (timer interval in
seconds, fractional OK, default `3`; `0` disables and leaves just `Ctrl-r`).
There is no idle gate: fzf needed one because a reload blocked its input
loop and swallowed arrow keys mid-navigation, and a tick in the picker's own loop
is just a redraw. A typed query and the cursor both survive one.

A refresh runs **off the input loop**, on a thread of its own, so the picker
keeps drawing and keeps taking keys while one is out and the rows it already has
stay on screen until the new ones land. One at a time, since a timer that stacked
refreshes would do it on exactly the machine where they are slow. Where one takes
longer than a second the border says `· refreshing`, and where it takes longer
than `TAIMUX_SLOW_REFRESH` (2s) it writes itself down in
`$XDG_RUNTIME_DIR/taimux/restart.log` with what the time went on: how many pane
captures, version probes and ssh calls it made, and how long each kind took.

That is not a hypothetical. The scan used to run **on** the loop, and after an
`F8` sweep restarted 30 sessions the picker sat on the sweep's last screen for
the whole 85 seconds the restarts took: no redraw, no key, and nothing on screen
to say it was alive. Every part of it measured in milliseconds afterwards, which
is why the log now carries the breakdown as well as the fix.

The **bottom border** carries two things: how many rows the query kept out of how
many there are, on the left, and on the right **which taimux drew the list**.

```
┌ agent sessions · live ──────────────────────────────────────────────────────┐
│ …                                                                           │
└ 5/7 ───────────────────────────────────────────────────────── taimux 0.4.0 ┘
```

That stamp is the same string `taimux version` prints. It is worth a permanent
corner because nothing else on screen answers it: the picker is a popup a tmux
binding launches, there is one binary per host, and an update swaps the launcher
under a running tmux server without touching a single pane, so the same keypress
can draw a different version tomorrow. It is dim, and on a popup too narrow for
both it is the half that **gives way**, since the count is the one you read
constantly.

## A terminal that changes size

**Smaller is tmux's job and it does it**: a popup is clamped to fit a client that
shrank, and grown back up to the size it was asked for when the client returns.
The picker sees that as an ordinary resize event and re-lays its rows at the new
width, so rotating a phone to portrait and back needs nothing from taimux.

**Bigger than the popup was ever asked for is nobody's job**, and that is the
gap this fills. A picker opened in portrait was asked for 100% of 80 columns; in
landscape it is still 80 columns wide, on a 140-column screen it was entitled to
80% of. tmux has no command that resizes a popup in place, so the picker does the
only thing that can work: it closes and asks for a new one at the geometry the
binding would choose now, carrying its query, its `Tab` filter, its search and
preview toggles and the row the cursor was on. What you see is a flicker and the
same list, wider.

It only ever does this in a popup that told it so with `-e TAIMUX_POPUP=1` (see
the binding above), because a picker running inline in a pane needs none of it:
tmux resizes a pane with the client already. `TAIMUX_RESIZE=0` turns it off and
leaves the popup at whatever size it opened with.

**And only when tmux can name the client without guessing**, which means exactly
one attached to the session the popup belongs to. `#{client_width}` with no
target answers for whichever client tmux considers current, and with two
attached, say a 213-column desktop and a 46-column phone, a picker opened on the
phone measured its 44-column popup against the desktop, decided it had been
outgrown, closed itself and reopened over there, on top of an unrelated session.
Nothing exposes which client owns a popup (there is no format for it, and
`display-popup -e` does not expand formats, so the binding cannot pass it
either), so with more than one client the picker leaves its size alone.

The reopen goes through `tmux run-shell -b`, which is tmux running the command
itself once the old popup is gone. A detached child of the picker was tried first
and does not work: tmux accepts the request, reports success, and then discards
the popup as it tears down the one the picker was in. Should the reopen fail
anyway, it says so in `$XDG_RUNTIME_DIR/taimux/restart.log` rather than leaving
you wondering where the picker went.

## Searching what a session said

Everything on a row answers **where** a session is: the host, the tmux target, the
directory, the agent, and whatever its title makes of the task. That is the wrong
half of what you actually remember. What sends you looking for a session two days
later is a word from **inside** it: a hostname, an error string, the name of the
thing you were arguing about. None of that is on the row.

So each claude session's transcript is indexed, and **`Ctrl-t`** makes typing search that too:

```
pick ❯ worktree                                                          10/27

  work:6.5       api-gateway: retry-budget-tuning              ⌕ …d it on a worktree off the release branc··
  work:9.4       config-generator: ruff-style-refactor          ⌕ …ing in a scratch worktree, so the main ··
  main:1.3       docs-site: rollback-the-theme-bump             ⌕ …hat worktree is still checked out here,··
```

The **snippet takes the place of the path column**, so a row that appears out of
nowhere says why it did, and the preview shows the first few places the word turns
up, with the term picked out. Nothing to configure and nothing on disk: the index
lives in `$XDG_RUNTIME_DIR` and dies with the boot.

**It is off until you press `Ctrl-t`, and that default was once a safety
feature.** fzf
reads a pasted line break as Enter, and the only thing stopping the picker
accepting on a paste is that [a pasted line matches no session](#notes). Searching
transcripts hands it something to match: measured over a few hundred real
transcripts, a line of pasted shell config went from matching 0 rows to matching
14, and one of them was plain
prose, so no "does it look like a paste" heuristic can save it. Left always on,
the picker accepts on the paste's first line, closes, and the rest is typed into
the agent in the pane behind. That is the bug the guard was written for, and it
came back the day search shipped, with a host going down behind it. Off by
default, typing matches rows exactly as it always did.

That reason is gone: the picker reads a paste as a paste (see
[the picker](#the-picker)). `Ctrl-t` still starts off, for the plainer reason
that searching every transcript on every keystroke is work to ask for rather
than to pay for by default.

Three things about how it matches, each of which had to be that way:

- **Substring, not fuzzy.** The row matcher is fuzzy, and a fuzzy query against a
  quarter of a megabyte of prose matches *everything*: the letters of `worktree`,
  in order but scattered, are in any long conversation. So taimux does the
  content matching itself, one plain substring test per whitespace-separated
  term, and puts only the short snippet on the row, which contains what you typed
  and so can be matched the ordinary way, on a row you can see all of.
- **Case-insensitive until you type a capital**, the smart case fzf popularised,
  so the row and the transcript behind it never disagree about what a capital
  means.
- **Three characters minimum.** Below that a term is in every transcript and the
  match would say nothing, so short queries filter on the row alone.

Indexing runs **behind** the picker, never in front of it: a cold pass over two
dozen live sessions is a second or two of `awk`, and the popup has to be up before
that. A list is built from whatever the cache already holds and a refresh is fired
off detached, so the auto-refresh timer shows the new coverage a tick or two
later. The honest cost is that for a moment after the first picker of a boot, a
search finds less than it is about to. Afterwards a refresh only reads the bytes a
transcript has *gained*, which is a few kilobytes for the session you are actually
working in.

What gets indexed is the **prose**: what you typed, what the agent wrote back, and
the names of files you attached. What does not is everything that would drown it:
tool results (file contents and command output, which are most of a transcript's
bytes: 23 MB of transcript here yields 428 KB of prose), tool-call arguments,
subagent sidechains, and the harness's injected catalogues and system reminders.
That last one is not hypothetical: the deferred-tool list alone puts
`EnterWorktree` in every session, and indexing it made `worktree` match 154
sessions instead of the 9 that had discussed one.

For a **live pane** it is claude-only, like `restart` and `resurrect` and for the
same reason: it has to know which conversation a pane is on, and claude is the
only agent that publishes that. Live rows for other agents still match on what
they show.

For a **[past session](#past-sessions)** that limit does not apply, because a
conversation on disk names itself: every agent's history is indexed, so a word
from inside a Gemini chat or an OpenCode session finds it too. Each is boiled
down the same way, and each needed its own exclusions for the same reason claude
needed `<system-reminder>`: Gemini opens a session with a `<session_context>`
block carrying the whole workspace listing, Codex staples an
`<environment_context>` to your first message, and Antigravity adds
`<ADDITIONAL_METADATA>` to everything you type. Indexing any of those would put
every project's file names in every conversation that ever touched it.

| variable | default | |
|---|---|---|
| `TAIMUX_SEARCH` | `1` | `0` removes `Ctrl-t` and the index entirely |
| `TAIMUX_SEARCH_MIN` | `3` | shortest query worth searching content for |
| `TAIMUX_SEARCH_CAP` | `262144` | prose kept per session, in bytes; the most **recent**, since the opening of a live session is already on its row |
| `TAIMUX_SEARCH_TTL` | `5` | seconds between background index refreshes |
| `TAIMUX_SEARCH_REMOTE` | `1` | `0` leaves other hosts' rows matching on what they show |
| `TAIMUX_SEARCH_REMOTE_TTL` | `60` | seconds between index fetches per host |



## Sessions running outdated code

Claude Code updates itself several times a day and a running session keeps the
inode it started with, so on any machine that has been up a while some of these
rows are executing code that was replaced hours ago. The
[version column](#agent-version) says which ones by painting them **yellow**.
Tab's fifth stop is that same question asked of the whole list:

```
running outdated code                                                       2/2

  work:1.1        Refactor auth middleware      webapp/api      claude 2.1.229
  ops:3.1       ✳ Fix the flaky CI pipeline     infra/terraform claude 2.1.229
```

It is not a state, which is why it sits outside the four that are: a session
waiting for an answer is as far behind as an idle one, so this stop **crosses**
the three above it. What it holds is exactly the rows `Ctrl-x` and `F8` act on,
and for the same reasons: never a session on
[another host](#sessions-on-other-hosts) (the installed version here says
nothing about what that box would start), never an
[past](#past-sessions) one however far behind it last ran (there is no
process to put back), and never another agent (only claude publishes what it
would take to relaunch it). One predicate answers both the colour and the list,
so the two can never disagree.

Left open, it **empties itself**: a row you press `Ctrl-x` on stays put, marked
`↻`, until the session comes back on the installed version and drops out of the
list. `F8` clears the whole of it in one go, after showing you the plan.

The stop is not in the cycle at all where nothing is installed to compare a
version against (no launcher under `~/.local/share/claude/versions/`, or
`TAIMUX_VERSIONS=0`), since every row would then be judged against nothing.
Tab skips it rather than landing on a list that could only ever be empty,
exactly as it skips the past list without a sessions cache.

## Past sessions

Everything above lists **panes**, so everything above can only find a session
that is still running. The ones you actually lose are the other kind: a pane
closed, a tmux server restarted, a machine rebooted, and a conversation you were
three hours into is now a file nothing points at. The tool that wrote it will
bring it back, but only if you can say *which* one, and by then you rarely can.

Tab's last stop lists them, **across every agent**:

```
past sessions                                                              712/712

  3h    billing: reconcile the ledger              services/billing   claude 2.1.229
  2d    Docker networking on the spot VM           cloud/platform     opencode
  6d    taimux: debug-tmux-script-issue            ai/taimux          claude 2.1.243
  41d   why are my chrome windows not tiled?       ~                  agy
  119d  Resolve the conflict                       homeassistant/…    gemini
```

How long ago it stopped goes where a pane label would, because that column is the
one thing a past session cannot have and the one thing you sort them by in your
head. **Enter** opens the conversation again, **in its own tool**, in the
directory it ran in. The **preview** shows how it left off, the last few turns
with your own prompt among them. And
[transcript search](#searching-what-a-session-said) covers these too, which is
half the reason to have them: what you remember about last Tuesday is what was
said, not where it ran.

### Five agents, three shapes

| agent | where it keeps a conversation | what Enter runs |
|---|---|---|
| Claude Code | `~/.claude/projects/<project>/<uuid>.jsonl` | `claude --resume <transcript>` |
| Codex | `~/.codex/sessions/**/rollout-*.jsonl` | `codex resume <id>` |
| Gemini | `~/.gemini/tmp/<project>/chats/session-*.jsonl` | `gemini --resume <id>` |
| Antigravity | `~/.gemini/antigravity-cli/brain/<id>/…/transcript.jsonl` | `agy --conversation <id>` |
| OpenCode | `~/.local/share/opencode/opencode.db` | `opencode --session <id>` |

Each is read where it is, in whatever shape it is in, and each one had something
in it worth knowing:

- **Gemini names its project directory after a SHA-256** of the path the session
  was opened in, so taimux carries a SHA-256 to read it. Half the directories on
  this machine are hashes and half are friendly labels, depending on which
  version wrote them, and both are resolved through `~/.gemini/projects.json`.
  It also has **two layouts**, one object per line and one pretty-printed object
  holding a `messages` array; the second is 106 of the 132 sessions here, and a
  line-based reader finds nothing at all in it. Both are read by the same code,
  by marker rather than by line.
- **Gemini's id is in the header, not in the filename**, which carries only its
  first eight characters. `--resume` is a recent addition over there and rses
  predates it, which is also the one thing on this page not verified against a
  live tool: Gemini is not installed on the machine this was written on, so the
  flag is what its session-management documentation specifies and everything
  else here was checked against real data.
- **Antigravity keeps the directory somewhere else entirely**, in a
  `history.jsonl` keyed by conversation id, and staples an
  `<ADDITIONAL_METADATA>` block to everything you type. The block is dropped.
- **Codex has two schemas**, and a rollout is one or the other depending on when
  it was written. Their record shapes are disjoint, so both are recognised per
  record, which is what lets a backwards reader that cannot see line 0 read
  either. Its own `state_<n>.sqlite` index is not used: everything the list needs
  is in the rollout, so this keeps working when that database changes shape.
- **OpenCode is a database**, which changes three things: the key is a session id
  rather than a path, there is no transcript to point a handoff at, and its
  fingerprint is a timestamp rather than a byte count. It is read through
  [turso](https://github.com/tursodatabase/turso), a pure-Rust SQLite, so
  `cargo build` still needs no C toolchain and the release is still one static
  binary. `--no-default-features` drops it and nothing else.

**Nothing is ever written where those tools live.** SQLite creates a `-wal` file
beside whatever path it is handed, so opening `opencode.db` where it lives drops
a file into another program's directory while that program may be running.
taimux opens a **symlink** in its own runtime directory instead, which puts the
`-wal` next to the symlink: same inode, same bytes read, nothing at all written
where OpenCode can see it.

### Only claude can be told apart from a live pane

Which conversation a pane is on is something the agent has to publish, and claude
is the only one that does (see [Told, rather than guessed](#told-rather-than-guessed)).
So a **claude** session that is open in a pane is left out of this list, because
it is already on every other one. A session belonging to any other agent is not:
taimux cannot tell, and pretending otherwise would mean guessing.

That is why this is "past sessions" rather than "ended" ones. It is the history,
and a conversation you happen to be in right now is part of it.

### A few decisions worth knowing

- **There is no cap.** There used to be one, at the newest 200, and what it cost
  was exactly the sessions you go looking for: a conversation from last month is
  the one you cannot find any other way, and it is also the first one a recency
  cap drops. With 572 claude transcripts here, two thirds of the history was
  invisible and nothing on the list said so. `TAIMUX_SESSIONS_MAX` still exists
  for a machine that wants a bound back; it defaults to `0`, meaning none.
- **Enter refuses if the directory has gone**, rather than falling back to
  `$HOME`. A session resumed in the wrong directory writes its history into a
  different project, and does it silently.
- **A new window, not this pane.** The picker is opened *from* somewhere, and
  taking that pane would be a jump that destroys where you jumped from.
- **`Ctrl-x` says so.** There is no process to restart and no pane to type into,
  so the restart key explains itself instead of looking like a key that did
  nothing.
- **An untitled session shows its last prompt.** A session can end before it was
  ever titled (a short one, or one that was cleared), and the prompt it was last
  given identifies it far better than `(no title)`. One of the ones here turns out
  to be the tail of a pasted config file, which is exactly the session you would
  go looking for. For the agents that record no title at all, the opening prompt
  *is* the title.
- **Subagents are not sessions.** A claude subagent writes to
  `projects/<proj>/<session>/subagents/agent-*.jsonl`, and a workflow's goes a
  level deeper still. Those are sidechains of a conversation that is itself in
  the list, they carry no title and no directory, and resuming one opens
  something nobody ever had a pane on. 72 of them were on this list; they are
  not now.
- **It is local-only**, on the same rule as [restart](#taimux-restart): a
  conversation lives on the box that had the pane, and resuming one is a command
  in a directory over there.

The list comes off a cache the [background indexer](#searching-what-a-session-said)
builds in the same pass, so it costs the picker nothing to open.

### What a pass costs

A pass has to answer two questions about every conversation: what a row says
about it, and what it said. Both used to be answered by reading it, and going
from 200 conversations to 712 made that a thirty-second pass. It is **1.1s cold
and 0.12s warm** now, over more than three times the history, and every part of
that came from measuring rather than guessing:

- **Ask before reading.** A conversation reports a fingerprint (a file's length,
  a row's timestamp) for a `stat` or an indexed lookup. Reading it to find out it
  had nothing new was a gigabyte of I/O per pass.
- **Read the tail, not the file.** Everything a row says is restated near the end
  of a transcript, so a 37 MB conversation costs the same as a small one.
- **The prose extractor was quadratic**, and had been since before any of this:
  pulling one JSON string out of a record copied everything from the match to the
  end of the LINE, three times, and a record holds many strings. 978 MB of
  transcripts took 14s to extract and take 1.6s now. That fix helps every live
  session too.
- **A shared queue, not a slice each.** Conversation sizes are skewed by two
  orders of magnitude, so cutting a recency-sorted list into four equal counts
  put three quarters of the bytes in one piece: 0.2s, 0.2s, 0.4s and 13.1s.
  Threads that take the next conversation as they finish the last cannot be
  handed the wrong share.

| variable | default | |
|---|---|---|
| `TAIMUX_SESSIONS` | `1` | `0` takes the stop out of the Tab cycle |
| `TAIMUX_SESSIONS_MAX` | `0` | a cap on how many conversations are tracked, newest first; `0` is none |
| `TAIMUX_DEAD_TURNS` | `6` | turns of the conversation the preview shows |
| `TAIMUX_INDEX_THREADS` | half the machine, at most 4 | threads a pass reads with |

## Carrying a conversation into another agent

The list above answers "where did that session get to". **`Ctrl-o`** answers the
question that usually comes next: continue it somewhere else. A Codex session you
want Claude to finish; a Claude session whose directory you want Gemini to look
at. It is the thing rses existed for, and the reason a conversation is worth
finding in the first place is often that you want a different tool on it.

It opens a new window running the target agent with one prompt already typed:

```
Continue this work. You are picking up from a Claude Code session.
Work in: /home/you/workspaces/billing
Branch: main

Task:
  billing: reconcile the ledger

Recent commits:
  8f2c1ad fix: the rounding on partial refunds
  …

Uncommitted changes:
   M src/ledger.rs

Recent conversation (6 messages):
  User: the totals are off by a cent on refunds
  Claude Code: That is the rounding in `apply_refund`. …

Full session transcript: /home/you/.claude/projects/…/8c1f….jsonl
Read this file if you need the complete conversation history.
```

Four things, in the order a model reads them: **the instruction** first, because
that is what it acts on; **the task**; **the git state**, which is the ground
truth of what was actually done and the half a conversation is worst at reporting
honestly; and **the last turns**, yours and its. Then a pointer to the
transcript, which is why the turn budget can stay small: the prompt is an
orientation, not an archive. (OpenCode keeps its conversation in a database and
so has no file to point at; everything it hands over is in the prompt.)

`Ctrl-o` works on a **past** row and on a **live claude pane**, which is the
other half of it: handing off what you are in the middle of is the common case,
and taimux can resolve a live claude pane to its transcript by the same ladder
[restart](#taimux-restart) uses. A live pane running any other agent refuses and
says why, on the rule above.

The menu offers only the agents actually on your `PATH`, and never the one the
conversation came from: continuing in the tool that already has it is **Enter**,
which resumes it rather than starting a fresh session carrying a summary of
itself.

Each target is started the way that tool takes a prompt interactively, which is
not always the obvious flag: `opencode --prompt` rather than `opencode run`, and
`agy -i` rather than `agy --prompt`, both of which would otherwise answer once
and exit.

The same thing without the picker:

```sh
taimux handoff <row id>                  # print the prompt
taimux handoff <row id> --to codex       # open a window running codex on it
taimux handoff <row id> --to codex --print   # …show the command line instead
```

A row id is what `taimux dead-rows` prints (`dead:<agent>:<key>`), or a local
claude pane id.

| variable | default | |
|---|---|---|
| `TAIMUX_HANDOFF_TURNS` | `6` | turns of the conversation the prompt carries |

## Sessions on other hosts

A pane holding `ssh <host> -t tmux …` is a window onto a whole other tmux server,
and everything above is blind to it: those sessions are not this server's panes
and not this box's processes. They are listed anyway, labelled by host:

```
  work:1.1      ✳ Refactor auth middleware      webapp/api        claude 2.1.229
  box/main:1.7  ◐ infra: chase the NIC resets   ~                 claude 2.1.246
  box/main:2.2    torrent client: bump to 0.16  workspaces/torrent claude 2.1.246
  lap/main:2.4    notes: tidy the inbox         home/you          claude 2.1.245
```

**Nothing to configure.** Whatever your panes are already ssh'd into *is* the
list. A host is discovered from the pane's own foreground `argv`, which is
already being read for every pane, and it joins the moment taimux is installed
over there. Discovery beats a config file on three counts that all follow from
the same fact: the pane's connection already exists. Its `ControlMaster` is warm,
so a fetch is a tenth of a second rather than a handshake; the host drops off the
list when its pane goes; and it arrives with its jump target already identified,
because the pane it was found in *is* the way back to it. `TAIMUX_REMOTE=0`
turns the whole thing off.

Each host is asked about **itself**, with `taimux list`. Nothing reaches in: the
hook state, the screen reading, the version, the transcript are all resolved on
the side that can actually see them, and the local tool only merges the replies.
That is also why `list` is deliberately local-only: it is the wire format, so a
host answering with remotes of its own is what would make a cycle (two boxes
ssh'd into each other) possible, and it simply cannot.

**Enter** puts that host's tmux on the pane you picked, *then* takes you to the
pane ssh is running in, so the window is already right when you land. The remote
server is driven over ssh as a peer rather than by typing at it, which is what
keeps the nested-prefix problem out of this entirely. **Preview** works the same
way, rendering the pane where the pane is. A host that has gone quiet since the
list was built still takes you to its pane, because that is where the dead
connection is; a host whose pane has *closed* since then refuses, and says so.
Enter is the one key here that must never look like it did nothing.

Pressed from **inside** a nested session, the pane you are in is the ssh pane,
which is not an agent row at all, so the picker would open at the top of the
list precisely when you were already looking at one of the sessions in it. The
current pane is therefore translated into that host's own current pane, and the
`●` marker and the opening cursor land on the row you are actually on. That one
is asked live rather than read off the cache: it changes the moment you move
around over there, and a few seconds of staleness would put the cursor on the row
you just left, which is a worse answer than the top of the list. It is bounded
tighter than anything else here (`TAIMUX_SSH_CUR_TIMEOUT`, default `2`) because
it happens in the opening frame, and a host already known to be down is not asked
at all: failing it costs the cursor placement and nothing else.

**Restarting does not.** `ctrl-x` on a remote row hands you the line that would
do it (`ssh <host> taimux restart --pane %6`) rather than half-doing it here: a
restart reads `/proc`, resolves a transcript under `~/.claude` and sends keys to
a tmux pane, all of which have to happen where the session is.

**[Searching what a session said](#searching-what-a-session-said) does**, and it
is the one part of federation that moves real bytes: each host indexes its own
sessions and hands the blobs over on request, and they are filed here beside the
local ones so a keystroke is still one pass over one directory. That is why it
has a TTL of its own, a minute rather than the three seconds a *list* is worth:
what was said in a session does not go stale on the timescale a pane's state
does. A host running a taimux that predates this answers with nothing, and its
rows go on matching what they show. Nothing breaks, and nothing has to be
upgraded in step.

Nothing on the refresh path waits on the network. A host with **nothing** cached
is fetched once, in parallel with the others, so the first picker after a reboot
is complete; after that a reply is served as it stands and refreshed behind the
picker, landing on the next tick. That matters because the refresh is on the
picker's critical path: a host over the network may cost the list its freshness,
never its responsiveness. Steady-state
cost of the whole feature: ~5 ms per host per refresh.

A host that **stops** answering keeps its place as one row saying so, since a
host silently vanishing looks exactly like a host with no sessions on it. Only
one that has answered before, though: a box that has simply never had taimux
installed is not an outage and does not get a row. Failures are cached for a
minute rather than seconds, so a sleeping host is not re-probed on every tick.

| variable | default | |
|---|---|---|
| `TAIMUX_REMOTE` | `1` | `0` disables federation entirely |
| `TAIMUX_REMOTE_TTL` | `3` | seconds before a reply is refreshed |
| `TAIMUX_REMOTE_FAIL_TTL` | `60` | seconds before a failed host is retried |
| `TAIMUX_HOSTS_TTL` | `5` | seconds before the host list is rediscovered |
| `TAIMUX_SSH_TIMEOUT` | `4` | hard limit on any one remote call |
| `TAIMUX_SSH_CONNECT_TIMEOUT` | `2` | ssh `ConnectTimeout` |
| `TAIMUX_SSH_CUR_TIMEOUT` | `2` | limit on the opening "which pane are you on" |

Two things worth knowing. A pane where you ssh'd in and then typed `tmux attach`
by hand cannot be told from a plain login shell and is not found; launching the
attach from the ssh command line is what makes it visible. And version staleness
is judged against the launcher on *this* box, so a remote version is reported but
never painted yellow: that host's own taimux is the one that knows.

## Putting a Claude Code session back

Everything above is about *finding* a session. Three subcommands are about
putting one **back**, and they are Claude Code specific because the thing that
makes it possible is: a session lives as a JSONL transcript under
`~/.claude/projects/`, and `--resume` takes one.

All three share one question, *which conversation is this pane on?*, answered
by a ladder that **refuses rather than guesses** at the bottom, because resuming
the wrong conversation pulls another pane's history in and orphans the work that
was actually here:

1. the **pane map** written by a `SessionStart` hook, which is rewritten on
   startup, resume, `/clear` and compact alike, so it tracks a session that
   changed conversation mid-life. Checked against the pane's cwd, which is what
   stops a nested `claude -p` (it inherits `$TMUX_PANE`) handing over its
   throwaway session;
2. an explicit **`--resume` in the live argv**, which covers every pane a
   previous restart relaunched. A *forked* session is the trap: it runs as
   `--session-id <new> --fork-session --resume <PARENT>`, so the `--resume` value
   names somebody else's conversation and the id is taken instead. **`claude
   attach <id>`** counts here too, and it is the one shape that can put a pane on
   a conversation from *another* directory (attach runs wherever you happen to be,
   while the session keeps the project it was created in), so its id is looked up
   across every project rather than under the pane's cwd. Only as the *first*
   argument: a flag in front of it shifts it out of the subcommand slot and claude
   reads the whole line as a prompt instead. An id matching more than one
   transcript is refused rather than guessed at;
3. the **pane title** against the titles each recent transcript recorded, in both
   prefix directions and with or without the title hook's `<project>: ` prefix;
4. when several sessions share a title, a **`tool-results` path in the pane's own
   scrollback**, which is pane-anchored evidence. Only ever used to narrow an
   existing title match, never alone;
5. a project directory holding exactly **one** recent transcript.

### `taimux restart`

Claude Code self-updates by writing a new binary under
`~/.local/share/claude/versions/` and repointing the launcher symlink. A running
session keeps executing the inode it started with, so it stays on its old version
until something restarts it, and nothing in the CLI does. Updates land several
times a day, so stale panes are the normal state.

```sh
taimux restart          # print the plan, then ask (dry run with no terminal)
taimux restart -y       # restart without asking
taimux restart -n       # print the plan and stop
```

Every confirmation here takes **`Y` as its default**, so `Enter` goes ahead with
the plan it has just printed. Anything that is not `Enter`, `y` or `yes` cancels,
including a read that fails outright, and that is stricter than `[Y/n]` usually
reads on purpose: see [the note under `F8`](#usage).

Only panes **behind** the installed version, and only **idle** ones unless
`--include-busy`: the transcript is appended per *completed* message, so
restarting a working session drops its in-flight turn. A pane is settled when its
screen shows a plain empty prompt box, no dialog is waiting, nothing is typed but
unsent, and its transcript has been quiet for 45s.

The dialog check is **never** skipped, `--include-busy` or not: an `Enter` sent to
a pane showing a permission prompt picks its highlighted option and approves a
tool call nobody approved.

Also `--update` (run `claude update` first), `--pane %id` (repeatable) and
`--transcript PATH` for a pane you have identified yourself.

Stale claude processes that are **not** a pane's foreground job (background
agents, Zed ACP, the daemon's pty hosts) are reported and never touched: they
have no tty and no pane to send keys to.

### `taimux resurrect`

[tmux-resurrect] saves the command line it finds under each pane and types it back
on restore. For a claude pane that is whatever the session was *launched* with,
normally a bare `claude`, so a restore opens a **new** conversation and orphans
the one that was live. This rewrites the save so each pane comes back on its own:

```tmux
set -g @resurrect-processes '... "~command claude"'
set -g @resurrect-hook-post-save-layout 'taimux resurrect'
```

Two details in that wiring are load-bearing. **`post-save-layout`**, not
`post-save-all`: it is handed the file just written and runs *before* resurrect
decides whether the save changed anything, so rewriting later would make every
save look like a change and pile up snapshots. And the restore-list entry must be
the **tilde** form: without `~` resurrect anchors the match at the start of the
string, and a rewritten command legitimately begins `cd <dir> &&`. A trailing
space inside the pattern is the other half of that trap, since
`"~command claude "` never matches a flagless `command claude`.

A pane whose conversation cannot be identified comes back as a **fresh** `claude`
in the right directory with the same flags, rather than as the bare shell
resurrect would leave: an empty pane where a session used to be is silent, a
claude prompt in the right directory says dig here.

Logs one line per save to `<save dir>/taimux-resurrect.log`, which is the only
place the wiring is visible between reboots (saves run detached with output
discarded). `-n` shows the rewrite as a diff and changes nothing.

### `taimux print-cmds`

The resolution on its own, one TSV record per claude pane: `pane`, `target`,
`cwd`, `status` (`resume`/`unresolved`), `why`, `command`. Read-only, and
deliberately without the version filter and the settle checks a restart needs: a
pane mid-turn is exactly the pane whose conversation a restore has to bring back.
This is the seam `resurrect` is built on.

[tmux-resurrect]: https://github.com/tmux-plugins/tmux-resurrect

## How detection works

For each tmux pane, `taimux` scans the processes in its terminal's **foreground
process group** (`ps`, `pgid == tpgid`) and matches their argv against the agent
patterns, taking the first that matches. Scanning the whole group rather than just
its leader means an agent started by a resident launcher that stays in front
(such as `rses` or `npx`, where the pane's leader is `node …` and the agent is
its child) is still detected. Because headless / SDK / sub-agent invocations (e.g.
`claude` with `--output-format` / `--print`) are never a pane foreground, they're
excluded, as is any agent running outside tmux. Only the current tmux server is
scanned; detection is stateless (no daemon, and nothing is cached but the version
strings described above).

## Demo

`demo/demo.sh` spins up an **isolated** tmux server (`-L taimux-demo`) populated
with synthetic agent sessions (generic project names and task summaries, fake
agent binaries) so you can try the picker without touching (or exposing) your
real sessions. Two of them draw a permission dialog and three an activity line,
so the state column and `Tab`'s filter have something to show. It binds
`prefix + a` in the demo server and attaches you to it; on exit it tears the
demo server down.

```sh
demo/demo.sh            # interactive: sets up fake sessions and attaches
demo/demo.sh --snapshot # non-interactive: rows, a sample preview, and print-cmds
demo/demo.sh --clean    # kill a leftover demo server
```

The snapshot also runs `print-cmds`, because the synthetic panes are exactly the
case it has to get right: a fake `claude` with no transcript behind it must come
back `unresolved` **with a reason**, never resolved to somebody else's
conversation. `restart` and `resurrect` are deliberately not demoed, since one
types into live panes and the other rewrites a resurrect save file, and a demo
should imitate neither.

## Notes

- The script prepends the mise shims directory
  (`${MISE_DATA_DIR:-~/.local/share/mise}/shims`) to `PATH`, because tmux's
  `display-popup` runs a non-login shell that otherwise can't find them.
- It also runs with `MISE_OFFLINE=1`. Reaching a tool through a shim otherwise
  puts mise's version resolution in front of the picker, and for a fuzzy pin
  (`latest`) it has never resolved before, that means a blocking call to
  `api.github.com`: while GitHub is unreachable the popup is an empty box for as
  long as the call takes to give up. Every tool the picker runs is one that is
  already installed, so nothing here needs the network. The cost is that a shim
  can no longer install a missing tool on first use; taimux says so instead of
  starting, and `MISE_OFFLINE=0 taimux` opts back in.
- Anything the picker runs on the way up is under a time limit. A helper that
  hangs cannot be escaped with `Ctrl-c` (the shell waits for it whatever it does
  with the signal), so that limit is the only thing standing between a wedged
  tool and a popup you can't close.
- The version column is painted **yellow** for a claude session running behind
  `~/.local/bin/claude`, so a self-update's leftovers are visible at a glance.
  Colour rather than an extra column, for the same reason the permission mode
  rides on the agent name: it costs the summary no width. An installed version
  that cannot be read marks nothing rather than everything.
- Other subcommands: `list` (machine-readable rows), `preview <pane_id>`,
  `switch <pane_id>`, `hook` (reads a hook payload on stdin),
  `bind` ([the bindings, in the running server only](#as-a-tmux-plugin-tpm--tpack)),
  `help`, and the
  three that put a Claude Code session back: `restart`, `resurrect`,
  `print-cmds` (see [Putting a Claude Code session back](#putting-a-claude-code-session-back)).
- The hook lines live in `$XDG_RUNTIME_DIR/taimux/`, so they die with the boot
  rather than outliving the pids they name. `SessionEnd` removes its own. The
  [transcript index](#searching-what-a-session-said), the list of
  [conversations](#past-sessions) and the pane scan behind a keystroke are
  cached there too, for the same reason: none of it is worth keeping across a
  reboot, and all of it is cheap to rebuild.
- **A paste that lands while the picker is open cannot escape it.** The picker
  asks the terminal for bracketed paste, so a paste arrives as one event
  carrying its own text and can never be mistaken for Enter.
  That is worth stating because the obvious implementation gets it wrong, and
  this one did. Reading keys one at a time, a pasted line break *is* Enter: the
  picker accepts on the first line, and every chunk still in flight arrives
  after it has gone, so the terminal hands them to whatever pane is current by
  then, types them into the agent living there, and the paste's own trailing
  newline submits them. That is not hypothetical: two sessions were caught days
  apart holding the tail of a pasted config file as a prompt nobody wrote.
  Guarding it after the fact takes an accept-on-non-empty rule, a drain of the
  terminal on the way out and a grace period to size, and still leaves a gap.
  Asking for bracketed paste removes the class instead, which is why the picker
  is drawn rather than delegated. See [the picker](#the-picker).

## The daemon

Every command runs in one binary, `taimux`. It started as a side process doing
the repeated collection work once instead of per picker, and it kept absorbing
until there was nothing left outside it.

Inside, it is four crates, and the boundaries are enforced by the compiler
rather than by convention:

| | |
|---|---|
| `core/` | pane scanning, transcripts, agent versions, paths, every agent's history. **One dependency, behind a default feature** |
| `daemon/` | the socket, the protocol both ends speak, and the background indexer. Depends only on `core` |
| `cli/` | the picker, the row layout, restart, resurrect, ssh federation. The only crate that links a terminal library |
| `src/` | the dispatcher, and the one binary everything ships as |

The split is not about shipping more artefacts, and it does not: the installer,
the tmux plugin, the launcher symlink and another host's ssh lookup all resolve
exactly one name. It is about the compiler being able to say no. "The daemon
does not link the terminal stack" used to be a claim in a comment; now
`cargo tree -p taimux-daemon` prints two lines and nothing can quietly add a
third.

A long-lived server is still optional and still worth it when several pickers are
open at once, because it caches the `capture-pane` per pane across the clients
asking within one refresh tick:

```sh
just build
target/release/taimux serve &      # optional
```

Every client falls back to doing the work itself when the socket is absent, the
daemon refuses or it hangs, and all three have tests. **The turn-boundary hook
deliberately does not use the socket at all**: the work is local and stateless
(parse a small payload, walk a few `/proc` entries, write one line), so a round
trip would add latency and a second failure mode and buy nothing, and it has to
keep working when nothing is running. That is also why it is the one client a
daemon cannot break.

Measured on 31 live panes, byte-identical output in all eight fields:

| | |
|---|---|
| `taimux list`, bash | 434 ms |
| the same list, daemon (warm) | **65 ms** |
| a full picker row build | 493 ms → **134 ms** |

It also carries the **session hook**, which is the most frequently executed command
in the whole system: five turn boundaries plus one per tool call, per session,
across ~28 sessions. `taimux install-hooks` registers it, and refuses when the
binary is not built rather than registering a command that cannot run. Measured at
**20.5 ms an event against 1.9 ms**, because every bash run parses the 4400-line
script before doing anything. That ratio is why the two per-tool-call events are
affordable at all: at 685 µs an invocation they cost a thirty-call turn 20 ms of
CPU, where bash would have spent 615 ms of it. It deliberately does *not* use the socket: the work is local and
stateless, so a round trip would add latency and a second failure mode and buy
nothing, and the hook must keep working when no daemon is running.

`taimux scan-list` does all of it in-process with no daemon at all, which is
what the equivalence check diffs against, and `taimux classify` reads a screen
on stdin so the two implementations can be fed identical input. That differential
found the one real porting bug: `capture-pane` pads its output to the pane height,
so a dialog four lines from the content's end was twenty lines from the text's
end, and every session waiting for an answer read as idle. Bash never had to think
about it because `$(...)` strips trailing newlines for free.

Five dependencies, and the count is deliberate: everything that can be std is
(the `/proc` scan, the socket, the protocol, the transcript parsing, the SHA-256
Gemini names its project directories with). Four are what the picker below needs:
`ratatui`, `crossterm`, `unicode-width` and `fuzzy-matcher`.

The fifth is [turso](https://github.com/tursodatabase/turso), a pure-Rust SQLite,
and it is the only one `core` takes. Two agents keep their conversations in a
database rather than in files, and reading a b-tree by hand is several hundred
lines that have to be right about overflow pages and WAL frames. Pure Rust is
what makes it affordable: `cargo build` still needs no C toolchain and the
release is still one static musl binary. It sits behind the default `sqlite`
feature, so `--no-default-features` drops it, and the only thing that changes is
that [OpenCode and Codex rows](#past-sessions) stop appearing.

It is not free: the static release artefact went from 1.1 MB to 12.8 MB, so
turso is now most of what a host downloads. That is a real cost and it bought one
agent's history on this machine, which is the trade to re-examine if it ever
buys less.

## Not a shell script

taimux was 3479 lines of bash. It is now a Rust binary, ported subsystem by
subsystem over one day, and the bash that remains (the tmux plugin entry point,
the test harness, the demo) does nothing at runtime.

Every subsystem was held to the same bar: **the same bytes into both
implementations, byte-identical out**, on live data, before the bash was deleted.

| | |
|---|---|
| the row layout | 440 of 440 combinations, frozen as `tests/golden/rows.expected` |
| the transcript extractor | 436 of 436 real transcripts |
| the indexer | 200 of 200 index files, 199 of 200 blobs |
| the preview | 30 of 30 live panes, 8 of 8 past, and the remote row |
| `print-cmds` | every pane, three consecutive runs |
| `restart -n` | every option combination |
| resurrect | the rewritten save file, over a real 198-line save |
| federation | the merged list, and again with the caches cleared |
| the installer | the config written, and the message printed |

That method paid for itself. It found `find -L` having no maxdepth (subagent
transcripts four levels down changed which 200 a recency cap kept), an agent pid
read out of the version column, a helper that trimmed the trailing newlines a
`capture-pane` needs, and one real bug in the bash itself: `_fresh_from_saved`
dropped only the first word of a saved command, so a pane restored after a
previous resurrect pass came back as `command claude claude --model opus`.

Where the deletion would have taken the only definition of a behaviour with it,
a golden file was frozen first. `tests/golden/rows.expected` is the bash layout's
own output over a fixed fixture, 504 cases, captured immediately before that code
was cut and replayed on every test run.

## The picker

**Since 2026-09-02 the picker is drawn by `taimux`, not by fzf**, and the fzf
path is gone rather than kept behind a flag. Same rows, same keys, same colours:
the layout was ported byte for byte and the proof of that is in the repo, see
below.

What it fixes, beyond one child for the whole session instead of fzf plus a
reload per keystroke, a preview per cursor move and a re-exec of this script
inside each of those:

- **A paste can no longer walk out through the picker.** fzf read a pasted line
  break as Enter, so the picker accepted on the paste's first line and the rest
  landed in the agent behind the popup. Bracketed paste arrives here as one
  event carrying its own text: its first line joins the query, the rest is
  dropped, and there is nothing left to guard against. The `Ctrl-t` default and
  the paste drain both existed for that bug; the drain is gone.
- **The mode is a variable.** fzf kept no state, so the bash picker stored its
  mode in the border label and read it back by matching words in the text,
  carried the mode and search flag through every reload as quoted arguments, and
  needed `--track --id-nth=2` so a reload did not drop the cursor.
- **The refresh timer loses its idle gate.** fzf needed one because a reload
  blocked its input loop and swallowed arrow keys mid-navigation. A tick here is
  a redraw, and a typed query survives it.
- **Rows are fitted to the popup**, which the picker can measure directly rather
  than guessing at `tput cols` less fzf's chrome before fzf is up to report it.

The row layout is ported rather than redesigned, and held to the same bar as
every other stage: the same bytes into both implementations, byte-identical out.
440 of 440 combinations over a 37-row live fixture (11 widths, 4 cursor positions,
5 state filters, both version modes), plus 24 of 24 for the search and
cached-title paths.

**That bar is now in the repo rather than in a transcript.**
`tests/golden/rows.expected` is the bash layout's own output over
`tests/golden/rows.tsv`, captured immediately before the fzf code was deleted:
504 cases, and it cannot be regenerated. The suite replays them through
`taimux rows` and demands byte equality, so the layout has a definition that is
not the code implementing it.

Nothing is left in bash. The tmux plumbing, the restart and resurrect work, the
transcript indexer and the ssh federation are all Rust now, and the picker
assembles its own rows, local panes merged with every discovered host's. What
bash still does here is the parts that are not the tool: the tmux plugin entry
point, the release downloader, this test harness and the demo.

## Development

Tasks are automated with [`just`](https://github.com/casey/just):

```sh
just                   # list recipes
just build             # build it; everything else needs this first
just unit              # the crate's own tests: parsing, layout, the ladder, the guards
just test              # integration: the binary, in fixtures
just lint              # fmt + clippy, and shellcheck on what shell is left
just check             # all of it (run before committing)
just snapshot          # leak-free demo snapshot
just build-static      # the musl artefact a release carries
just install-release   # install the latest release here, as any host would
just ship <host>       # push the WORKING-TREE build to a host, for testing
```

Commit messages are [Conventional
Commits](https://www.conventionalcommits.org/), and not only as a convention:
release-please reads them to decide the next version, so `feat:`, `fix:`,
`perf:` and `refactor:` cut a release and `docs:`, `test:`, `ci:` and `chore:`
ride along in the next one.

## History

taimux began in June 2026 as a bash prototype: 3479 lines of shell that scanned
tmux panes, read their screens and drove an `fzf` picker. It worked, and it was
too slow in the one place that mattered. The turn-boundary hook alone ran five
times per turn per session, and `bash` plus `awk` plus their subshells cost more
than the work they did. `taimux list` took 434 ms where the binary takes 1.9 ms,
most of it spent parsing the 4400-line script before doing anything at all.

Over a day in September 2026 it was ported to Rust, one subsystem at a time,
each compared against the shell on live data before the shell version was
deleted. Where deleting would have destroyed the only definition of a behaviour,
the shell's own output was frozen first: `tests/golden/rows.expected` is 504
cases of it, it is compared byte for byte, and it cannot be regenerated. That is
why [Not a shell script](#not-a-shell-script) reads as it does, and why so many
comments in the source name "the bash version" as the specification. The
prototype is gone; what it defined is not.

The name took longer to settle than the rewrite. It was **jumpmux** until it
collided with an unrelated project doing much the same job, and after one
short-lived detour it became **taimux**: the AI sessions in your tmux. The
current repository starts at the rename, which is why its history is shorter
than the project is.

## Prior art

This space is crowded, and the biggest thing in it is
[`herdr`](https://github.com/herdrdev/herdr), "the runtime your coding agents
live on". It keeps terminals in its own background server, marks every pane
working / blocked / idle, gathers several machines into one list, lets agents
prompt each other over a socket API, and runs on macOS, Linux and Windows. If
you want the full product, that is the one to look at.

The difference is not a feature, it is where the thing sits. **herdr owns the
terminal**, with its own panes, splits and prefix keys, so adopting it means
leaving tmux and, on a restart, leaving the processes that were running in it.
**taimux owns nothing.** It is a picker over the tmux you already run: your
config, your plugins, your bindings and your muscle memory are all still there
afterwards, and removing it leaves no trace. That is the entire pitch, and it is
the right trade only if you already have a tmux you like.

Two things follow from sitting on top rather than underneath, and they are the
features herdr has no equivalent for. Whatever pane is already `ssh`'d into
another box *is* the remote configuration, so there is nothing to register. And
because the conversations are just files on disk, taimux can
[search what a session said](#searching-what-a-session-said) and
[list the ones that have ended](#past-sessions), not only the ones still
running.

[`rses`](https://github.com/yazcaleb/rses) asked the other half of the question, and
[past sessions](#past-sessions) and
[the handoff](#carrying-a-conversation-into-another-agent) are its ideas: browse
every tool's conversations in one list, whether or not anything is running them,
and continue one in a different tool. It has no tmux in it at all, which is
exactly why the two fit together rather than competing: it was the answer to
"which conversation was that", and this is that answer inside the picker you
already open. What came across is the shape of the handoff prompt, the tail-read
preview, and the observation that a harness's injected blocks will otherwise
flood a search index. What did not is its Node runtime, and its cap-free listing
is now the default here rather than the exception.

Smaller neighbours worth knowing:
[`tmux-agent`](https://github.com/trentdavies/tmux-agent) (`ta`, Rust, embedded
picker, worktrees) and
[`agent-deck`](https://github.com/asheshgoplani/agent-deck) (full TUI session
manager).

## Uninstall

```sh
rm ~/.local/bin/taimux
# then remove the "# taimux" block from ~/.tmux.conf.local (Oh My Tmux) or
# ~/.tmux.conf (plain tmux)
```

Installed as a plugin, drop the `set -g @plugin 'pdecat/taimux'` line and let
the manager remove the checkout (`prefix + alt-u` on tpm and tpack). There is no
config block to clean up, and a `~/.local/bin/taimux` symlink is there only if
you ran `taimux install` as well. Either way the keys stay bound in a server
that is already running, until it is restarted or you `unbind` them.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or
  https://www.apache.org/licenses/LICENSE-2.0)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or
  https://opensource.org/licenses/MIT)

at your option.

### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.
