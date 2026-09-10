#!/usr/bin/env bash
# taimux demo: runs the real picker against an ISOLATED tmux server populated
# with synthetic agent sessions. Nothing from your real tmux sessions is read
# or displayed: generic project names, generic task summaries, fake agent
# binaries, a dedicated socket ('-L taimux-demo'), its own XDG_RUNTIME_DIR (so
# even the caches and hook files land inside the demo's temp dir), and its own
# teardown.
#
# A SECOND isolated server ('-L taimux-demo-remote') stands in for another
# host, so the picker's "sessions on other hosts" half is demonstrated too. The
# transport is the only thing faked: an `ssh` on the demo's PATH runs the
# "remote" command locally against that second server. Everything above it is
# the real code path: the pane really does hold a nested tmux, discovery really
# reads it out of that pane's argv, and the rows, the preview and the jump all
# go through the same functions they would over a network. What it does NOT
# prove is anything about ssh itself: latency, ControlMaster reuse, a host that
# has gone away.
#
#   demo/demo.sh            interactive: set up fake sessions and attach
#   demo/demo.sh --snapshot non-interactive: print the rows, a preview, print-cmds
#   demo/demo.sh --clean    kill a leftover demo server
set -uo pipefail

SOCK="taimux-demo"
RSOCK="taimux-demo-remote"    # the second server, standing in for another host
RHOST="demo-remote"            # what that host is called in the rows

# EVERY tmux call below passes `-f`, and that is load-bearing rather than
# tidiness. A `-L` socket is a separate server but NOT a separate configuration:
# a new one reads ~/.tmux.conf like any other. With `@continuum-restore on` in
# it (a common setup, and the one on the machine this was written on) starting
# the demo server makes tmux restore the user's real saved layout into it: their
# session names, their project paths, and every `command claude --resume
# <transcript>` line tmux-resurrect saved, which would start REAL agent sessions
# inside a server this script advertises as synthetic. It also takes long enough
# to look like a hang. So the demo inherits nothing and brings its own config,
# which is also what makes it look the same on anybody's machine.
HERE="$(cd "$(dirname "$(readlink -f "$0")")" && pwd)"
# taimux IS the binary. The demo shows ROWS rather than driving the picker
# itself, since a TUI in an alternate screen has nothing to show in a transcript.
TAIMUXD="$HERE/../target/release/taimux"
[ -x "$TAIMUXD" ] || TAIMUXD="$(command -v taimux 2>/dev/null || echo "$TAIMUXD")"
if [ ! -x "$TAIMUXD" ]; then
  printf '%s\n' "taimux is not built: run \`just build\`." >&2
  exit 1
fi
REAL_TMUX="$(command -v tmux)"

kill_demo() {
  local d s
  for s in "$SOCK" "$RSOCK"; do
    if command -v timeout >/dev/null 2>&1; then
      timeout -k 1 3 tmux -f /dev/null -L "$s" kill-server 2>/dev/null
    else
      tmux -f /dev/null -L "$s" kill-server 2>/dev/null
    fi
  done
  # tmux leaves the socket FILE behind when its server is SIGKILLed, which is
  # what a demo cut short by a timeout leaves: the next run then hangs trying to
  # connect to it rather than replacing it, and with two sockets there are now
  # two ways to inherit that. The kill above has seen off anything alive, so
  # whatever is left here is stale. Only ever the two sockets the demo owns, by
  # exact name, and nothing in this script may go near the default one.
  d="${TMUX_TMPDIR:-/tmp}/tmux-$(id -u)"
  for s in "$SOCK" "$RSOCK"; do [ -S "$d/$s" ] && rm -f -- "$d/$s"; done
  return 0
}
clean()     { kill_demo; [ -n "${DEMO_DIR:-}" ] && rm -rf "$DEMO_DIR"; }

case "${1:-}" in
  --clean) kill_demo; echo "demo servers '$SOCK' and '$RSOCK' killed."; exit 0 ;;
esac

kill_demo   # start from a clean slate

DEMO_DIR="$(mktemp -d "${TMPDIR:-/tmp}/taimux-demo.XXXXXX")"
BIN="$DEMO_DIR/bin"; mkdir -p "$BIN"

# The config both servers are started with, in place of the user's. The indexes
# are here rather than set afterwards because they only affect sessions and
# windows created AFTER they are applied, and the first spawn is what brings the
# server up: set imperatively they would arrive one window too late, and the demo
# would show `work:0.0` where every screenshot and the README say `work:1.1`.
CONF="$DEMO_DIR/tmux.conf"
cat > "$CONF" <<'EOF'
set -g base-index 1
set -g pane-base-index 1
set -g prefix C-a
set -g status-style 'bg=colour236,fg=colour252'
set -g status-left ' #[bold]taimux demo#[default]  (C-a a to pick) '
set -g status-left-length 40
EOF

# Fake agent binaries, at <agent>/<version>/<agent>, the shape a real install
# has, so the picker's version column has something to read (it takes the version
# from the running binary's own path). Each is a copy of bash: it has to be a
# real executable (a script's running binary is its interpreter), and coreutils
# is out because its multi-call binary refuses to run under another name.
fake_agent_bin() {   # $1 = agent -> path to a versioned fake binary for it
  local ver d
  case "$1" in
    claude) ver=2.1.229 ;; gemini)      ver=0.41.2  ;; codex) ver=0.20.3 ;;
    agy)    ver=1.0.14  ;; antigravity) ver=0.3.1   ;; pi)    ver=0.9.4  ;;
    *)      ver=1.18.10 ;;
  esac
  d="$DEMO_DIR/agents/$1/$ver"
  # Only copied once. Several sessions can share one agent (three claudes now,
  # across the two servers), and copying over a binary another pane is already
  # executing fails with ETXTBSY.
  mkdir -p "$d" && { [ -x "$d/$1" ] || cp "${BASH:-/bin/bash}" "$d/$1"; } && printf '%s' "$d/$1"
}

# A single synthetic "coding agent": publishes a task title, draws a small panel,
# then idles as the foreground process *under the agent's name*, so taimux
# detects it exactly as it would a real agent (incl. the argv path used for
# Node-wrapped CLIs like gemini).
#
# It also draws whatever its assigned state looks like on screen, at the bottom of
# the pane where a real agent's prompt box sits, because that is where taimux
# reads the state from: a numbered choice list for a session holding a question, a
# live activity line for one mid-turn, a bare prompt for one that is idle. Without
# it every demo row would read as idle and the state column would show nothing.
FAKE="$BIN/agent-stub"
cat > "$FAKE" <<'STUB'
#!/usr/bin/env bash
name="${TAIMUX_DEMO_AGENT:-agent}"
task="${TAIMUX_DEMO_TASK:-working…}"
glyph="${TAIMUX_DEMO_GLYPH:-✳}"
state="${TAIMUX_DEMO_STATE:-idle}"
printf '\033]2;%s %s\007' "$glyph" "$task"      # publish the task as the pane title
clear 2>/dev/null || true
printf '\n  \033[1m%s\033[0m \033[2m· taimux demo (synthetic session)\033[0m\n\n' "$name"
printf '  %s  %s\n\n' "$glyph" "$task"
printf '  \033[2m> working in %s/\033[0m\n' "$(basename "$PWD")"
printf '  \033[2m> (this pane is fake, no real agent is running)\033[0m\n'
case "$state" in
  input) block=$'  2. Yes, and don’t ask again\n  3. No\n\033[1m ❯ 1. Yes\033[0m\n \033[2mEsc to cancel · Tab to amend\033[0m' ;;
  run)   block=$'\033[2m✽ Twisting… (35s · ↓ 1.6k tokens)\033[0m\n ❯ ' ;;
  *)     block=' ❯ ' ;;
esac
n="$(printf '%s\n' "$block" | grep -c '')"      # how many lines it occupies
printf '\033[%d;1H%s' "$(( $(tput lines 2>/dev/null || echo 24) - n ))" "$block"
bin="${TAIMUX_DEMO_BIN:-}"                     # idle under the agent's own name
[ -x "$bin" ] && exec "$bin" -c 'sleep infinity & wait'
exec -a "$name" sleep infinity
STUB
chmod +x "$FAKE"

mkproj() { mkdir -p "$DEMO_DIR/projects/$1"; printf '%s/projects/%s' "$DEMO_DIR" "$1"; }

# spawn SESSION WINDOW AGENT PROJECT TASK [GLYPH] [STATE]  onto $TARGET_SOCK
TARGET_SOCK="$SOCK"
spawn() {
  local sess="$1" win="$2" agent="$3" dir task="$5" glyph="${6:-✳}" state="${7:-idle}" bin
  dir="$(mkproj "$4")"
  bin="$(fake_agent_bin "$agent")"
  local env=( -e "TAIMUX_DEMO_AGENT=$agent" -e "TAIMUX_DEMO_TASK=$task" \
              -e "TAIMUX_DEMO_GLYPH=$glyph" -e "TAIMUX_DEMO_BIN=$bin" \
              -e "TAIMUX_DEMO_STATE=$state" )
  if tmux -f "$CONF" -L "$TARGET_SOCK" has-session -t "$sess" 2>/dev/null; then
    tmux -f "$CONF" -L "$TARGET_SOCK" new-window  -t "$sess:" -n "$win" -c "$dir" "${env[@]}" "$FAKE"
  else
    tmux -f "$CONF" -L "$TARGET_SOCK" new-session -d -s "$sess" -x 220 -y 50 -n "$win" -c "$dir" "${env[@]}" "$FAKE"
  fi
}

# --- the second server, and the two shims that make it look like a host -------
#
# `tmux`, pinned to the other server. Only ever on the PATH the fake ssh builds
# for the command it runs, never on the demo server's own, so the picker's own
# tmux calls (which must reach the server it is running in) are untouched. The
# real binary by absolute path, or it would exec itself.
RBIN="$BIN/remote"; mkdir -p "$RBIN"
cat > "$RBIN/tmux" <<EOF
#!/usr/bin/env bash
exec "$REAL_TMUX" -f /dev/null -L "$RSOCK" "\$@"
EOF
chmod +x "$RBIN/tmux"

# `ssh`, which runs the "remote" command here instead of anywhere else. Real ssh
# joins its command argv with spaces and hands the result to the remote LOGIN
# SHELL, so `sh -c "\$*"` is the faithful thing to do rather than a shortcut: it
# is what makes the quoting in taimux's remote commands behave as it does over
# a real connection.
cat > "$BIN/ssh" <<EOF
#!/usr/bin/env bash
# Fake ssh, taimux demo only. See demo/demo.sh.
host=""
while [ \$# -gt 0 ]; do          # ssh's own options, then the host
  case "\$1" in
    --) shift; host="\${1:-}"; shift; break ;;
    -b|-c|-D|-E|-e|-F|-I|-i|-J|-L|-l|-m|-O|-o|-p|-Q|-R|-S|-W|-w) shift 2 ;;
    -*) shift ;;
    *)  host="\$1"; shift; break ;;
  esac
done
while [ \$# -gt 0 ]; do          # options may also follow it (ssh host -t cmd)
  case "\$1" in -*) shift ;; *) break ;; esac
done
[ "\$host" = "$RHOST" ] || exit 255
# A real ssh lands in a fresh login on another machine, where none of this
# side's tmux variables exist. Here they would be inherited, and \$TMUX is
# exactly what makes tmux refuse to start a session ("sessions should be nested
# with care"), so the pane would die instead of holding a nested server.
unset TMUX TMUX_PANE
export PATH="$RBIN:\$PATH"
# A real remote also has its own runtime and Claude Code directories: its hook
# lines, its transcript index and its conversations are all over there. Shared,
# the "other host" would answer with THIS one's index, which it did, and the
# demo showed a remote row matching a word only a local session had said.
export XDG_RUNTIME_DIR="$DEMO_DIR/run-remote"
export CLAUDE_CONFIG_DIR="$DEMO_DIR/claude-remote"
[ \$# -gt 0 ] || exit 0
# Deliberately NOT exec. Real ssh stays resident for the life of the session, so
# the pane's foreground argv keeps naming it, and that argv is the only thing
# discovery has to go on. Exec'ing would replace this process with the tmux
# client and the pane would stop looking like a window onto anywhere.
sh -c "\$*"
EOF
chmod +x "$BIN/ssh"

# Both of these have to be exported HERE, before the first tmux call brings a
# server up, because a tmux server hands its own startup environment to
# everything it spawns and `-e` / `set-environment` do NOT override PATH:
# measured, a pane created either way still resolved `ssh` to /usr/bin/ssh and
# the fake was never reached. Exporting instead covers every pane and the picker
# popup at once, which is what the picker needs anyway, since taimux looks up
# `ssh` on PATH like anything else.
export PATH="$BIN:$PATH"
# And the whole feature's on-disk state goes in the demo's own dir, so a run
# leaves no caches, hook lines or restart log behind in the real one.
export XDG_RUNTIME_DIR="$DEMO_DIR/run"
mkdir -p "$XDG_RUNTIME_DIR"
# Same for the Claude Code side of it. Transcript search reads conversations out
# of here, so pointing it at the demo's own directory is what lets a synthetic
# session have something to say without going anywhere near a real one.
export CLAUDE_CONFIG_DIR="$DEMO_DIR/claude"
mkdir -p "$CLAUDE_CONFIG_DIR/tmux-panes"

# Synthetic fleet, generic names only. The last column is the state each session
# draws on its screen, so the list has some of each: two waiting for an answer
# (what Tab filters down to), three mid-turn, the rest idle at the prompt.
spawn work claude      claude      webapp/api      "Refactor auth middleware"     "✳" input
spawn work gemini      gemini      webapp/frontend "Write tests for the parser"   "⠂" run
spawn work codex       codex       services/search "Add pagination to results"    "✳" idle
spawn work agy          agy          services/auth   "Migrate DB session schema"    "¯" run
spawn ops  opencode    opencode    infra/terraform "Fix the flaky CI pipeline"    "⠙" run
spawn ops  pi          pi          docs/site       "Update README and changelog"  "✳" input
spawn ops  antigravity antigravity apps/mobile     "Optimize image thumbnails"    "⠹" idle
# A non-agent pane, to show it is correctly NOT listed:
tmux -f "$CONF" -L "$SOCK" new-window -t ops: -n shell -c "$DEMO_DIR" "bash" 2>/dev/null || true

# A conversation for one of the claude panes to have had, so transcript search
# has something to find. Told the way a real session tells taimux: the pane map
# a SessionStart hook writes, which is the first and cheapest rung of the
# resolution ladder, plus a transcript in the shape Claude Code writes one. The
# words in it are deliberately NOT on the pane's row: that is the whole point of
# the feature, and typing `skew` into the demo picker is what shows it.
# CLAUDE_CONFIG_DIR is taken as an argument rather than read from the
# environment, because the other "host" keeps its own (see the fake ssh below)
# and its sessions have to be written where IT will look for them.
fake_conversation() {   # $1 = socket, $2 = claude dir, $3 = session:window, $4 = project, $5 = said
  local pane dir uuid tr
  pane="$(tmux -f "$CONF" -L "$1" list-panes -t "$3" -F '#{pane_id}' 2>/dev/null | head -n 1)"
  [ -n "$pane" ] || return 0
  dir="$DEMO_DIR/projects/$4"
  uuid="00000000-0000-4000-8000-$(printf '%012d' "${pane#%}")"
  tr="$2/projects/$(printf '%s' "$dir" | sed 's/[^A-Za-z0-9]/-/g')/$uuid.jsonl"
  mkdir -p "${tr%/*}" "$2/tmux-panes"
  printf '{"type":"user","message":{"role":"user","content":"%s"},"cwd":"%s"}\n' "$5" "$dir" > "$tr"
  printf '{"pane":"%s","transcript_path":"%s","cwd":"%s","source":"startup"}\n' \
         "$pane" "$tr" "$dir" > "$2/tmux-panes/${pane#%}.json"
}
fake_conversation "$SOCK" "$CLAUDE_CONFIG_DIR" work:claude webapp/api \
  "the tokens are fine, it is a clock skew between the two issuers"

# …and one that nothing is running: a transcript with no pane map beside it and
# no pane on it, which is exactly what a session looks like once its window has
# gone. This is the whole of what makes a session "ended", so the demo needs no
# more than to write one and leave it alone. Backdated, because how long ago it
# stopped is the column the list is read by.
fake_ended() {   # $1 = claude dir, $2 = project, $3 = title, $4 = what was said, $5 = age
  local dir tr
  dir="$DEMO_DIR/projects/$2"; mkdir -p "$dir"
  tr="$1/projects/$(printf '%s' "$dir" | sed 's/[^A-Za-z0-9]/-/g')/$(date +%s%N).jsonl"
  mkdir -p "${tr%/*}"
  {
    printf '{"type":"user","message":{"role":"user","content":"%s"},"cwd":"%s","version":"2.1.229"}\n' "$4" "$dir"
    printf '{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"%s"}]},"cwd":"%s","version":"2.1.229"}\n' \
           "Right, that was the cause." "$dir"
    printf '{"type":"custom-title","customTitle":"%s"}\n' "$3"
  } > "$tr"
  touch -d "$5" "$tr" 2>/dev/null
}
fake_ended "$CLAUDE_CONFIG_DIR" services/billing "billing: reconcile the ledger" \
           "the totals drift by one cent per invoice" "3 hours ago"
fake_ended "$CLAUDE_CONFIG_DIR" webapp/api "api: retire the v1 endpoints" \
           "the old handler still answers, so nothing can be deleted yet" "2 days ago"

# The other "host": its own server, its own sessions, reached only through the
# shims above.
TARGET_SOCK="$RSOCK"
spawn main billing claude services/billing "Reconcile the invoice totals" "✳" input
spawn main gateway claude services/gateway "Chase a 502 from the gateway"  "⠙" run
# …and a conversation over there too, in that host's own Claude directory. It is
# what proves the index federates: the word below is said only on the other host,
# so a row for it can only come back through the fetch.
fake_conversation "$RSOCK" "$DEMO_DIR/claude-remote" main:gateway services/gateway \
  "the 502s all came from one upstream with a clock skew of its own"
TARGET_SOCK="$SOCK"

# The pane that makes the other host visible at all. Its argv is exactly the
# shape discovery looks for: an ssh whose command starts a tmux over there.
tmux -f "$CONF" -L "$SOCK" new-window -t ops: -n "$RHOST" -c "$DEMO_DIR" \
  "ssh $RHOST -t tmux new-session -As main" 2>/dev/null || true

# The prefix and the status line come from $CONF above. Only the binding is set
# here, and deliberately so: it carries a `#{pane_id}` that the config parser
# would treat as a comment.
# Same shape as the binding `taimux install` writes, #{pane_id} included, even
# though display-popup expands no format in the command it runs: bare like this
# the shell drops it as a comment and the picker asks tmux which pane it was
# opened from, whereas quoting it would hand the picker the format as a string.
tmux -f "$CONF" -L "$SOCK" bind-key a display-popup -E -w 80% -h 80% "$TAIMUXD pick #{pane_id}" >/dev/null

if [ "${1:-}" = "--snapshot" ]; then
  snap="$DEMO_DIR/snap.txt"
  echo "=== taimux list, against the ISOLATED demo server (synthetic data) ==="
  tmux -f "$CONF" -L "$SOCK" run-shell "$TAIMUXD list-local > '$snap' 2>&1" ; cat "$snap"
  first="$(awk -F'\t' 'NR==1{print $1}' "$snap")"
  if [ -n "$first" ]; then
    echo; echo "=== sample preview of the first agent pane ($first) ==="
    tmux -f "$CONF" -L "$SOCK" run-shell "$TAIMUXD preview '$first' > '$DEMO_DIR/prev.txt' 2>&1"
    cat "$DEMO_DIR/prev.txt"
  fi
  # `list` above is the LOCAL wire format by design (a host must never answer
  # with remotes of its own), so it structurally cannot show the other server.
  # The picker's own row builder can, and this is the one place the demo proves
  # the federation end to end: discovery off the ssh pane's argv, the fetch
  # through the fake ssh, the host-prefixed ids and the labelled rows. `_panes`
  # is the internal entry point the picker asks for its rows, merged local and
  # remote, and `taimux rows` is what lays them out.
  echo; echo "=== the picker's rows: this server AND the simulated host '$RHOST' ==="
  tmux -f "$CONF" -L "$SOCK" run-shell \
    "$TAIMUXD all-panes | $TAIMUXD rows --width 100 --home '$HOME' > '$DEMO_DIR/rows.txt' 2>&1"
  awk -F'\t' 'NF==2 { printf "%-18s %s\n", $2, $1 }' "$DEMO_DIR/rows.txt"

  # Transcript search, end to end: the indexer resolves which conversation each
  # claude pane is on, boils it down and, for the other host, fetches its
  # answer to the same question; then the row builder matches a query against the
  # lot. `skew` is on no row anywhere, only inside the two synthetic
  # conversations, one per host. Only the rows carrying a snippet are shown,
  # which is what the picker's own matcher would be left holding.
  echo; echo "=== typing 'skew': matched on what a session SAID, not on its row ==="
  tmux -f "$CONF" -L "$SOCK" run-shell \
    "$TAIMUXD index; $TAIMUXD all-panes > '$DEMO_DIR/panes.txt' 2>&1"
  TAIMUX_Q=skew TAIMUX_SNIPS="$("$TAIMUXD" snips skew)" \
    "$TAIMUXD" rows --width 100 --home "$HOME" \
    < "$DEMO_DIR/panes.txt" > "$DEMO_DIR/found.txt" 2>&1
  awk -F'\t' '/⌕/ && NF==2 { printf "%-18s %s\n", $2, $1 }' "$DEMO_DIR/found.txt"

  # Tab's last stop: the conversations nothing is running any more. They come
  # off the sessions cache the same pass built above rather than off any scan of
  # this server, which is why the pane list and this one cannot contain each
  # other: the live claude session is absent here, and the two ended ones are
  # absent from every list above.
  echo; echo "=== tab, tab, tab, tab, tab: sessions that have ENDED ==="
  tmux -f "$CONF" -L "$SOCK" run-shell \
    "$TAIMUXD dead-rows | $TAIMUXD rows --width 100 --home '$HOME' > '$DEMO_DIR/ended.txt' 2>&1"
  # An ended row is named by its whole transcript path, which is the point (there
  # is no pane to name it by) and far too wide for a side-by-side dump, so only
  # the file it ends in is shown here.
  awk -F'\t' 'NF==2 { id = $2; sub(/^dead:.*\//, "dead:…/", id); printf "%-26s %s\n", id, $1 }' \
      "$DEMO_DIR/ended.txt"
  first="$(awk -F'\t' 'NF==2 { print $2; exit }' "$DEMO_DIR/ended.txt")"
  if [ -n "$first" ]; then
    echo; echo "=== …and how the highlighted one left off ==="
    tmux -f "$CONF" -L "$SOCK" run-shell "$TAIMUXD preview '$first' > '$DEMO_DIR/dprev.txt' 2>&1"
    cat "$DEMO_DIR/dprev.txt"
  fi

  # print-cmds is the read-only half of restart/resurrect, and the synthetic panes
  # are exactly the case it has to get right: fake claude binaries with no
  # transcript behind them, so every row must come back `unresolved` with a reason
  # rather than resolving to somebody else's conversation. Nothing is killed and
  # no save file is touched, so it is safe in a snapshot; `restart` and
  # `resurrect` are deliberately NOT demoed, since one types into panes and the
  # other rewrites a resurrect save, neither of which a demo should imitate.
  echo; echo "=== taimux print-cmds, refusing to guess, with no real transcripts ==="
  tmux -f "$CONF" -L "$SOCK" run-shell "$TAIMUXD print-cmds > '$DEMO_DIR/cmds.txt' 2>&1"
  if [ -s "$DEMO_DIR/cmds.txt" ]; then
    cut -f1,2,4,5 "$DEMO_DIR/cmds.txt"
  else
    echo "(no claude panes in the demo server)"
  fi
  clean
  exit 0
fi

trap clean EXIT
cat <<EOF

taimux demo: an isolated tmux server ('-L $SOCK') with synthetic agent sessions.
Nothing from your real sessions is used or shown, and no configuration of yours
is read: both servers start with '-f', so nothing of yours can be restored into
them, and the caches land in this run's own temp dir.

  • Press your prefix (C-a) then a to open the picker.
  • Enter to jump (the pane is zoomed) · Esc to cancel · Ctrl-/ toggles preview.
  • Tab filters: all → waiting → working → idle → outdated · Home/End jump to
    the ends.
  • Two rows are on ANOTHER HOST, labelled '$RHOST/…'. Window '$RHOST' in
    session 'ops' is the ssh pane they are reached through, and that pane is how
    the host was found at all. Enter on one of those rows puts that host on the
    pane and takes you there; Ctrl-x tells you restarting is local-only.
    Simulated LOCALLY: the '$RHOST' ssh is a stand-in that talks to a second
    isolated server, so everything above the transport is the real code path,
    and nothing about ssh itself is being demonstrated.
  • The fake claude's version shows yellow (it is behind the real installed one),
    which is also what puts it in Tab's 'outdated' list, so Ctrl-x / F8 are live
    here too, and safely refuse: a synthetic pane has no real session behind it.
    \$XDG_RUNTIME_DIR/taimux/restart.log says so.
  • Detach with C-a d, and both demo servers are then torn down.

Attaching…
EOF
sleep 1.6
tmux -f "$CONF" -L "$SOCK" attach -t work
