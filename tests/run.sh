#!/usr/bin/env bash
# INTEGRATION tests for taimux. Pure bash, no external test framework.
#
# These drive the taimux BINARY, in fixtures, and assert on what it produced.
# They source nothing: there is no script left to source, and the unit-level
# questions (parsing, layout, the ladder, the guards) are answered by the crate's
# own 230 tests, which can reach the pure functions directly.
#
# What is here is what only an end-to-end run can check: the bytes written to a
# real settings.json, the tmux command lines actually issued, an index built and
# then read back, the frozen layout replayed, and the locking a concurrent pass
# depends on.
#
#   tests/run.sh        # run all tests; exits non-zero on any failure
#
# Two patterns recur, and both exist because a compiled implementation cannot see
# a shell function:
#   · a fake `tmux` on PATH, writing every command to a log;
#   · a COPY of the binary at the path under test, so `current_exe` is that path.

HERE="$(cd "$(dirname "$(readlink -f "$0")")" && pwd)"
set +u +o pipefail   # keep the harness forgiving

# Federation OFF by default: these must not shell out over ssh, and their answers
# must not depend on which hosts the machine running them happens to have a pane
# open onto.
export TAIMUX_REMOTE=0

TMP="$(mktemp -d "${TMPDIR:-/tmp}/muxhop-tests.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT

# Everything taimux caches (the hook lines, the pane scan, the transcript index)
# lives under _hook_dir, so the whole of it is pointed at the temp dir before
# the first test rather than partway down: a run must neither read the developer's
# live state nor leave anything of its own in it. (The hook section below re-exports
# this to the same place, and reads on that.)
export XDG_RUNTIME_DIR="$TMP/run"

# Transcript search and the ended list OFF by default, for the same reason as
# federation above: they read real transcripts out of ~/.claude and fire a
# background indexer, neither of which a unit test may depend on or set off.
# Their own sections turn them on against fixtures.
#
# The indexer one is not merely untidy, and the reason is worth keeping even
# though the shape of it is gone. In the bash version $SELF was `readlink -f
# "$0"` and this suite SOURCED taimux, so $SELF here was THIS FILE: a background
# indexer fired from a row build re-ran the whole suite, which built more rows,
# which fired more indexers, each with its own mktemp runtime dir and so its own
# rate-limit stamp and no shared throttle. Exponential, and it took the machine
# down twice. A compiled implementation cannot make that mistake (`current_exe()`
# is the binary, whoever called it) and this suite sources nothing now, so these
# two exports are no longer a lock on that door: they are here so a run neither
# reads Patrick's real transcripts nor sets an indexer off against them.
export TAIMUX_SEARCH=0
export TAIMUX_SESSIONS=0

PASS=0; FAIL=0
ok() { PASS=$((PASS+1)); printf '  \033[32mok\033[0m   %s\n' "$1"; }
no() { FAIL=$((FAIL+1)); printf '  \033[31mFAIL\033[0m %s\n       %s\n' "$1" "$2"; }
eq()    { if [ "$2" = "$3" ]; then ok "$1"; else no "$1" "expected [$2] got [$3]"; fi; }
has()   { case "$2" in *"$3"*) ok "$1";; *) no "$1" "[$2] missing [$3]";; esac; }
hasnt() { case "$2" in *"$3"*) no "$1" "[$2] unexpectedly has [$3]";; *) ok "$1";; esac; }
# Counted as neither a pass nor a failure, and said out loud: a check that
# quietly does not run is worse than one that fails.
skip() { printf '  \033[33mskip\033[0m %s\n' "$1"; }
# Delete inside a fixture directory, refusing to do it with an empty base.
#
# `rm -f "$X"/*` is `rm -f /*` when $X is unset, and `rm -f "$X".*` globs the CWD:
# deleting a function this suite interpolated turned one of those into
# `rm -f "" ".*"` and it took this repo's .gitignore with it. One guard, used
# everywhere a fixture is emptied, so the next empty variable is a loud failure.
scrub() {   # $1 = the directory to empty
  case "${1:-}" in
    ''|/|.|..) no "scrub refused" "[${1:-empty}] is not a fixture directory"; return 1 ;;
  esac
  [ -d "$1" ] || return 0
  find "$1" -mindepth 1 -maxdepth 1 -exec rm -rf {} + 2>/dev/null
}
section() { printf '\n\033[1m%s\033[0m\n' "$1"; }

# ============================================================================
section "install and bind: against the real binary, in a fixture HOME"
# ============================================================================
# The whole of this drives taimux rather than sourcing anything, because there
# is nothing left to source. Two fixtures make that possible without touching
# Patrick's own config: a COPY of the binary at the path being tested (so
# `current_exe` is genuinely that path, which is what `install` reasons about),
# and a fake tmux on PATH that answers show-options out of two variables and logs
# everything else.
#
# The pure parts (choose_conf, strip_block, insert_block, plugin_checkout,
# popup_cmd) have their own tests in install.rs. What is here is what only an
# end-to-end run can check: which file gets written, what lands in it, what gets
# bound live, and that a plugin checkout is left alone.
IBIN="$HERE/../target/release/taimux"
if [ ! -x "$IBIN" ]; then
  skip "install and bind (no taimux built; run just daemon-build)"
else
  r=0; [ -x "$HERE/../taimux.tmux" ] || r=1
  eq  "taimux.tmux is executable, or no manager will run it" "0" "$r"
  has "and it hands off to its own checkout's binary" \
      "$(cat "$HERE/../taimux.tmux")" 'bind'

  IH="$TMP/ihome"; mkdir -p "$IH"
  IFT="$TMP/ift"; mkdir -p "$IFT"
  IBLOG="$TMP/ibinds"
  cat > "$IFT/tmux" <<'FAKEI'
#!/usr/bin/env bash
# show-options out of the two variables, everything else to the log. "unset"
# means the option was never set, which is different from set-to-empty: empty
# means "do not bind that key" and must not fall back to the default.
opt_of() {
  case "$1" in
    # ${X-unset}, NOT ${X:-unset}: the colon form substitutes for an EMPTY
    # value too, which is exactly the distinction being tested. This fake got it
    # wrong first and three checks failed on the fake rather than on the code.
    @taimux-key)      printf '%s\n' "${OPT_KEY-unset}" ;;
    @taimux-root-key) printf '%s\n' "${OPT_ROOT-unset}" ;;
    *)                 printf 'unset\n' ;;
  esac
}
case "${1:-} ${2:-}" in
  "show-options -g") exit 0 ;;                       # the "is there a server" probe
  "show-options -gq"|"show-options -gqv")
    v="$(opt_of "${3:-}")"
    [ "$v" = unset ] && exit 0                       # never set: print nothing
    case "$2" in -gq) printf "%s '%s'\n" "$3" "$v" ;; *) printf '%s\n' "$v" ;; esac
    exit 0 ;;
esac
printf '%s\n' "$*" >> "$IBLOG"
exit 0
FAKEI
  chmod +x "$IFT/tmux"
  # A copy at the path under test, so current_exe() really is that path.
  at() { mkdir -p "${1%/*}"; cp "$IBIN" "$1"; printf '%s' "$1"; }
  jm() { : > "$IBLOG"; PATH="$IFT:$PATH" HOME="$IH" IBLOG="$IBLOG" TMUX=fake "$@"; }

  # --- a plugin-manager checkout: the symlink, and no config write -----------
  # The symlink is what puts restart, resurrect and the ssh federation's `list`
  # on PATH; the bindings are taimux.tmux's job on every launch and reload, so
  # writing them to a config too would be a second, stale copy.
  PLUG="$(at "$IH/.tmux/plugins/taimux-9f2c1b/taimux")"
  OUT="$(OPT_KEY=unset OPT_ROOT=unset jm "$PLUG" install)"
  has "install from a plugin checkout writes no config" "$OUT" "owns the bindings"
  r=0; [ -L "$IH/.local/bin/taimux" ] || r=1
  eq  "but it still symlinks the launcher onto PATH"    "0" "$r"
  eq  "pointing at that checkout"      "$PLUG" "$(readlink "$IH/.local/bin/taimux")"
  r=0; [ -e "$IH/.tmux.conf" ] && r=1
  eq  "and creates no tmux config"     "0" "$r"
  B="$(cat "$IBLOG")"
  has "while still binding live"       "$B" "bind-key a if-shell"
  has "by absolute path, since a plugin cannot count on PATH" "$B" "$PLUG pick"

  # …by any of the three paths a manager-owned checkout can live under.
  XP="$(at "$IH/.config/tmux/plugins/taimux/taimux")"
  has "so is one under the XDG config dir" \
      "$(XDG_CONFIG_HOME="$IH/.config" OPT_KEY=unset OPT_ROOT=unset jm "$XP" install)" \
      "owns the bindings"
  RP="$(at "$IH/elsewhere/plugins/taimux/taimux")"
  has "and so is a relocated plugin path" \
      "$(TMUX_PLUGIN_MANAGER_PATH="$IH/elsewhere/plugins" OPT_KEY=unset OPT_ROOT=unset jm "$RP" install)" \
      "owns the bindings"

  # --- a plain checkout: the config block ------------------------------------
  PLAIN="$(at "$IH/workspaces/taimux/taimux")"
  printf '%s\n' "set -g mouse on" > "$IH/.tmux.conf"
  OPT_KEY=unset OPT_ROOT=unset jm "$PLAIN" install >/dev/null
  CONF="$(cat "$IH/.tmux.conf")"
  has "a plain checkout gets the prefix binding written" "$CONF" "bind-key a if-shell"
  has "and the no-prefix one"                            "$CONF" "bind-key -n F1 if-shell"
  has "spelled by bare name, since install symlinks it"  "$CONF" '"taimux pick'
  has "the popup is near-full width on a narrow client"  "$CONF" '-w 100% -h 90%'
  has "and 80% on a roomy one"                           "$CONF" '-w 80% -h 80%'
  has "and unrelated config is left alone"               "$CONF" "mouse on"

  # The two key options, honoured, including "set to empty means off". Running
  # install again also proves the block is STRIPPED rather than doubled: a
  # reinstall that left both would open the popup twice.
  OPT_KEY=M-j OPT_ROOT='' jm "$PLAIN" install >/dev/null
  CONF="$(cat "$IH/.tmux.conf")"
  has   "a custom prefix key is what lands in the config" "$CONF" "bind-key M-j if-shell"
  hasnt "the previous binding is stripped, not doubled"   "$CONF" "bind-key a"
  hasnt "and a root key set to empty binds nothing"       "$CONF" "bind-key -n"

  # --- bind on its own: the running server, and nothing written -------------
  OUT="$(OPT_KEY=unset OPT_ROOT=unset jm "$PLUG" bind)"
  B="$(cat "$IBLOG")"
  has "bind names the binary it ran from" "$OUT" "$PLUG"
  has "and binds the default prefix key"  "$B" "bind-key a if-shell"
  has "and the default root key"          "$B" "bind-key -n F1 if-shell"
  has "by that checkout's own path"       "$B" "$PLUG pick"
  OUT="$(OPT_KEY='' OPT_ROOT='' jm "$PLUG" bind)"
  has "with both options empty it says so" "$OUT" "nothing bound"
  eq  "and leaves the server untouched"    "" "$(cat "$IBLOG")"

  # --- outside tmux there is nothing to bind in ------------------------------
  r=0; PATH="$IFT:$PATH" HOME="$IH" env -u TMUX "$PLUG" bind >/dev/null 2>&1 || r=$?
  eq "bind still works without \$TMUX, since the server answers" "0" "$r"

  # --- install-hooks, over a real settings.json ------------------------------
  # jq does the merge, deliberately: settings.json is a file the AGENT rewrites
  # for itself, so a mangled one is a config lost.
  if command -v jq >/dev/null 2>&1; then
    export CLAUDE_CONFIG_DIR="$TMP/icfg"; mkdir -p "$CLAUDE_CONFIG_DIR"
    printf '%s\n' '{"theme":"dark","hooks":{"Stop":[{"hooks":[{"type":"command","command":"someone-elses-hook"}]}]}}' \
      > "$CLAUDE_CONFIG_DIR/settings.json"
    "$PLUG" install-hooks >/dev/null 2>&1
    SET="$(cat "$CLAUDE_CONFIG_DIR/settings.json")"
    hooked() { printf '%s' "$SET" | jq -r --arg e "$1" --arg c "$PLUG hook" \
      '[.hooks[$e][]?.hooks[]?.command] | map(select(. == $c)) | length'; }
    eq "SessionStart gets the hook"          "1" "$(hooked SessionStart)"
    eq "UserPromptSubmit gets the hook"      "1" "$(hooked UserPromptSubmit)"
    eq "Stop gets the hook"                  "1" "$(hooked Stop)"
    eq "PermissionRequest gets the hook"     "1" "$(hooked PermissionRequest)"
    eq "SessionEnd gets the hook"            "1" "$(hooked SessionEnd)"
    has "a hook already there is left alone" "$SET" "someone-elses-hook"
    has "and so is everything else"          "$SET" '"theme": "dark"'
    "$PLUG" install-hooks >/dev/null 2>&1
    SET="$(cat "$CLAUDE_CONFIG_DIR/settings.json")"
    eq "running it twice adds nothing twice"  "1" "$(hooked Stop)"
    # Not ours to repair, and replacing it would lose whatever is in there.
    printf 'not json at all\n' > "$CLAUDE_CONFIG_DIR/settings.json"
    "$PLUG" install-hooks >/dev/null 2>&1
    eq "an unparseable settings file is left as it was" "not json at all" \
       "$(cat "$CLAUDE_CONFIG_DIR/settings.json")"
    unset CLAUDE_CONFIG_DIR
  else
    skip "install-hooks (no jq)"
  fi
fi


# ============================================================================
section "switch and preview: the tmux command lines (fake tmux on PATH)"
# ============================================================================
# Both are taimux's now, so the fake tmux goes on PATH rather than being a
# shell function: a binary cannot see one of those. The assertions are unchanged,
# because the thing that can be wrong is the command LINE, not who runs it.
XBIN0="$HERE/../target/release/taimux"
if [ -x "$XBIN0" ]; then
  LOG="$TMP/tmux.log"
  FT="$TMP/ft"; mkdir -p "$FT"
  cat > "$FT/tmux" <<'FAKE'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$TMUXLOG"
case "$*" in
  *"#{session_name}:"*)      printf 'work:1.1   /home/u/proj/web\n' ;;
  *"#{session_name}"*)       printf 'fakesess\n' ;;
  *"#{window_zoomed_flag}"*) printf '0\n' ;;   # not zoomed -> switch should zoom
esac
case "$1" in
  capture-pane) printf '%s\n' "first line" "second line MARKER123" ;;
esac
exit 0
FAKE
  chmod +x "$FT/tmux"
  jd() { : > "$LOG"; PATH="$FT:$PATH" TMUXLOG="$LOG" "$XBIN0" "$@"; }

  jd switch "%42"; rc=$?
  SW="$(cat "$LOG")"
  eq  "switch returns 0"                     "0" "$rc"
  has "selects the window by pane id"        "$SW" "select-window -t %42"
  has "selects the pane by pane id"          "$SW" "select-pane -t %42"
  has "zooms the selected pane full-screen"  "$SW" "resize-pane -Z -t %42"
  has "switches client to its session"       "$SW" "switch-client -t fakesess"

  TAIMUX_ZOOM=0 jd switch "%55" >/dev/null
  hasnt "TAIMUX_ZOOM=0 disables the zoom"   "$(cat "$LOG")" "resize-pane"

  jd switch "" >/dev/null 2>&1; rc=$?
  eq  "empty id returns non-zero"            "1" "$rc"
  eq  "empty id issues no tmux commands"     ""  "$(cat "$LOG")"

  PV="$(jd preview %9)"
  has "preview includes the pane header"  "$PV" "work:1.1"
  has "preview includes captured content" "$PV" "MARKER123"
  # A remote id is not this binary's to render: capture-pane only works where the
  # pane is, and the ssh is still the script's.
  # A remote id is RENDERED now, by asking that host for its own pane: the ssh
  # moved in with the rest of federation. The fake tmux above has no ssh, so what
  # is asserted is that it tried rather than refused.
  PV="$(jd preview "ha:%6" 2>&1)"
  has "a remote id is asked of its own host" "$PV" "ha"
else
  skip "switch and preview (no taimux built; run just daemon-build)"
fi

# ============================================================================
section "search: what a transcript is boiled down to"
# ============================================================================
# The extractor is the whole of the flood control, so every exclusion gets a line
# in the fixture: what survives is what the picker can be searched on, and what
# does not is what would otherwise make every session match everything.
#
# It is taimux's now (436 of 436 real transcripts came out byte-identical
# before the bash one was deleted), and these assertions are kept BECAUSE the
# rules they pin were each found by looking at a real corpus. Driven through the
# binary rather than reimplemented in Rust so the fixture stays one file.
export TAIMUX_SEARCH=1
TR="$TMP/t/proj/aaaa.jsonl"; mkdir -p "${TR%/*}"
{
  printf '%s\n' '{"type":"user","message":{"role":"user","content":"please fix the worktree bug"},"cwd":"/w"}'
  printf '%s\n' '{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Looking at the WORKTREE now"}]}}'
  printf '%s\n' '{"type":"user","toolUseResult":{"n":1},"message":{"role":"user","content":[{"tool_use_id":"t1","type":"tool_result","content":"SECRETFILEBODY"}]}}'
  printf '%s\n' '{"type":"user","isMeta":true,"message":{"role":"user","content":"METACAVEAT"}}'
  printf '%s\n' '{"type":"assistant","isSidechain":true,"message":{"role":"assistant","content":[{"type":"text","text":"SUBAGENTPROSE"}]}}'
  printf '%s\n' '{"type":"attachment","attachment":{"type":"deferred_tools_delta","content":"EnterWorktree ExitWorktree CATALOGNOISE"}}'
  printf '%s\n' '{"type":"attachment","attachment":{"type":"file","filename":"/w/keepme.py","content":{"type":"text","file":{"filePath":"/w/keepme.py","content":"ATTACHEDFILEBODY"}}}}'
  printf '%s\n' '{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","name":"Bash","input":{"command":"echo TOOLINPUT"}}]}}'
  printf '%s\n' '{"type":"user","message":{"role":"user","content":"<system-reminder>NOISEONE</system-reminder>REALPROMPT<system-reminder>NOISETWO</system-reminder>"}}'
  printf '%s\n' '{"type":"user","message":{"role":"user","content":"line one\nline two\ttabbed"}}'
} > "$TR"
XBIN="$HERE/../target/release/taimux"
BLOB="$([ -x "$XBIN" ] && "$XBIN" extract "$TR")"

has   "a plain user turn is kept"                  "$BLOB" "please fix the worktree bug"
has   "an assistant text block is kept"            "$BLOB" "Looking at the WORKTREE now"
hasnt "a tool RESULT is not"                       "$BLOB" "SECRETFILEBODY"
hasnt "nor an isMeta caveat"                       "$BLOB" "METACAVEAT"
hasnt "nor a subagent's sidechain turn"            "$BLOB" "SUBAGENTPROSE"
hasnt "nor an injected attachment catalogue"       "$BLOB" "CATALOGNOISE"
hasnt "nor the body of an attached file"           "$BLOB" "ATTACHEDFILEBODY"
has   "…though its NAME is worth searching on"     "$BLOB" "keepme.py"
hasnt "nor a tool call's arguments"                "$BLOB" "TOOLINPUT"
# The reason untag() exists: a prompt can sit BETWEEN two reminders, and awk has
# no lazy quantifier, so a single greedy gsub would swallow the prompt with them.
has   "a prompt between two reminders survives"    "$BLOB" "REALPROMPT"
hasnt "…while the first reminder does not"         "$BLOB" "NOISEONE"
hasnt "…nor the second"                            "$BLOB" "NOISETWO"
has   "an escaped newline becomes a space"         "$BLOB" "line one line two"
eq    "the blob is one line"                       "1"  "$(printf '%s\n' "$BLOB" | wc -l)"
eq    "…with no tab in it to split a row on"       "0"  "$(printf '%s' "$BLOB" | tr -dc '\t' | wc -c)"

# ============================================================================
section "search: the per-pane index, and what it does not read twice"
# ============================================================================
IDX="$XDG_RUNTIME_DIR/taimux/index"
# Every one of these is about the INCREMENTAL behaviour, which is where a mistake
# is silent: a grown transcript re-read from the top costs a pass its whole point,
# and one appended to after a /clear puts two conversations in one blob.
ix() { "$XBIN" index-pane "$@"; }
ix %70 "$TR"
eq "the header names the pane and its transcript" "idx $(stat -c %s "$TR") %70 $TR" \
   "$(awk 'NR==1{print $1, $2, $4, $5}' "$IDX/_70")"
has "…and the blob is on the line under it" "$(sed -n 2p "$IDX/_70")" "worktree bug"

# A blob nobody has any business rewriting is the proof that nothing was read
# again: mark it, and see the mark survive.
sed -i '2s/^/MARKER /' "$IDX/_70"
ix %70 "$TR"
has "an unchanged transcript is not re-read" "$(sed -n 2p "$IDX/_70")" "MARKER"

printf '%s\n' '{"type":"user","message":{"role":"user","content":"and now about REBASING"}}' >> "$TR"
ix %70 "$TR"
BL="$(sed -n 2p "$IDX/_70")"
has "a grown transcript keeps what was already indexed" "$BL" "MARKER"
has "…and gains only what is new"                       "$BL" "REBASING"
eq  "…with the header moved on to the new size" "$(stat -c %s "$TR")" \
    "$(awk 'NR==1{print $2}' "$IDX/_70")"

# A transcript that SHRANK is not the same file any more, whatever its name says.
head -n 2 "$TR" > "$TR.short"; mv "$TR.short" "$TR"
ix %70 "$TR"
BL="$(sed -n 2p "$IDX/_70")"
hasnt "a shrunken transcript is indexed from scratch" "$BL" "MARKER"
has   "…and still has what is left of it"             "$BL" "worktree bug"

# A pane moved onto another conversation (a /clear) is a header naming a
# transcript that is no longer this pane's, so the blob is rebuilt rather than
# appended to: the one thing that must never happen is two sessions in one blob.
cp "$TR" "$TMP/t/proj/bbbb.jsonl"
printf '%s\n' '{"type":"user","message":{"role":"user","content":"OTHERCONVERSATION"}}' >> "$TMP/t/proj/bbbb.jsonl"
ix %70 "$TMP/t/proj/bbbb.jsonl"
BL="$(sed -n 2p "$IDX/_70")"
has   "a /clear onto a new transcript re-indexes" "$BL" "OTHERCONVERSATION"
eq    "…keyed to the transcript it is now on"     "$TMP/t/proj/bbbb.jsonl" \
      "$(awk 'NR==1{print $5}' "$IDX/_70")"

# The cap keeps the TAIL: these are live sessions, and the opening of one is
# already on its row, in the summary its title is made of.
rm -f "$IDX/_70"
TAIMUX_SEARCH_CAP=40 ix %70 "$TMP/t/proj/bbbb.jsonl"
BL="$(sed -n 2p "$IDX/_70")"
has "the cap keeps the end of the conversation" "$BL" "OTHERCONVERSATION"
if [ "${#BL}" -le 40 ]; then ok "…and honours the cap"; else no "…and honours the cap" "${#BL} > 40"; fi
case "$BL" in ' '*|'') no "…cutting on a word boundary" "[$BL]";; *) ok "…cutting on a word boundary";; esac

# The index is written HERE, in bash, and read by taimux: that format is a
# contract between two implementations, and the only test that means anything
# about it is one that crosses it. Everything above proved the writing; this
# proves the reading, on the file the writing just produced.
SBIN="$HERE/../target/release/taimux"
if [ -x "$SBIN" ]; then
  scrub "$IDX"; ix %70 "$TMP/t/proj/bbbb.jsonl"
  eq "the reader finds the pane the header names" "%70" \
     "$("$SBIN" snips OTHERCONVERSATION | awk -F'\t' '{print $1}')"
  has "…with the words it was asked for in the snippet" \
      "$("$SBIN" snips OTHERCONVERSATION)" "OTHERCONVERSATION"
  eq  "a term nothing said matches nothing" "" "$("$SBIN" snips zzzznothing)"
  # Every term has to be in there, the same rule the row is kept by: a session
  # that mentions one of two words is not a hit.
  eq  "…and so does a term only half present" "" \
      "$("$SBIN" snips OTHERCONVERSATION zzzznothing)"
else
  skip "index format contract (no taimux built; run just daemon-build)"
fi

# ============================================================================
section "ended: what one conversation says about itself"
# ============================================================================
export TAIMUX_SEARCH=1
FC="$TMP/fakeclaude"; mkdir -p "$FC/projects/p1" "$FC/projects/p2"
_claude_dir() { printf '%s\n' "$FC"; }
export CLAUDE_CONFIG_DIR="$FC"          # …and the same for taimux
# Four of the five agents keep their history under $HOME rather than behind a
# variable of their own, so the fixture only means anything with HOME pointed
# somewhere empty. Without this the suite read the DEVELOPER'S history: 254 real
# conversations arrived in a section asserting there were five.
REALHOME="$HOME"; export HOME="$TMP/fakehome"; mkdir -p "$HOME"
# session_meta is taimux's now. Read BACKWARDS and stopped as soon as it has
# all three, which is what makes hundreds of conversations affordable, so these
# assertions are about what it stops on rather than about parsing.
sm() { "$XBIN" session-meta "$@"; }
mk() {   # $1 = file, $2… = records
  local f="$1"; shift; mkdir -p "${f%/*}"; printf '%s\n' "$@" > "$f"
}

A="$FC/projects/p1/aaaa.jsonl"
mk "$A" \
  '{"type":"user","message":{"role":"user","content":"start the thing"},"cwd":"/w/one","version":"2.1.1"}' \
  '{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"done"}]},"cwd":"/w/one","version":"2.1.9"}' \
  '{"type":"ai-title","aiTitle":"the bare title"}' \
  '{"type":"custom-title","customTitle":"proj: the prefixed title"}'
IFS=$'\t' read -r M_CWD M_VER M_TTL M_SRC < <(sm "$A")
eq "the cwd is read out of it"                  "/w/one"                   "$M_CWD"
# Backwards, so the version is the one it was LAST running, not the one it started
# under: a session that lived across a self-update ran on both.
eq "…and the version it last ran under"         "2.1.9"                    "$M_VER"
eq "…and the custom title beats the ai one"     "proj: the prefixed title" "$M_TTL"
eq "…and it is marked as a real title"          "t" "$M_SRC"

B="$FC/projects/p1/bbbb.jsonl"
mk "$B" '{"type":"user","message":{"role":"user","content":"hi"},"cwd":"/w/two","version":"2.1.1"}' \
        '{"type":"ai-title","aiTitle":"only an ai title"}'
eq "an ai title stands in when there is no custom one" "only an ai title" \
   "$(sm "$B" | cut -f3)"

# A session can end before it was ever titled. The prompt it was last given
# identifies it far better than nothing at all.
C="$FC/projects/p1/cccc.jsonl"
mk "$C" '{"type":"user","message":{"role":"user","content":"x"},"cwd":"/w/three","version":"2.1.1"}' \
        '{"type":"last-prompt","lastPrompt":"the thing I asked it\nover two lines"}'
eq "an untitled session falls back to its last prompt" "the thing I asked it over two lines" \
   "$(sm "$C" | cut -f3)"
eq "…and that stand-in is marked as one, not as a title" "p" \
   "$(sm "$C" | cut -f4)"
D="$FC/projects/p1/dddd.jsonl"
mk "$D" '{"type":"user","message":{"role":"user","content":"x"},"cwd":"/w/four","version":"2.1.1"}' \
        "{\"type\":\"last-prompt\",\"lastPrompt\":\"$(printf 'y%.0s' $(seq 1 200))\"}"
eq "…and a pasted one is cut to something a row can hold" "80" \
   "$(printf '%s' "$(sm "$D" | cut -f3)" | wc -m)"
E="$FC/projects/p1/eeee.jsonl"
mk "$E" '{"type":"user","message":{"role":"user","content":"x"},"cwd":"/w/five","version":"2.1.1"}'
eq "a session with nothing to call it says nothing" "" "$(sm "$E" | cut -f3)"

# ============================================================================
section "ended: the cache of every conversation, and what is still open"
# ============================================================================
touch -d '2026-01-01 10:00' "$A"; touch -d '2026-01-01 09:00' "$B"
touch -d '2026-01-01 08:00' "$C"; touch -d '2026-01-01 07:00' "$D"
touch -d '2026-01-01 06:00' "$E"
SF="$XDG_RUNTIME_DIR/taimux/sessions"
# The live map goes in on stdin, as it always did: resolving which pane is on
# which conversation is the expensive part of a pass, so it is done once by the
# caller and handed in.
scan() { "$XBIN" index-sessions; }

printf '%%9\t%s\n' "$B" | scan
eq "the header says when it was taken"  "sess" "$(awk 'NR==1{print $1}' "$SF")"
eq "one line per conversation"          "5"    "$(( $(wc -l < "$SF") - 1 ))"
eq "newest first"                       "$A"   "$(awk -F'\t' 'NR==2{print $4}' "$SF")"
eq "a conversation a pane is on carries that pane" "%9" \
   "$(awk -F'\t' -v p="$B" '$4==p{print $2}' "$SF")"
eq "…and one nothing is on carries a dash"         "-" \
   "$(awk -F'\t' -v p="$A" '$4==p{print $2}' "$SF")"
eq "which agent wrote it is on the line"  "claude" "$(awk -F'\t' -v p="$A" '$4==p{print $3}' "$SF")"
eq "the metadata rides along"           "/w/one" "$(awk -F'\t' -v p="$A" '$4==p{print $5}' "$SF")"

# Re-reading half a gigabyte on every pass is the thing this must not do, so a
# line whose transcript has not moved is reused verbatim. Marked, then checked.
sed -i "s|\t/w/one\t|\tMARKER\t|" "$SF"
printf '%%9\t%s\n' "$B" | scan
eq "an untouched conversation is not read again" "MARKER" \
   "$(awk -F'\t' -v p="$A" '$4==p{print $5}' "$SF")"
touch -d '2026-01-01 11:00' "$A"
printf '%%9\t%s\n' "$B" | scan
eq "…and one that has said something since, is"  "/w/one" \
   "$(awk -F'\t' -v p="$A" '$4==p{print $5}' "$SF")"

# There is no cap by default any more: the bound used to be the newest 200, and
# what it actually cost was the conversations you go looking for. The knob stays
# for a machine that wants one back, and still takes the newest.
TAIMUX_SESSIONS_MAX=2 scan </dev/null
eq "the cap keeps the newest and drops the rest" "2" "$(( $(wc -l < "$SF") - 1 ))"
eq "…keeping the newest of them"                 "$A" "$(awk -F'\t' 'NR==2{print $4}' "$SF")"
printf '%%9\t%s\n' "$B" | scan     # put it back for the rows below

# ============================================================================
section "ended: the rows, and the three keys that meet them"
# ============================================================================
# Built by taimux off the sessions cache the section above just wrote: the bash
# `_dead_rows` went with the fzf picker, and this is the same eight fields. Skipped
# rather than silently passing where nothing has been built.
DBIN="$HERE/../target/release/taimux"
DR="$([ -x "$DBIN" ] && "$DBIN" dead-rows)"
eq "only the ones nothing is running"  "4" "$(printf '%s\n' "$DR" | grep -c .)"
hasnt "…so the one on a pane is left out" "$DR" "$B"
has   "the id names the agent and the transcript" "$DR" "dead:claude:$A"
eq "the label column is how long ago it stopped, not a pane" "1" \
   "$(printf '%s\n' "$DR" | awk -F'\t' '$1=="dead:claude:'"$A"'" && $2 ~ /^[0-9]+[mhd]$/ {print 1}')"
eq "the state marks them as ended"       "dead" \
   "$(printf '%s\n' "$DR" | awk -F'\t' '$1=="dead:claude:'"$A"'"{print $6}')"
eq "an untitled one still says something" "the thing I asked it over two lines" \
   "$(printf '%s\n' "$DR" | awk -F'\t' '$1=="dead:claude:'"$C"'"{print $8}')"

# Nothing scanned yet is a real state: the indexer runs behind the picker, so the
# first time this mode is opened after a boot it can genuinely have nothing.
mv "$SF" "$SF.aside"
has "an unscanned box says so rather than looking empty" \
    "$([ -x "$DBIN" ] && "$DBIN" dead-rows)" "still reading"
mv "$SF.aside" "$SF"

# Enter is the key that acts on these rows, and what it builds is a resume in the
# directory the conversation ran in. taimux runs the real tmux, so the stub goes
# on PATH rather than being a shell function: a binary cannot see one of those,
# and putting it here rather than mocking the call is what keeps the assertion
# about the command LINE, which is the part that can be wrong.
FAKETMUX="$TMP/faketmux"; mkdir -p "$FAKETMUX"
TMUXLOG="$TMP/tmux.log"
cat > "$FAKETMUX/tmux" <<'FAKED'
#!/usr/bin/env bash
printf 'tmux %s\n' "$*" >> "$TMUXLOG"
case "$*" in
  *"#{session_name}"*)       printf 'fakesess\n' ;;
  *"#{window_zoomed_flag}"*) printf '0\n' ;;
esac
exit 0
FAKED
chmod +x "$FAKETMUX/tmux"
mkdir -p /tmp/muxhop-w-one 2>/dev/null
# The cwd comes off the sessions cache now, not from a stubbed lookup, so the
# cache is pointed at a directory that exists.
sed -i "s|\t/w/one\t|\t/tmp/muxhop-w-one\t|" "$SF"
# The exit code is the binary's, not `cat`'s: a refusal is asserted on both the
# status and the message, and piping through cat would have hidden every one.
sw() {
  : > "$TMUXLOG"
  local out rc=0
  out="$(PATH="$FAKETMUX:$PATH" TMUXLOG="$TMUXLOG" "$XBIN" switch "$@" 2>&1)" || rc=$?
  printf '%s\n%s' "$out" "$(cat "$TMUXLOG")"
  return "$rc"
}
OUT="$(sw "dead:claude:$A")"
has "enter resumes it in a new window"  "$OUT" "new-window -c /tmp/muxhop-w-one"
has "…on the transcript itself"         "$OUT" "--resume $A"
has '…through `command`, so no alias fires and resurrect can see it' \
    "$OUT" "command claude"
# It refuses rather than falling back to $HOME: a session resumed in the wrong
# directory writes its history into a different project, silently.
sed -i "s|\t/tmp/muxhop-w-one\t|\t/nowhere/at/all\t|" "$SF"
r=0; out="$(sw "dead:claude:$A")" || r=1
eq "a directory that has gone is a refusal" "1" "$r"
has "…and it says which"                    "$out" "/nowhere/at/all"
hasnt "…and no window is opened"            "$out" "new-window"
r=0; out="$(sw "dead:claude:$FC/projects/p1/never.jsonl")" || r=1
eq "so is a transcript that has gone"       "1" "$r"
has "…and it says that too"                 "$out" "no longer on disk"
# A live pane is moved to rather than resumed, and the zoom is a TOGGLE, so a
# pane already zoomed must be left alone by the very key that asked for it.
OUT="$(sw %7)"
has "a live pane is selected and switched to" "$OUT" "select-pane -t %7"
has "…and the client follows it"              "$OUT" "switch-client"
OUT="$(TAIMUX_ZOOM=0 sw %7)"
hasnt "TAIMUX_ZOOM=0 leaves the zoom alone"  "$OUT" "resize-pane -Z"

# ============================================================================
section "ended: what the index is allowed to forget"
# ============================================================================
IDX="$XDG_RUNTIME_DIR/taimux/index"
scrub "$IDX"; mkdir -p "$IDX"
NOW="$(date +%s)"
idxfile() { printf 'idx 0 %s %s %s\n%s\n' "$2" "$1" "${3:--}" "blob" > "$IDX/$(printf '%s' "$1" | tr -c 'A-Za-z0-9' '_')"; }
idxfile "%9"        "$NOW"                    # a live pane, per the cache above
idxfile "dead:claude:$A" "$NOW"                    # an ended one the cache still knows
idxfile "%77"       "$NOW"                    # a pane that has since closed
idxfile "ha:%6"     "$NOW"                    # another host, fresh
idxfile "old:%6"    "$(( NOW - 200000 ))"     # another host, long silent
"$XBIN" index-prune
present() { [ -e "$IDX/$1" ] && printf 'yes\n' || printf 'no\n'; }
eq "a live pane keeps its blob"                  "yes" "$(present '_9')"
# Age is the wrong test for an ended one: its blob is written once and never
# touched, so any age rule would throw it away and re-read the transcript to get
# it back. Membership of the cache is the honest question.
eq "so does an ended session the cache tracks"   "yes" "$(present "$(printf '%s' "dead:claude:$A" | tr -c 'A-Za-z0-9' '_')")"
eq "a pane that closed does not"                 "no"  "$(present '_77')"
eq "another host's blob is kept while it answers" "yes" "$(present 'ha__6')"
eq "…and expires once it stops"                  "no"  "$(present 'old__6')"

# ============================================================================
section "past: the four agents that are not claude"
# ============================================================================
# Each keeps its history somewhere else and in its own shape, and all four are
# under $HOME, which the fixture already owns. OpenCode is the exception and is
# not here: it is a SQLite database, so there is nothing to write with printf.
mkg() { mkdir -p "${1%/*}"; printf '%s\n' "${@:2}" > "$1"; }

# Gemini: a chat log per session, and a project directory named by the SHA-256
# of the path it was opened in. The hash is the whole reason taimux carries one.
# The system's sha256, deliberately: taimux carries its own implementation, and
# this fixture only resolves if the two agree.
GHASH="$(printf '%s' /w/gem | sha256sum | cut -d' ' -f1)"
mkg "$HOME/.gemini/projects.json" '{ "projects": { "/w/gem": "gemlabel" } }'
mkdir -p /w 2>/dev/null || true
GS="$HOME/.gemini/tmp/$GHASH/chats/session-2026-01-01T00-00-abcd1234.jsonl"
mkg "$GS" \
  "{\"sessionId\":\"g1\",\"projectHash\":\"$GHASH\"}" \
  '{"id":"m1","type":"user","content":"the gemini question"}' \
  '{"id":"m2","type":"gemini","content":[{"text":"the gemini answer"}]}'

# …and the legacy shape: ONE pretty-printed object with a messages array, which
# is most of the sessions on a machine that has had Gemini a while.
GL="$HOME/.gemini/tmp/gemlabel/chats/session-2026-01-02T00-00-beef5678.json"
mkdir -p "${GL%/*}"
cat > "$GL" <<'GEMJSON'
{
  "sessionId": "g2",
  "messages": [
    { "id": "n1", "type": "user", "content": [ { "text": "the legacy question" } ] }
  ]
}
GEMJSON

# Antigravity: a transcript per conversation, with the directory in a separate
# history file keyed by the conversation id, which is also the directory name.
AS="$HOME/.gemini/antigravity-cli/brain/conv-777/.system_generated/logs/transcript.jsonl"
mkdir -p /tmp/muxhop-agy /tmp/muxhop-codex 2>/dev/null
mkg "$HOME/.gemini/antigravity-cli/history.jsonl" \
  '{"conversationId":"conv-777","workspace":"/tmp/muxhop-agy"}'
mkg "$AS" \
  '{"type":"USER_INPUT","content":"<USER_REQUEST>the agy question</USER_REQUEST><ADDITIONAL_METADATA>noise</ADDITIONAL_METADATA>"}' \
  '{"type":"PLANNER_RESPONSE","content":"the agy answer"}'

# Codex: a rollout, in the newer of its two schemas.
CS="$HOME/.codex/sessions/2026/01/rollout-2026-01-03-11111111-2222-3333-4444-555555555555.jsonl"
mkg "$CS" \
  '{"type":"session_meta","payload":{"id":"11111111-2222-3333-4444-555555555555","cwd":"/tmp/muxhop-codex"}}' \
  '{"type":"event_msg","payload":{"type":"user_message","message":"the codex question"}}'

scan </dev/null
PR="$("$XBIN" dead-rows)"
agentof() { printf '%s\n' "$PR" | awk -F'\t' -v k="$1" '$1==k{print $4}'; }
titleof() { printf '%s\n' "$PR" | awk -F'\t' -v k="$1" '$1==k{print $8}'; }
cwdof()   { printf '%s\n' "$PR" | awk -F'\t' -v k="$1" '$1==k{print $3}'; }

eq "a gemini chat is listed as gemini"  "gemini" "$(agentof "dead:gemini:$GS")"
eq "…titled by what it was first asked" "the gemini question" "$(titleof "dead:gemini:$GS")"
# The directory is not in the chat log at all: it is the SHA-256 in the path.
eq "…in the directory its hash decodes to" "/w/gem" "$(cwdof "dead:gemini:$GS")"
eq "the legacy layout reads the same"   "the legacy question" "$(titleof "dead:gemini:$GL")"
eq "an agy transcript is listed as agy" "agy" "$(agentof "dead:agy:$AS")"
eq "…with the metadata block left off"  "the agy question" "$(titleof "dead:agy:$AS")"
eq "…and the directory from its history file" "/tmp/muxhop-agy" "$(cwdof "dead:agy:$AS")"
eq "a codex rollout is listed as codex" "codex" "$(agentof "dead:codex:$CS")"
eq "…with the cwd off its session record" "/tmp/muxhop-codex" "$(cwdof "dead:codex:$CS")"

# Enter resumes each in ITS OWN tool, and the one tool that cannot be told which
# session to resume says so rather than opening the wrong one.
OUT="$(sw "dead:agy:$AS")"
has "agy resumes by conversation id"  "$OUT" "command agy --conversation conv-777"
OUT="$(sw "dead:codex:$CS")"
has "codex resumes by session id"     "$OUT" \
    "command codex resume 11111111-2222-3333-4444-555555555555"
# Gemini keeps its id in the chat log's header; the filename carries only the
# first eight characters of it.
mkdir -p /tmp/muxhop-gem 2>/dev/null
sed -i "s|\t/w/gem\t|\t/tmp/muxhop-gem\t|" "$SF"
OUT="$(sw "dead:gemini:$GS")"
has "gemini resumes by the id in its header" "$OUT" "command gemini --resume g1"

# ============================================================================
section "past: the handoff, which is the other thing to do with a conversation"
# ============================================================================
HO="$("$XBIN" handoff "dead:agy:$AS")"
has "it leads with the instruction"   "$HO" "Continue this work."
has "…naming the tool it came from"   "$HO" "an Antigravity session"
has "…and the directory to work in"   "$HO" "Work in: /tmp/muxhop-agy"
has "the task is what it was asked"   "$HO" "the agy question"
has "both sides of the conversation are carried" "$HO" "User: the agy question"
has "…including what the agent said"  "$HO" "Antigravity: the agy answer"
has "…and a pointer to the whole transcript" "$HO" "Full session transcript: $AS"

# Every target takes its prompt differently, and two of them would otherwise run
# it non-interactively, which is the opposite of what a handoff is for.
has "claude takes a bare argument" \
    "$("$XBIN" handoff "dead:agy:$AS" --to claude --print)" "command claude 'Continue this work."
has "opencode is started interactively, not with \`run\`" \
    "$("$XBIN" handoff "dead:agy:$AS" --to opencode --print)" "command opencode --prompt "
has "agy takes -i, which is its interactive prompt" \
    "$("$XBIN" handoff "dead:agy:$AS" --to agy --print)" "command agy -i "
r=0; out="$("$XBIN" handoff "dead:agy:$AS" --to nosuchtool --print 2>&1)" || r=1
eq "a target taimux cannot start is a refusal" "1" "$r"

export TAIMUX_SEARCH=0
unset -f _claude_dir
export HOME="$REALHOME"

# ============================================================================
section "conversation: claude attach, which names a session outright"
# ============================================================================
FC2="$TMP/fc2"; mkdir -p "$FC2/projects/pA" "$FC2/projects/pB"
_claude_dir() { printf '%s\n' "$FC2"; }
export CLAUDE_CONFIG_DIR="$FC2"
# The resolver is taimux's. `resolve` prints "<transcript>\t<why>" and exits
# non-zero when no rung answers, which is what $R_TRANSCRIPT / $R_WHY were.
tbi() { "$XBIN" transcript-by-id "$@"; }
rfp() { "$XBIN" resolve "$@"; }
: > "$FC2/projects/pA/263946b5-9bd7-493d-8be1-10b0c180dd34.jsonl"
: > "$FC2/projects/pB/abcd1234-0000-4000-8000-000000000000.jsonl"
: > "$FC2/projects/pB/abcd1234-1111-4000-8000-000000000000.jsonl"

eq "a full id finds its transcript" "$FC2/projects/pA/263946b5-9bd7-493d-8be1-10b0c180dd34.jsonl" \
   "$(tbi 263946b5-9bd7-493d-8be1-10b0c180dd34)"
eq "…and so does the prefix claude asks you to type" \
   "$FC2/projects/pA/263946b5-9bd7-493d-8be1-10b0c180dd34.jsonl" "$(tbi 263946b5)"
# A prefix is all claude asks for, so it can be short enough to be ambiguous, and
# resuming the wrong conversation is what this whole ladder exists to avoid.
eq "an ambiguous prefix is refused, not guessed" "" "$(tbi abcd1234)"
eq "…and one that names nothing is too"          "" "$(tbi ffffffff)"
eq "a path is not an id"                         "" "$(tbi /etc/passwd)"

# That a pane in one directory can resolve to a conversation in another, and that
# `attach` counts only in the subcommand slot, are asserted in conv.rs: both need
# a stubbed argv, and a compiled resolver reads /proc instead. The replay below is
# still bash, so it stays here.
mkdir -p "$FC2/projects/$(printf '%s' "$HOME" | sed 's/[^A-Za-z0-9]/-/g')"
# The replay is asserted in conv.rs too, for the same reason: it needs a stubbed
# argv, which is `/proc` now. What is left here is the format contract between the
# two, which only an end-to-end run can check.
unset -f _claude_dir

# ============================================================================
section "the wire format: what another host answers with over ssh"
# ============================================================================
# `taimux list` is what a remote picker asks for, so it has to work with NOTHING
# else running: a host is not required to have a daemon up, and for most of them
# none ever will be.
#
# This broke silently once and is the reason the section exists. `list` was left
# meaning "ask a running daemon", which exits 1 where none is listening, so every
# remote host answered nothing and simply vanished from the list. No error
# anywhere: a host that answers nothing is indistinguishable from one with no
# sessions.
WBIN="$HERE/../target/release/taimux"
if [ -x "$WBIN" ]; then
  # A socket path nothing is listening on, which is the state on every host that
  # has never run a daemon.
  export TAIMUX_SOCKET="$TMP/no-such-socket"
  r=0; OUT="$("$WBIN" list 2>&1)" || r=$?
  eq "list answers with no daemon listening"    "0" "$r"
  # Eight fields, or the reader on the other side shifts every value after the
  # empty one. Only checked when this machine actually has an agent pane.
  if [ -n "$OUT" ]; then
    eq "…in the eight fields the picker reads" "8" \
       "$(printf '%s' "$OUT" | awk -F'\t' '{print NF; exit}')"
    hasnt "…and never a remote row of its own"  "$OUT" ":%"
  else
    skip "no agent panes on this machine to shape-check"
  fi
  # And it is the same answer either way, which is what lets the daemon be asked
  # first when there is one.
  eq "list-local is the same answer, daemon or not" "$OUT" "$("$WBIN" list-local 2>&1)"

  # …and now with one actually LISTENING, which is the half the assertions above
  # cannot see: they point at a dead socket, so both sides of them take the
  # in-process path and the socket's own answer is never read. It carried a
  # trailing blank line for as long as it existed, a row of no fields handed to
  # whatever parses the wire format, and nothing here could tell.
  #
  # The shape is asserted rather than the bytes: two scans taken ~100ms apart on
  # a live machine legitimately differ the moment any session changes state,
  # which would make a byte comparison flaky for a reason that is not a bug.
  export TAIMUX_SOCKET="$TMP/live-socket"
  "$WBIN" serve & DPID=$!
  n=0; while [ "$n" -lt 40 ] && [ ! -S "$TAIMUX_SOCKET" ]; do n=$((n+1)); sleep 0.05; done
  if [ -S "$TAIMUX_SOCKET" ]; then
    eq "the daemon answers ping"               "pong" "$("$WBIN" ping 2>&1)"
    DOUT="$("$WBIN" list 2>&1)"
    # Both of these need a machine that HAS an agent pane, and a CI runner does
    # not. `$(...)` strips the trailing newlines, so an empty answer and a
    # one-blank-line answer are the same empty string by the time they get here,
    # and the bug cannot show on a machine with nothing to list. Asserting it
    # anyway is how this went red on CI and green on a workstation: `printf
    # '%s\n' ""` still prints a line, which awk counts.
    if [ -n "$DOUT" ]; then
      eq "…and its rows carry no blank line"   "0" \
         "$(printf '%s\n' "$DOUT" | awk 'NF==0' | wc -l | tr -d ' ')"
      eq "…in the same eight fields"           "8" \
         "$(printf '%s' "$DOUT" | awk -F'\t' '{print NF; exit}')"
    else
      skip "no agent panes here to shape-check the daemon's answer"
    fi
    # The version refusal (a daemon left running by an older build answers rows
    # that look perfectly right) is driven in the crate's own tests, against a
    # fake listener: the alternative here is keeping an old binary around to run.
    kill "$DPID" 2>/dev/null; wait "$DPID" 2>/dev/null
    rm -f "$TAIMUX_SOCKET"
  else
    kill "$DPID" 2>/dev/null
    skip "a live daemon (the socket never appeared)"
  fi
  unset TAIMUX_SOCKET
  export TAIMUX_SOCKET="$TMP/no-such-socket"

  # The SECOND wire format, and it failed the same silent way for longer.
  #
  # index_fetch asked every host for `_index --dump`, a bash-era name no Rust
  # build ever implemented, so each one answered `no such command` and exited 1.
  # Its caller reads a non-zero rc as "an old host with nothing to say" and
  # returns, so remote index federation fetched nothing from anywhere and said
  # nothing about it. Both halves are asserted here because neither alone is
  # enough: the name has to be one the dispatcher answers, AND the one the
  # caller actually sends.
  r=0; "$WBIN" index-dump >/dev/null 2>&1 || r=$?
  eq "index-dump is a command the dispatcher answers" "0" "$r"
  r=0; "$WBIN" _index --dump >/dev/null 2>&1 || r=$?
  eq "…and the bash-era _index is refused, as it always was" "1" "$r"
  # cli/, not daemon/: the file moved in the workspace split and this path was
  # not followed, so `cat` failed and the assertion has been reading an EMPTY
  # string ever since, which contains nothing and therefore passes whatever the
  # code says. A test that cannot fail is worth less than no test, because it is
  # counted.
  hasnt "…and nothing still sends the old name" \
        "$(cat "$HERE/../cli/src/remote.rs")" "_index --dump"

  unset TAIMUX_SOCKET
else
  skip "the wire format (no taimux built; run just build)"
fi

# ============================================================================
section "indexer: one pass at a time, and not too often"
# ============================================================================
# The properties that matter here are the two the bash version got wrong, and
# both cost Patrick a reboot: two passes overlapping (a lock DIRECTORY plus a
# steal-it-if-old rule), and the rate limit failing to fire (a separate stamp
# file, which every generation of the fork bomb got a fresh copy of).
IXBIN="$HERE/../target/release/taimux"
if [ -x "$IXBIN" ]; then
  IXD="$TMP/ixrun"; mkdir -p "$IXD"
  # An empty corpus, so a pass is instant and asserts nothing about this machine.
  export CLAUDE_CONFIG_DIR="$TMP/ixclaude"; mkdir -p "$CLAUDE_CONFIG_DIR/projects"
  # The suite turns both features off at the top (that is half of what stopped the
  # fork bomb), and with both off a pass is correctly a no-op, so they go back on
  # for this section only.
  ix_run() { XDG_RUNTIME_DIR="$IXD" TAIMUX_SEARCH=1 TAIMUX_SESSIONS=1 "$IXBIN" index "$@"; }
  ix_run
  eq "a pass leaves a lock file behind, not a lock directory" "file" \
     "$([ -f "$IXD/taimux/index/.lock" ] && printf file; [ -d "$IXD/taimux/index/.lock" ] && printf dir)"
  eq "…and no separate stamp file to lose" "0" \
     "$([ -e "$IXD/taimux/index/.stamp" ] && printf 1 || printf 0)"
  # Held for the WHOLE pass: a second holder is refused rather than queued, since
  # an indexer that cannot get the lock has nothing left to do.
  if command -v flock >/dev/null 2>&1; then
    r=0; flock -n "$IXD/taimux/index/.lock" -c true || r=1
    eq "the lock is free once the pass is over" "0" "$r"
    r=0; flock "$IXD/taimux/index/.lock" -c 'flock -n '"$IXD"'/taimux/index/.lock -c true' || r=1
    eq "…and admits exactly one holder while it is held" "1" "$r"
  fi
  # The rate limit is that same file's mtime, so it cannot be defeated by a fresh
  # runtime directory the way a separate stamp was.
  touch "$IXD/taimux/marker"
  TAIMUX_SEARCH_TTL=3600 ix_run
  eq "a pass that has just run is skipped" "0" \
     "$(find "$IXD/taimux/index/.lock" -newer "$IXD/taimux/marker" 2>/dev/null | wc -l)"
  TAIMUX_SEARCH_TTL=3600 ix_run --force
  eq "…unless it is forced"                "1" \
     "$(find "$IXD/taimux/index/.lock" -newer "$IXD/taimux/marker" 2>/dev/null | wc -l)"
  # Turning both features off is a no-op rather than an empty cache.
  rm -rf "$IXD/taimux"
  XDG_RUNTIME_DIR="$IXD" TAIMUX_SEARCH=0 TAIMUX_SESSIONS=0 "$IXBIN" index
  eq "with both off it does nothing at all" "0" \
     "$([ -d "$IXD/taimux/index" ] && printf 1 || printf 0)"
  unset CLAUDE_CONFIG_DIR
else
  skip "indexer locking (no taimux built; run just daemon-build)"
fi

# ============================================================================
section "release: the downloader every host installs through"
# ============================================================================
# scripts/install-release.sh is how a host gets taimux: it reads a GitHub
# release, takes the statically linked asset out of it, and installs the binary
# plus the launcher symlink. Two of Patrick's hosts have no cargo, so this is not
# a convenience, it is the install path.
#
# All of it runs here with no network and no GitHub: TAIMUX_RELEASE_URL points
# at a fixture JSON on disk whose asset url is a `file://` too, and curl treats
# the two schemes the same. What that buys is the branches that only ever fire
# when something is wrong, which are exactly the ones nobody exercises by hand.
IRS="$HERE/../scripts/install-release.sh"
if [ ! -x "$IRS" ]; then
  skip "release downloader (scripts/install-release.sh missing)"
else
  RD="$TMP/rel"; mkdir -p "$RD"

  # A stand-in for the artefact: all the installer asks of it is that it runs
  # and says which version it is.
  fake_bin() {   # $1 = path, $2 = version it claims
    printf '#!/bin/sh\n[ "$1" = version ] && echo "taimux %s"\n' "$2" > "$1"
    chmod +x "$1"
  }
  # A release, in the shape the API answers with. tag_name and the asset's url
  # are the only two fields the installer reads.
  fake_release() {   # $1 = json path, $2 = tag, $3 = asset name, $4 = asset path
    cat > "$1" <<JSON
{ "tag_name": "$2",
  "assets": [ { "name": "$3", "url": "file://$4" } ] }
JSON
  }

  r=0; OUT="$("$IRS" --help 2>&1)" || r=$?
  eq  "--help exits 0"            "0" "$r"
  has "…and says what it takes"   "$OUT" "--force"
  r=0; "$IRS" --nonsense >/dev/null 2>&1 || r=$?
  eq  "an unknown option is refused, not ignored" "2" "$r"

  # The contract between the two files: the installer decides whether to
  # download by comparing the release's tag with field 2 of `taimux version`.
  # Change that output and every host silently re-downloads on every apply, or
  # worse, never upgrades. Nothing else would notice.
  if [ -x "$IBIN" ]; then
    eq "version answers in two fields" "2" "$("$IBIN" version | awk '{print NF}')"
    eq "…the first naming the tool"    "taimux" "$("$IBIN" version | awk '{print $1}')"
    r=0; printf '%s' "$("$IBIN" version | awk '{print $2}')" |
      grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+' || r=1
    eq "…the second a semantic version" "0" "$r"
  else
    skip "the version contract (no taimux built; run just build)"
  fi

  # The ordinary case, end to end.
  fake_bin "$RD/artefact" "1.2.3"
  fake_release "$RD/release.json" "v1.2.3" "taimux-x86_64-linux-musl" "$RD/artefact"
  D="$RD/bin"
  r=0
  OUT="$(TAIMUX_RELEASE_URL="file://$RD/release.json" "$IRS" --dir "$D" 2>&1)" || r=$?
  eq  "a release installs"                 "0" "$r"
  has "…saying which version arrived"      "$OUT" "1.2.3"
  r=0; [ -x "$D/taimux" ] || r=1
  eq  "…as an executable binary"           "0" "$r"
  eq  "…and it answers as that version"    "taimux 1.2.3" "$("$D/taimux" version)"

  # Idempotent, and it must SAY so rather than silently re-downloading: this
  # runs on every `chezmoi apply`.
  BEFORE="$(stat -c %i "$D/taimux")"
  OUT="$(TAIMUX_RELEASE_URL="file://$RD/release.json" "$IRS" --dir "$D" 2>&1)"
  has "a version already installed is left alone" "$OUT" "already installed"
  eq  "…the same file, not a fresh copy of it"    "$BEFORE" "$(stat -c %i "$D/taimux")"
  OUT="$(TAIMUX_RELEASE_URL="file://$RD/release.json" "$IRS" --dir "$D" --force 2>&1)"
  has "…unless it is forced"                      "$OUT" "fetching"

  # An upgrade, which is the case that has to keep the launcher working.
  fake_bin "$RD/artefact2" "1.3.0"
  fake_release "$RD/rel2.json" "v1.3.0" "taimux-x86_64-linux-musl" "$RD/artefact2"
  OUT="$(TAIMUX_RELEASE_URL="file://$RD/rel2.json" "$IRS" --dir "$D" 2>&1)"
  has "an upgrade names both versions"   "$OUT" "1.2.3 -> 1.3.0"
  eq  "…and the launcher still resolves" "taimux 1.3.0" "$("$D/taimux" version)"

  # A release whose asset is named something else. This is what a renamed CI
  # artefact looks like from here, and it must not install anything.
  fake_release "$RD/rel3.json" "v2.0.0" "taimux-aarch64-linux-musl" "$RD/artefact2"
  r=0
  OUT="$(TAIMUX_RELEASE_URL="file://$RD/rel3.json" "$IRS" --dir "$D" 2>&1)" || r=$?
  eq  "a release without the expected asset is refused" "1" "$r"
  has "…and says what it does have instead" "$OUT" "taimux-aarch64-linux-musl"
  eq  "…leaving the installed one in place" "taimux 1.3.0" "$("$D/taimux" version)"

  # A tag and a binary that disagree. Worth a check of its own because it is the
  # one failure that cannot be seen after the fact: install it and the host is
  # running something other than what the release says it is.
  fake_bin "$RD/artefact4" "0.0.1"
  fake_release "$RD/rel4.json" "v9.9.9" "taimux-x86_64-linux-musl" "$RD/artefact4"
  r=0
  OUT="$(TAIMUX_RELEASE_URL="file://$RD/rel4.json" "$IRS" --dir "$D" 2>&1)" || r=$?
  eq  "a binary that disagrees with its tag is refused" "1" "$r"
  has "…naming both"                        "$OUT" "0.0.1"
  eq  "…and nothing is replaced"            "taimux 1.3.0" "$("$D/taimux" version)"

  # An artefact that cannot run at all: the wrong architecture, a truncated
  # download, or a dynamically linked build on a host whose libc is too old.
  printf 'not a binary\n' > "$RD/artefact5"
  fake_release "$RD/rel5.json" "v9.9.9" "taimux-x86_64-linux-musl" "$RD/artefact5"
  r=0
  OUT="$(TAIMUX_RELEASE_URL="file://$RD/rel5.json" "$IRS" --dir "$D" 2>&1)" || r=$?
  eq  "an artefact that does not run is refused" "1" "$r"
  has "…and says so"                         "$OUT" "does not run here"
  eq  "…and no partial download is left behind" "0" \
      "$(find "$D" -maxdepth 1 -name '.taimux.download.*' | wc -l)"

  # A release that is not there.
  #
  # This repository is public and the downloader asks for no credential, but the
  # message still has to mention one, because `--repo` can point at a private
  # fork and GitHub answers "you cannot see this" exactly as it answers "this
  # does not exist". Someone hitting that gets a 404 for a release they are
  # looking straight at, and nothing else on the path can tell them why.
  r=0
  OUT="$(TAIMUX_RELEASE_URL="file://$RD/nothing.json" "$IRS" --dir "$D" 2>&1)" || r=$?
  eq  "an unreadable release is an error, not a silent skip" "1" "$r"
  has "…and it names the one thing that looks identical"     "$OUT" "credential"

  unset -f fake_bin fake_release
fi

# ============================================================================
section "the picker's keys: driven in a real terminal, read off the screen"
# ============================================================================
# The one class of bug nothing else here can see. A key that is simply NOT BOUND
# fails silently: no error, no wrong output, just a key that does nothing, and
# the only way it surfaces is somebody noticing. That is exactly how Page Up and
# Page Down went missing in the port (fzf bound them itself, `Home` and `End`
# were ported by hand and these were not) and it took a report to find.
#
# So this drives the real TUI in an ISOLATED tmux server, sends the actual keys,
# and reads which row the cursor is on off the screen. Never near the default
# socket: `-L` with its own name, `-f /dev/null` so no configuration is
# inherited (see demo/demo.sh for what @continuum-restore does to a `-L` server),
# and no `kill-server` anywhere but that name.
#
# `TAIMUX_SELF` is the row source, which makes the fixture 40 numbered rows
# rather than whatever this machine happens to be running.
KBIN="$HERE/../target/release/taimux"
KSOCK="taimux-keytest-$$"
ktmux() { tmux -f /dev/null -L "$KSOCK" "$@"; }
if [ ! -x "$KBIN" ]; then
  skip "the picker's keys (no taimux built; run just build)"
elif ! command -v tmux >/dev/null 2>&1; then
  skip "the picker's keys (no tmux here to drive it in)"
else
  KT="$TMP/keys"; mkdir -p "$KT/run"
  cat > "$KT/feed" <<'FEED'
#!/usr/bin/env bash
[ "${1:-}" = _panes ] || exit 1
for i in $(seq -w 1 40); do
  printf '%%%s\twork:%s.1\t/tmp/p%s\tclaude\t2.1.229\tidle\t-\trow-%s\n' \
    "$i" "$i" "$i" "$i"
done
FEED
  chmod +x "$KT/feed"

  # Which row the cursor sits on. The line carries the picker's own border, so
  # the pointer is not at column 0.
  kcursor() {
    ktmux capture-pane -p 2>/dev/null |
      sed -n 's/.*▶ .*\(row-[0-9][0-9]\).*/\1/p' | head -n 1
  }
  # Poll rather than sleep a guessed interval: a fixed wait is either flaky
  # under load or slow for everyone. Gives up after ~3s.
  kwait() {   # $1 = the row wanted -> 0 when it arrives
    local n=0
    while [ "$n" -lt 60 ]; do
      [ "$(kcursor)" = "$1" ] && return 0
      n=$((n+1)); sleep 0.05
    done
    return 1
  }
  kkey() { ktmux send-keys "$@" 2>/dev/null; }
  kexpect() {  # $1 = what this proves, $2 = the row wanted
    if kwait "$2"; then ok "$1"; else no "$1" "expected [$2] got [$(kcursor)]"; fi
  }

  ktmux new-session -d -x 100 -y 24 \
    "TAIMUX_SELF=$KT/feed TAIMUX_SEARCH=0 TAIMUX_SESSIONS=0 TAIMUX_REMOTE=0 \
     XDG_RUNTIME_DIR=$KT/run $KBIN tui >$KT/chosen 2>$KT/err" 2>/dev/null

  kexpect "the picker opens on the first row" "row-01"
  # 24 lines, less a border, a prompt, a header and a 60% preview, leaves the
  # list 8 tall. A page is that height, so this also pins that a page is the
  # DRAWN height rather than a constant somebody picked.
  kkey PageDown
  kexpect "Page Down moves one screenful, not one row" "row-09"
  kkey PageUp
  kexpect "…and Page Up brings it back"                "row-01"
  # Eight pages is well past the end of forty rows.
  for _ in 1 2 3 4 5 6 7 8; do kkey PageDown; done
  kexpect "paging past the end stops at the last row"  "row-40"
  for _ in 1 2 3 4 5 6 7 8; do kkey PageUp; done
  kexpect "…and paging past the start stops at the first" "row-01"
  # The pair that never broke, in the same run, so the comparison is real
  # rather than remembered.
  kkey End
  kexpect "End still goes to the last row"             "row-40"
  kkey Home
  kexpect "Home still goes to the first"               "row-01"
  # Single steps DO wrap, which is what the paging above must not do.
  kkey Up
  kexpect "Up from the top wraps to the bottom"        "row-40"
  kkey Down
  kexpect "…and Down comes back round"                 "row-01"

  # --- the fzf defaults an audit found missing -------------------------------
  # Every one of these was a no-op, confirmed by driving it before binding it.
  # fzf binds both pairs for up and down, and ctrl-j is safe to take: a terminal
  # sends LF for it and CR for Enter, so it does not shadow accept.
  kkey C-j; kexpect "ctrl-j moves down, as in fzf"     "row-02"
  kkey C-k; kexpect "…and ctrl-k up"                   "row-01"
  kkey C-k; kexpect "…wrapping like the arrows"        "row-40"
  kkey C-j; kexpect "…both ways"                       "row-01"

  # What the query does with the editing keys. The query is append-and-pop with
  # no cursor, so fzf's motion keys have nothing to act on, but deleting the last
  # WORD is meaningful and was missing.
  # The query line, with the picker's border and the prompt taken off. The
  # prompt's trailing space has to go BEFORE the right-hand padding is trimmed,
  # or an empty query reads back as the prompt itself.
  kquery() {
    ktmux capture-pane -p 2>/dev/null | sed -n '2p' |
      sed 's/│//g; s/^pick ❯ //; s/ *$//'
  }
  # Poll for it, like kexpect does for the cursor: a fixed sleep is either flaky
  # or slow, and every failure it produces looks like a broken binding.
  kqwait() {
    local n=0
    while [ "$n" -lt 60 ]; do
      [ "$(kquery)" = "$1" ] && return 0
      n=$((n+1)); sleep 0.05
    done
    return 1
  }
  kqexpect() { if kqwait "$2"; then ok "$1"; else no "$1" "expected [$2] got [$(kquery)]"; fi; }
  ktmux send-keys 'red apple' 2>/dev/null
  kqexpect "typing filters on what it is given"  "red apple"
  kkey C-w
  kqexpect "ctrl-w drops the last word"          "red"
  ktmux send-keys ' apple' 2>/dev/null; kqwait 'red apple'
  kkey M-BSpace
  kqexpect "…and so does alt-backspace"          "red"
  # A word-kill leaves the space behind it, as readline's unix-word-rubout does,
  # so "red apple" becomes "red " and not "red". The character checks therefore
  # start from a query with no spaces in it: a trailing space is invisible on
  # screen, and an expectation that cannot see it is an expectation that lies.
  kkey C-u; kqwait ""
  ktmux send-keys 'abc' 2>/dev/null; kqwait abc
  kkey C-h
  kqexpect "ctrl-h is a backspace, one character" "ab"
  kkey BSpace
  kqexpect "…and so is backspace"                 "a"

  # THE defect this audit was worth doing for. `Char(c) if !ctrl` matched while
  # ALT was held, because ALT is not CTRL, so every alt-chord TYPED ITS LETTER
  # into the query. fzf uses alt-b, alt-f and alt-d for word motion, so the keys
  # most likely to be pressed by habit were the ones that corrupted the search.
  for k in M-b M-f M-d M-x; do
    kkey "$k"
  done
  sleep 0.4
  eq "an alt chord types nothing at all"    "a" "$(kquery)"
  kkey C-u
  kqexpect "ctrl-u clears the query"        ""

  # ctrl-l repaints. The check is not that the screen looks right, it is that
  # STDOUT is untouched: `Terminal::clear` would have been the obvious call and
  # it snapshots the cursor through crossterm, which writes ESC[6n to the
  # process's stdout. Stdout here carries one thing, the chosen pane id, so that
  # put `[6n` where the caller reads the answer.
  kkey C-l; sleep 0.4
  eq "ctrl-l writes nothing to stdout"      "" "$(cat "$KT/chosen" 2>/dev/null)"
  kexpect "…and the list is still there"    "row-01"

  kkey Escape
  eq "the picker chose nothing on Esc" "" "$(cat "$KT/chosen" 2>/dev/null)"
  eq "…and said nothing on stderr"     "" "$(cat "$KT/err" 2>/dev/null)"
  ktmux kill-server 2>/dev/null

  # The three other keys fzf aborts on. Each needs its own picker, since the
  # assertion is that it exited, so they come last and get a session each.
  for k in C-g C-q C-c; do
    ktmux new-session -d -x 100 -y 24 \
      "TAIMUX_SELF=$KT/feed TAIMUX_SEARCH=0 TAIMUX_SESSIONS=0 TAIMUX_REMOTE=0 \
       XDG_RUNTIME_DIR=$KT/run $KBIN tui >$KT/abort 2>/dev/null" 2>/dev/null
    kwait row-01
    ktmux send-keys "$k" 2>/dev/null
    n=0; while [ "$n" -lt 60 ] && ktmux has-session 2>/dev/null; do n=$((n+1)); sleep 0.05; done
    r=0; ktmux has-session 2>/dev/null && r=1
    eq "$k aborts the picker"            "0" "$r"
    eq "…choosing nothing"               ""  "$(cat "$KT/abort" 2>/dev/null)"
    ktmux kill-server 2>/dev/null
  done
fi

# ============================================================================
section "F1 from a pane with no agent: where the cursor opens"
# ============================================================================
# The picker tests above drive `tui`, which is handed a row source and nothing
# else. This one drives `pick`, the subcommand the binding runs, because the
# question is about the pane the picker was opened FROM rather than about a key
# pressed once it is up: that pane runs a shell, so it is not a row, the ●
# marker has nothing to sit on, and the cursor used to land on whichever row
# the scan listed first.
#
# Three sessions, and each run's answer has to be one no OTHER rule would give,
# or a passing test proves nothing. The first picker opens in the directory of
# the MIDDLE session: not the top of the list, which is what the cursor used to
# fall to, and not the nearest pane either, which is the session after it. The
# second opens somewhere with nothing in common with any of them, which is the
# case where the directory says nothing at all and the tmux list decides, and
# its answer is the nearest pane rather than the top of the list again.
NSOCK="taimux-neartest-$$"
ntmux() { tmux -f /dev/null -L "$NSOCK" "$@"; }
if [ ! -x "$KBIN" ]; then
  skip "the opening cursor (no taimux built; run just build)"
elif ! command -v tmux >/dev/null 2>&1; then
  skip "the opening cursor (no tmux here to drive it in)"
else
  NT="$TMP/near"
  mkdir -p "$NT/run" "$NT/bin" "$NT/tree/other" "$NT/tree/web" "$NT/tree/api" "$NT/elsewhere"
  # A pane whose foreground process IS "claude", the way demo.sh fakes one.
  cp "${BASH:-/bin/bash}" "$NT/bin/claude"
  nagent="$NT/bin/claude -c 'while :; do sleep 1; done'"
  npick="TAIMUX_SEARCH=0 TAIMUX_SESSIONS=0 TAIMUX_REMOTE=0 XDG_RUNTIME_DIR=$NT/run $KBIN pick"
  # Which session the cursor is on, by the directory its row shows. The path
  # column is the last two components of it, which is what tells the two apart.
  ncursor() {   # $1 = the window the picker is in
    ntmux capture-pane -p -t "near:$1" 2>/dev/null |
      sed -n 's/.*▶ .*\(tree\/[a-z]*\).*/\1/p' | head -n 1
  }
  # Polled, like the keys above: the first scan lands when it lands, and a fixed
  # sleep is either flaky under load or slow for everyone. ~6s.
  nexpect() {   # $1 = what this proves, $2 = the window, $3 = the row wanted
    local n=0
    while [ "$n" -lt 120 ]; do
      [ "$(ncursor "$2")" = "$3" ] && { ok "$1"; return 0; }
      n=$((n+1)); sleep 0.05
    done
    no "$1" "expected [$3] got [$(ncursor "$2")]"
  }

  # Windows 0, 1 and 2 are the agent sessions, in that order, and every picker
  # below opens in a window of its own after them: so the top of the list is
  # `other`, and the nearest pane to any picker is `api`.
  ntmux new-session -d -s near -x 100 -y 24 -c "$NT/tree/other" "$nagent" 2>/dev/null
  ntmux new-window  -d -t near              -c "$NT/tree/web"   "$nagent" 2>/dev/null
  ntmux new-window  -d -t near              -c "$NT/tree/api"   "$nagent" 2>/dev/null

  # The picker's window is the CURRENT one, not a background one: with no client
  # attached, the pane a picker asks tmux for is resolved against the session's
  # current window, and a picker opened with `-d` is handed the pane of whatever
  # window was in front instead. That pane is one of the agents, which has a row,
  # so the cursor lands on it and both runs below pass while proving nothing.
  ntmux new-window -t near -c "$NT/tree/web" "$npick" 2>/dev/null
  nexpect "the cursor opens on the session in the same directory" 3 "tree/web"

  ntmux new-window -t near -c "$NT/elsewhere" "$npick" 2>/dev/null
  nexpect "…and on the nearest pane when no session shares it"    4 "tree/api"

  ntmux kill-server 2>/dev/null
  pkill -f "$NT/bin/claude" 2>/dev/null
fi

# ============================================================================
section "Tab's outdated stop: the rows ctrl-x and F8 act on, gathered"
# ============================================================================
# Same reasoning as the keys above, and one more of its own: this stop is the
# only one whose filter is not a STATE. It asks what a session is RUNNING, so it
# crosses waiting, working and idle, and it can only be reached by pressing Tab
# past all three. A cycle that quietly skipped it would look exactly like a cycle
# that has it.
#
# The fixture owns its own HOME, because what counts as behind is measured
# against the claude the launcher there points at: read off the developer's real
# machine it would be a different answer every week.
OSOCK="taimux-outdatedtest-$$"
otmux() { tmux -f /dev/null -L "$OSOCK" "$@"; }
if [ ! -x "$KBIN" ]; then
  skip "Tab's outdated stop (no taimux built; run just build)"
elif ! command -v tmux >/dev/null 2>&1; then
  skip "Tab's outdated stop (no tmux here to drive it in)"
else
  OT="$TMP/outdated"; mkdir -p "$OT/run" "$OT/home/.local/bin" \
                               "$OT/home/.local/share/claude/versions" "$OT/bare"
  : > "$OT/home/.local/share/claude/versions/2.1.243"
  ln -sf "$OT/home/.local/share/claude/versions/2.1.243" "$OT/home/.local/bin/claude"

  # One row per case the predicate has to get right: behind while idle, behind
  # while asking, current, another agent entirely, and one on another host (where
  # the installed version here says nothing about it).
  cat > "$OT/feed" <<'OFEED'
#!/usr/bin/env bash
[ "${1:-}" = _panes ] || exit 1
printf '%%01\twork:1.1\t/tmp/p1\tclaude\t2.1.229\tidle\t-\trow-01\n'
printf '%%02\twork:2.1\t/tmp/p2\tclaude\t2.1.229\tinput\t-\trow-02\n'
printf '%%03\twork:3.1\t/tmp/p3\tclaude\t2.1.243\trun\t-\trow-03\n'
printf '%%04\tops:1.1\t/tmp/p4\tgemini\t0.41.2\tidle\t-\trow-04\n'
printf 'ha:%%05\tops:2.1\t/tmp/p5\tclaude\t2.1.229\tidle\t-\trow-05\n'
OFEED
  chmod +x "$OT/feed"

  # The border label, which is where the picker says which list this is.
  olabel() { otmux capture-pane -p 2>/dev/null | sed -n '1p'; }
  owait() {   # $1 = what the label must contain
    local n=0
    while [ "$n" -lt 60 ]; do
      case "$(olabel)" in *"$1"*) return 0 ;; esac
      n=$((n+1)); sleep 0.05
    done
    return 1
  }
  oexpect() { if owait "$2"; then ok "$1"; else no "$1" "expected [$2] in [$(olabel)]"; fi; }
  # Which rows are on screen. The preview shows a row's target and cwd, never its
  # summary, so nothing below the list can answer this by accident.
  orows() {
    otmux capture-pane -p 2>/dev/null |
      sed -n 's/.*\(row-[0-9][0-9]\).*/\1/p' | sort -u | tr '\n' ' ' | sed 's/ *$//'
  }
  otab() { otmux send-keys Tab 2>/dev/null; }

  otmux new-session -d -x 110 -y 24 \
    "TAIMUX_SELF=$OT/feed TAIMUX_SEARCH=0 TAIMUX_SESSIONS=0 TAIMUX_REMOTE=0 \
     HOME=$OT/home XDG_RUNTIME_DIR=$OT/run $KBIN tui >$OT/chosen 2>$OT/err" 2>/dev/null

  oexpect "the picker opens on the whole list"      "agent sessions"
  otab; oexpect "tab: the sessions waiting"         "waiting for an answer"
  otab; oexpect "…then the ones working"            "working"
  otab; oexpect "…then the idle ones"               "idle at the prompt"
  otab; oexpect "…then the ones running outdated code" "running outdated code"
  # Behind while idle AND behind while asking: the stop crosses the states above
  # it rather than sitting inside one of them.
  sleep 0.3
  eq "…which is every row a restart would act on" "row-01 row-02" "$(orows)"
  otab; oexpect "…and round to the whole list again" "agent sessions"

  # The version column and the list have to agree, since both answer "would
  # ctrl-x do anything here": one yellow version per row in that list. tmux
  # re-emits a basic colour in its 256-colour form, so yellow reads back as
  # `38;5;3` and not the `33` the layout wrote; anchoring on the version text
  # keeps the waiting star, which is yellow too, out of the count.
  otab; otab; otab; otab; owait "running outdated code"
  eq "the two rows are the two yellow versions" "2" \
     "$(otmux capture-pane -pe 2>/dev/null | grep -c '\[38;5;3m2\.1\.229')"

  otmux send-keys Escape 2>/dev/null
  eq "…and nothing was said on stderr" "" "$(cat "$OT/err" 2>/dev/null)"
  otmux kill-server 2>/dev/null

  # Nothing installed to compare against, so the stop is out of the cycle
  # entirely rather than reached and found empty.
  otmux new-session -d -x 110 -y 24 \
    "TAIMUX_SELF=$OT/feed TAIMUX_SEARCH=0 TAIMUX_SESSIONS=0 TAIMUX_REMOTE=0 \
     HOME=$OT/bare XDG_RUNTIME_DIR=$OT/run $KBIN tui >/dev/null 2>&1" 2>/dev/null
  owait "agent sessions"
  otab; otab; otab; owait "idle at the prompt"
  otab; oexpect "with no claude installed, idle is the last stop" "agent sessions"
  otmux kill-server 2>/dev/null
fi

# ============================================================================
section "a child that cannot start SAYS so, instead of flashing past"
# ============================================================================
# Reported 2026-09-09 as a blank popup after F8, with keys echoing into it.
#
# The press re-execs the picker's OWN binary, and the result was DISCARDED:
# `let _ = act_child(...)`. So a child that could not be started looked exactly
# like one that ran and did nothing. The picker suspended, the exec failed,
# the picker resumed, and the only evidence was a flicker.
#
# It is an ordinary thing to hit rather than an exotic one, because the path
# re-exec'd is the running binary's own: moving the checkout or rebuilding it
# under a live picker is enough, which is precisely what had been happening.
FSOCK="taimux-failedchild-$$"
ftmux() { tmux -f /dev/null -L "$FSOCK" "$@"; }
if [ ! -x "$KBIN" ]; then
  skip "a child that cannot start (no taimux built; run just build)"
elif ! command -v tmux >/dev/null 2>&1; then
  skip "a child that cannot start (needs tmux)"
else
  FT="$TMP/failedchild"; mkdir -p "$FT/run"
  fwait() {
    local n=0
    while [ "$n" -lt 100 ]; do
      ftmux capture-pane -p 2>/dev/null | grep -q "$1" && return 0
      n=$((n+1)); sleep 0.05
    done
    return 1
  }

  # A script path that does not exist. The row source falls back on its own, so
  # the picker still comes up; it is F8 that has nothing to exec.
  ftmux new-session -d -x 90 -y 16 \
    "TAIMUX_SELF=$FT/not-here TAIMUX_SEARCH=0 TAIMUX_SESSIONS=0 TAIMUX_REMOTE=0 \
     XDG_RUNTIME_DIR=$FT/run $KBIN tui >$FT/chosen 2>$FT/err; sleep 30" 2>/dev/null
  if fwait "agent sessions"; then ok "the picker is up with an unrunnable script"
  else no "the picker is up with an unrunnable script" \
          "got [$(ftmux capture-pane -p | sed -n '1p')]"; fi

  ftmux send-keys F8 2>/dev/null
  if fwait "could not run"; then ok "F8 reports a child it could not start"
  else no "F8 reports a child it could not start" \
          "screen: [$(ftmux capture-pane -p | sed -n '1,3p' | tr '\n' '|')]"; fi

  # Held, not flashed. The whole failure of the old behaviour was that the
  # screen went by before it could be read, so the pause is the fix.
  if fwait "Press any key"; then ok "…and holds the screen to be read"
  else no "…and holds the screen to be read" "no pause"; fi

  ftmux send-keys q 2>/dev/null
  if fwait "agent sessions"; then ok "…and the picker comes back after it"
  else no "…and the picker comes back after it" \
          "got [$(ftmux capture-pane -p | sed -n '1p')]"; fi

  # stdout still carries only what the caller reads for a chosen pane.
  eq "…with nothing of the report on stdout" "" "$(cat "$FT/chosen" 2>/dev/null)"
  ftmux kill-server 2>/dev/null
fi

# ============================================================================
section "the version stamp: which taimux drew this list"
# ============================================================================
# A popup launched by a tmux binding says nothing about which binary drew it, and
# a self-update swaps the launcher under a running server, so the same keypress
# can draw a different version tomorrow. The stamp answers that in the bottom
# right, and the check compares it against `taimux version` so the two cannot
# drift apart.
#
# The narrow case is the one worth driving: ratatui gives a right-aligned title
# precedence over a left-aligned one, so the stamp ate the count at 20 columns
# before the width check went in. The count is the live half and must survive.
VSOCK="taimux-vertest-$$"
vtmux() { tmux -f /dev/null -L "$VSOCK" "$@"; }
if [ ! -x "$KBIN" ]; then
  skip "the version stamp (no taimux built; run just build)"
elif ! command -v tmux >/dev/null 2>&1; then
  skip "the version stamp (no tmux here to drive it in)"
else
  VT="$TMP/version"; mkdir -p "$VT/run"
  # One row, so the count is a known short string on both borders below.
  cat > "$VT/feed" <<'VFEED'
#!/usr/bin/env bash
[ "${1:-}" = _panes ] || exit 1
printf '%%01\twork:1.1\t/tmp/p1\tclaude\t2.1.229\tidle\t-\trow-01\n'
VFEED
  chmod +x "$VT/feed"
  # The bottom border, which is where both the count and the stamp live.
  vfoot() { vtmux capture-pane -p 2>/dev/null | sed -n '$p'; }
  vwait() {   # $1 = what the bottom border must contain
    local n=0
    while [ "$n" -lt 60 ]; do
      case "$(vfoot)" in *"$1"*) return 0 ;; esac
      n=$((n+1)); sleep 0.05
    done
    return 1
  }
  vup() {  # $1 = window width
    vtmux new-session -d -x "$1" -y 12 \
      "TAIMUX_SELF=$VT/feed TAIMUX_SEARCH=0 TAIMUX_SESSIONS=0 TAIMUX_REMOTE=0 \
       XDG_RUNTIME_DIR=$VT/run $KBIN tui >/dev/null 2>&1" 2>/dev/null
  }

  # Exactly what the CLI reports, so a bumped crate version moves both or neither.
  VER="$("$KBIN" version)"
  vup 100
  if vwait "$VER"; then ok "the bottom border carries '$VER'"
  else no "the bottom border carries '$VER'" "got [$(vfoot)]"; fi
  # Right-aligned means nothing but the corner follows it. Tested by what comes
  # AFTER the stamp rather than by counting columns, since a border dash is
  # three bytes and every column count in shell is a locale question.
  vline="$(vfoot)"
  case "${vline#*taimux }" in
    *─*) no "…in the RIGHT corner" "border fill follows the stamp: [$vline]" ;;
    *)   ok "…in the RIGHT corner, with only the border corner after it" ;;
  esac
  # Waited for, not assumed: the first scan runs off the input loop now, so the
  # border legitimately reads 0/0 for the moment before the rows land.
  n=0; while [ "$n" -lt 60 ]; do case "$(vfoot)" in *1/1*) break ;; esac; n=$((n+1)); sleep 0.05; done
  has "…with the count still in the left one" "$(vfoot)" "1/1"
  vtmux kill-server 2>/dev/null

  # Too narrow for both: the stamp goes, the count stays.
  vup 20
  vwait "1/1"
  hasnt "a border too narrow drops the stamp" "$(vfoot)" "taimux"
  has   "…and keeps the count, which is the live half" "$(vfoot)" "1/1"
  vtmux kill-server 2>/dev/null
fi

# ============================================================================
section "a slow refresh: the picker stays alive, and says so"
# ============================================================================
# The bug this exists for, reported 2026-09-05: after F8 restarted 30 sessions,
# the picker sat on the sweep's last screen for the 85 seconds the restarts took.
# Esc did nothing, ctrl-c did nothing, tmux's own F1 did nothing. The scan ran ON
# the input loop, so for its whole duration there was no draw and no key read,
# and nothing on screen said the picker was alive.
#
# So this drives a row source that HANGS and asserts the three things that were
# false that day: the picker still draws, it still takes keys, and Esc still gets
# you out. The first scan answers instantly, because the picker cannot draw
# anything until one does; every later one sleeps.
RSOCK="taimux-slowtest-$$"
rtmux() { tmux -f /dev/null -L "$RSOCK" "$@"; }
if [ ! -x "$KBIN" ]; then
  skip "a slow refresh (no taimux built; run just build)"
elif ! command -v tmux >/dev/null 2>&1; then
  skip "a slow refresh (no tmux here to drive it in)"
else
  ST="$TMP/slow"; mkdir -p "$ST/run"
  cat > "$ST/feed" <<'SFEED'
#!/usr/bin/env bash
[ "${1:-}" = _panes ] || exit 1
[ -e "$ST/warm" ] && sleep 4
: > "$ST/warm"
printf '%%01\twork:1.1\t/tmp/p1\tclaude\t2.1.229\tidle\t-\trow-01\n'
SFEED
  chmod +x "$ST/feed"

  rfoot() { rtmux capture-pane -p 2>/dev/null | sed -n '1p'; }
  rquery() {
    rtmux capture-pane -p 2>/dev/null | sed -n '2p' |
      sed 's/│//g; s/^pick ❯ //; s/ *$//'
  }
  rwait() {   # $1 = what the border must contain
    local n=0
    while [ "$n" -lt 100 ]; do
      case "$(rfoot)" in *"$1"*) return 0 ;; esac
      n=$((n+1)); sleep 0.05
    done
    return 1
  }

  # A one-second timer, so the hanging scan is in flight almost at once.
  rtmux new-session -d -x 90 -y 14 \
    "ST=$ST TAIMUX_SELF=$ST/feed TAIMUX_REFRESH=1 TAIMUX_SEARCH=0 TAIMUX_SESSIONS=0 \
     TAIMUX_REMOTE=0 XDG_RUNTIME_DIR=$ST/run $KBIN tui >$ST/chosen 2>$ST/err" 2>/dev/null

  if rwait "agent sessions"; then ok "the picker opens on the first (fast) scan"
  else no "the picker opens on the first (fast) scan" "got [$(rfoot)]"; fi
  # The scan is now stuck. The border has to say so rather than looking dead.
  if rwait "refreshing"; then ok "a scan that is taking too long says so on the border"
  else no "a scan that is taking too long says so on the border" "got [$(rfoot)]"; fi
  # …and the loop is still reading keys while it hangs, which is the whole point.
  rtmux send-keys 'zzz' 2>/dev/null
  n=0; while [ "$n" -lt 60 ] && [ "$(rquery)" != "zzz" ]; do n=$((n+1)); sleep 0.05; done
  eq "typing still works while a scan is stuck" "zzz" "$(rquery)"
  rtmux send-keys C-u 2>/dev/null

  # It lands eventually, and being slow it writes itself down where the sweep
  # already points: that log is what names the culprit next time.
  n=0; while [ "$n" -lt 120 ] && ! grep -q "picker refresh took" "$ST/run/taimux/restart.log" 2>/dev/null; do
    n=$((n+1)); sleep 0.1
  done
  if grep -q "picker refresh took" "$ST/run/taimux/restart.log" 2>/dev/null; then
    ok "a slow refresh is written to the log with its timing"
  else
    no "a slow refresh is written to the log with its timing" "nothing in restart.log"
  fi

  # And Esc gets you out during one, which it did not on the day.
  rtmux send-keys Escape 2>/dev/null
  n=0; while [ "$n" -lt 40 ] && rtmux has-session 2>/dev/null; do n=$((n+1)); sleep 0.05; done
  r=0; rtmux has-session 2>/dev/null && r=1
  eq "Esc gets out while a scan is in flight" "0" "$r"
  eq "…and nothing was said on stderr"        ""  "$(cat "$ST/err" 2>/dev/null)"
  rtmux kill-server 2>/dev/null
fi

# ============================================================================
section "an empty list says so instead of closing"
# ============================================================================
# Reported 2026-09-07 as "F1 no longer works" on a machine that turned out to
# have no agent sessions running at all: the picker closed itself on an empty
# list, so the popup opened and shut too fast to see. That is indistinguishable
# from an unbound key, a taimux that is not installed, and a popup that failed
# to start, which is three wrong things to go looking for.
ESOCK="taimux-emptytest-$$"
etmux() { tmux -f /dev/null -L "$ESOCK" "$@"; }
if [ ! -x "$KBIN" ]; then
  skip "an empty list (no taimux built; run just build)"
elif ! command -v tmux >/dev/null 2>&1; then
  skip "an empty list (no tmux here to drive it in)"
else
  ET="$TMP/empty"; mkdir -p "$ET/run"
  # A machine with nothing running: the row source answers, with no rows.
  printf '#!/usr/bin/env bash\n[ "${1:-}" = _panes ] && exit 0\nexit 1\n' > "$ET/none"
  # …and one with a single idle session, for the per-state message.
  cat > "$ET/one" <<'EFEED'
#!/usr/bin/env bash
[ "${1:-}" = _panes ] || exit 1
printf '%%01\twork:1.1\t/tmp/p1\tclaude\t2.1.229\tidle\t-\trow-01\n'
EFEED
  chmod +x "$ET/none" "$ET/one"
  eup() {   # $1 = which feed
    etmux new-session -d -x 84 -y 14 \
      "TAIMUX_SELF=$ET/$1 TAIMUX_SEARCH=0 TAIMUX_SESSIONS=0 TAIMUX_REMOTE=0 \
       XDG_RUNTIME_DIR=$ET/run $KBIN tui >$ET/chosen 2>$ET/err; sleep 20" 2>/dev/null
  }
  ewait() {   # $1 = text wanted on screen
    local n=0
    while [ "$n" -lt 100 ]; do
      etmux capture-pane -p 2>/dev/null | grep -q "$1" && return 0
      n=$((n+1)); sleep 0.05
    done
    return 1
  }

  : > "$ET/chosen"; : > "$ET/err"
  eup none
  if ewait "No agent sessions on this machine"; then
    ok "a machine with nothing running gets told so, rather than a popup that vanishes"
  else
    no "a machine with nothing running gets told so" "screen: [$(etmux capture-pane -p | sed -n '4p')]"
  fi
  has "…and it says how to close it" "$(etmux capture-pane -p 2>/dev/null)" "Esc closes this"
  # Still a live picker: Esc gets out, and it chose nothing.
  etmux send-keys Escape 2>/dev/null
  n=0; while [ "$n" -lt 40 ] && [ ! -s "$ET/chosen" ]; do n=$((n+1)); sleep 0.05; done
  eq "…and Esc closes it, choosing nothing" "" "$(cat "$ET/chosen" 2>/dev/null)"
  etmux kill-server 2>/dev/null; sleep 0.3

  # One session running, viewed through a state it is not in: that is a
  # different silence and has to read differently, or the picker claims the
  # machine is empty while a row sits one Tab away.
  eup one
  ewait "row-01"
  etmux send-keys Tab 2>/dev/null
  if ewait "Nothing is waiting for an answer right now"; then
    ok "a state with nothing in it says THAT, not that the machine is empty"
  else
    no "a state with nothing in it says that" "screen: [$(etmux capture-pane -p | sed -n '4p')]"
  fi
  hasnt "…and does not claim there are no sessions" \
        "$(etmux capture-pane -p 2>/dev/null)" "No agent sessions"
  # A query nobody matches is a third one, and it says what undoes it.
  etmux send-keys Tab 2>/dev/null; etmux send-keys Tab 2>/dev/null
  etmux send-keys 'zzzz' 2>/dev/null
  if ewait "Nothing matches zzzz"; then ok "a query that matches nothing says so"
  else no "a query that matches nothing says so" "screen: [$(etmux capture-pane -p | sed -n '4p')]"; fi
  has "…and points at ctrl-u" "$(etmux capture-pane -p 2>/dev/null)" "ctrl-u"
  etmux kill-server 2>/dev/null; sleep 0.3

  # And the state BEFORE the first scan lands. It used to be synchronous, so a
  # slow one left the popup empty with no keys: reported 2026-09-08 as a stuck
  # picker, a blank box over a session.
  cat > "$ET/slow" <<'EFEED'
#!/usr/bin/env bash
[ "${1:-}" = _panes ] || exit 1
sleep 3
printf '%%01\twork:1.1\t/tmp/p1\tclaude\t2.1.229\tidle\t-\trow-01\n'
EFEED
  chmod +x "$ET/slow"
  eup slow
  if ewait "Looking for agent sessions"; then
    ok "a first scan still running says so, rather than drawing an empty box"
  else
    no "a first scan still running says so" "screen: [$(etmux capture-pane -p | sed -n '4p')]"
  fi
  # …and the loop is alive while it runs, which it could not be when the first
  # scan was synchronous.
  etmux send-keys 'ab' 2>/dev/null
  n=0; while [ "$n" -lt 40 ] && ! etmux capture-pane -p 2>/dev/null | sed -n '2p' | grep -q 'ab'; do
    n=$((n+1)); sleep 0.05
  done
  r=0; etmux capture-pane -p 2>/dev/null | sed -n '2p' | grep -q 'ab' || r=1
  eq "…and takes keys while it scans" "0" "$r"
  etmux send-keys C-u 2>/dev/null      # or the arriving row is filtered out by it
  if ewait "row-01"; then ok "…then the rows arrive into the same picker"
  else no "…then the rows arrive" "screen: [$(etmux capture-pane -p | sed -n '4p')]"; fi
  etmux kill-server 2>/dev/null
fi

# ============================================================================
section "two clients on one session: the popup stays where it was opened"
# ============================================================================
# Reported 2026-09-08 as another stuck picker: a popup arrived on the desktop,
# over an unrelated session, empty. The cause is that `#{client_width}` with no
# target answers for whichever client tmux considers current, and Patrick keeps
# two attached to one session, a 213-column desktop and a 46-column phone. A
# picker opened on the PHONE measured its 44-column popup against the desktop,
# concluded it had been outgrown, closed itself and reopened over there.
#
# So the resize only acts when tmux can name the client without guessing, which
# means exactly one attached to our session. This drives the two-client shape and
# asserts the picker stays put.
TSOCK="taimux-twoclient-$$"
ttmux() { tmux -f /dev/null -L "$TSOCK" "$@"; }
if [ ! -x "$KBIN" ]; then
  skip "two clients on one session (no taimux built; run just build)"
elif ! command -v tmux >/dev/null 2>&1 || ! command -v script >/dev/null 2>&1; then
  skip "two clients on one session (needs tmux and script to attach two clients)"
else
  TC="$TMP/twoclient"; mkdir -p "$TC/run" "$TC/bin" "$TC/agents/claude/2.1.229"
  cp "$KBIN" "$TC/bin/taimux"
  cp "${BASH:-/bin/bash}" "$TC/agents/claude/2.1.229/claude"
  ttmux new-session -d -x 200 -y 60 \
    "$TC/agents/claude/2.1.229/claude -c 'while :; do sleep 1; done'" 2>/dev/null
  for v in "XDG_RUNTIME_DIR $TC/run" "TAIMUX_REFRESH 1" "TAIMUX_SEARCH 0" \
           "TAIMUX_SESSIONS 0" "TAIMUX_REMOTE 0"; do
    # shellcheck disable=SC2086
    ttmux set-environment -g $v
  done
  script -q -c "tmux -f /dev/null -L $TSOCK attach" /dev/null > "$TC/big" 2>&1 &
  TBIG=$!
  sleep 1.5
  script -q -c "tmux -f /dev/null -L $TSOCK attach" /dev/null > "$TC/small" 2>&1 &
  TSMALL=$!
  sleep 2
  TT1="$(ttmux list-clients -F '#{client_tty}' 2>/dev/null | sed -n '1p')"
  TT2="$(ttmux list-clients -F '#{client_tty}' 2>/dev/null | sed -n '2p')"
  tlive() { pgrep -c -f "$TC/bin/taimux pick" 2>/dev/null || echo 0; }
  if [ -z "$TT2" ]; then
    skip "two clients on one session (could not attach two clients here)"
  else
    stty -F "$TT1" cols 200 rows 60 2>/dev/null
    stty -F "$TT2" cols 46 rows 38 2>/dev/null
    sleep 1
    # Opened on the SMALL client, the way pressing F1 on the phone does.
    ttmux display-popup -c "$TT2" -E -e TAIMUX_POPUP=1 -w 100% -h 90% \
      "$TC/bin/taimux pick" 2>/dev/null &
    n=0; while [ "$n" -lt 60 ] && [ "$(tlive)" = 0 ]; do n=$((n+1)); sleep 0.1; done
    r=0; [ "$(tlive)" -ge 1 ] || r=1
    eq "the picker opens on the small client"    "0" "$r"
    # Several refresh ticks: long enough for a misfiring resize to have fired.
    sleep 6
    r=0; [ "$(tlive)" -ge 1 ] || r=1
    eq "…and is still there, not closed by a resize it should not make" "0" "$r"
    r=0; grep -q "agent sessions" "$TC/small" 2>/dev/null || r=1
    eq "…drawn on the client it was opened on"   "0" "$r"
    r=0; grep -q "agent sessions" "$TC/big" 2>/dev/null && r=1
    eq "…and never on the OTHER client"          "0" "$r"
  fi
  ttmux kill-server 2>/dev/null
  kill "$TBIG" "$TSMALL" 2>/dev/null
  pkill -f "$TC/bin/taimux" 2>/dev/null
fi

# ============================================================================
section "the sweep's screen does not outlive the sweep"
# ============================================================================
# Reported 2026-09-06: F8, confirm, the list comes back, and then choosing a row
# put the sweep's "restart every outdated session" screen back on the terminal,
# as if it had run a second time.
#
# Two faults, both from the child inheriting things it should not. It owns the
# NORMAL screen while it runs, so its last frame sits under the picker and the
# alternate screen is what hides it: leaving that on the way out reveals it
# again. And it inherits the picker's STDOUT, which carries exactly one thing,
# the chosen pane id, so the whole sweep screen came back to the caller with the
# pane id on the end of it.
XSOCK="taimux-sweepscreen-$$"
xtmux() { tmux -f /dev/null -L "$XSOCK" "$@"; }
if [ ! -x "$KBIN" ]; then
  skip "the sweep's screen (no taimux built; run just build)"
elif ! command -v tmux >/dev/null 2>&1; then
  skip "the sweep's screen (no tmux here to drive it in)"
else
  XT="$TMP/sweepscreen"; mkdir -p "$XT/run"
  # Stands in for `taimux _sweep`: clears the screen, draws a plan, waits for
  # one key, reports, and exits. The shape is what matters, not the plan.
  cat > "$XT/self" <<'XFEED'
#!/usr/bin/env bash
case "${1:-}" in
  _panes)
    printf '%%01\twork:1.1\t/tmp/p1\tclaude\t2.1.229\tidle\t-\trow-01\n'
    printf '%%02\twork:2.1\t/tmp/p2\tclaude\t2.1.229\trun\t-\trow-02\n'
    exit 0 ;;
  _sweep)
    printf '\033[H\033[2Jtaimux: restart every outdated session\n\n  %%01 work:1.1  a title\n\n'
    printf 'Restart 1 session(s)? [Y/n] '
    stty raw -echo 2>/dev/null; dd bs=1 count=1 2>/dev/null >/dev/null; stty sane 2>/dev/null
    printf '\nStarted, detached.\n'
    sleep 0.8
    exit 0 ;;
esac
exit 1
XFEED
  chmod +x "$XT/self"
  xrow() { xtmux capture-pane -p 2>/dev/null | sed -n '4p'; }
  xwait() {   # $1 = text wanted anywhere on screen
    local n=0
    while [ "$n" -lt 100 ]; do
      xtmux capture-pane -p 2>/dev/null | grep -q "$1" && return 0
      n=$((n+1)); sleep 0.05
    done
    return 1
  }

  xtmux new-session -d -x 90 -y 16 \
    "TAIMUX_SELF=$XT/self TAIMUX_SEARCH=0 TAIMUX_SESSIONS=0 TAIMUX_REMOTE=0 \
     XDG_RUNTIME_DIR=$XT/run $KBIN tui >$XT/chosen 2>$XT/err; sleep 30" 2>/dev/null
  if xwait "agent sessions"; then ok "the picker is up"
  else no "the picker is up" "got [$(xtmux capture-pane -p | sed -n '1p')]"; fi

  xtmux send-keys F8 2>/dev/null
  if xwait "restart every outdated"; then ok "F8 hands the terminal to the sweep"
  else no "F8 hands the terminal to the sweep" "no sweep screen"; fi

  xtmux send-keys Enter 2>/dev/null          # confirm
  if xwait "agent sessions"; then ok "…and the picker comes back when it exits"
  else no "…and the picker comes back when it exits" "got [$(xtmux capture-pane -p | sed -n '1p')]"; fi

  xtmux send-keys Enter 2>/dev/null          # choose the highlighted row
  n=0; while [ "$n" -lt 60 ] && [ ! -s "$XT/chosen" ]; do n=$((n+1)); sleep 0.05; done
  # THE bug: the answer must be one pane id, not the sweep's screen with a pane
  # id on the end.
  eq "the answer is the pane id and nothing else" "%01" "$(tr -d '\n' < "$XT/chosen" 2>/dev/null)"
  sleep 0.5
  # …and the screen the child drew must not come back from under the picker.
  r=0; xtmux capture-pane -p 2>/dev/null | grep -q "restart every outdated" && r=1
  eq "…and the sweep's screen is not revealed on the way out" "0" "$r"
  eq "…and nothing was said on stderr" "" "$(cat "$XT/err" 2>/dev/null)"
  xtmux kill-server 2>/dev/null
fi

# ============================================================================
section "a terminal that GROWS: the popup follows it"
# ============================================================================
# tmux shrinks a popup to fit a client that got smaller, and grows it back up to
# the size it was asked for, but never past that. So a picker opened on a phone
# in portrait stays portrait-wide after the rotation, at 63% of a screen it was
# told to take 80% of, and there is no tmux command that resizes a popup in
# place. The picker therefore leaves and asks for a new one at the geometry the
# binding would choose now, carrying what it was doing.
#
# Every part of that is invisible to `capture-pane`, since a popup is drawn OVER
# the panes rather than in one. So this attaches a real client through `script`,
# resizes its pty with `stty`, and reads the picker's own border off the client's
# terminal stream. The binary is a COPY at a fixture path, so nothing here counts
# or kills the developer's own picker.
GSOCK="taimux-growtest-$$"
gtmux() { tmux -f /dev/null -L "$GSOCK" "$@"; }
if [ ! -x "$KBIN" ]; then
  skip "a terminal that grows (no taimux built; run just build)"
elif ! command -v tmux >/dev/null 2>&1 || ! command -v script >/dev/null 2>&1; then
  skip "a terminal that grows (needs tmux and script to drive a real client)"
else
  GT="$TMP/grow"; mkdir -p "$GT/run" "$GT/bin" "$GT/agents/claude/2.1.229"
  cp "$KBIN" "$GT/bin/taimux"
  # A pane whose foreground process IS "claude", the way demo.sh fakes one: the
  # picker needs at least one row or it closes again immediately.
  cp "${BASH:-/bin/bash}" "$GT/agents/claude/2.1.229/claude"
  gtmux new-session -d -x 200 -y 60 \
    "$GT/agents/claude/2.1.229/claude -c 'while :; do sleep 1; done'" 2>/dev/null
  # The reopened popup inherits the SERVER's environment, so the fixture's
  # settings have to live there rather than in the first popup's own.
  for v in "XDG_RUNTIME_DIR $GT/run" "TAIMUX_REFRESH 1" "TAIMUX_SEARCH 0" \
           "TAIMUX_SESSIONS 0" "TAIMUX_REMOTE 0"; do
    # shellcheck disable=SC2086
    gtmux set-environment -g $v
  done
  script -q -c "tmux -f /dev/null -L $GSOCK attach" /dev/null > "$GT/client" 2>&1 &
  GCLIENT=$!
  sleep 2
  GTTY="$(gtmux list-clients -F '#{client_tty}' 2>/dev/null | head -1)"
  # The widest bottom border the picker has drawn: its window's width.
  gwidest() {
    sed 's/\x1b\[[0-9;?]*[a-zA-Z]//g' "$GT/client" 2>/dev/null |
      grep -o '└[^┘]*┘' | awk '{ print length($0) }' | sort -n | tail -1
  }
  galive() { pgrep -c -f "$GT/bin/taimux pick" 2>/dev/null || echo 0; }
  if [ -z "$GTTY" ]; then
    skip "a terminal that grows (no client attached; script may be restricted here)"
  else
    stty -F "$GTTY" cols 80 rows 24 2>/dev/null
    sleep 1
    # Opened exactly as the binding opens it, marker included.
    gtmux display-popup -E -e TAIMUX_POPUP=1 -w 100% -h 90% "$GT/bin/taimux pick" 2>/dev/null &
    n=0; while [ "$n" -lt 60 ] && [ "$(galive)" = 0 ]; do n=$((n+1)); sleep 0.1; done
    # At least one: the popup runs the command through a shell, so the pattern
    # matches that wrapper as well as the binary.
    r=0; [ "$(galive)" -ge 1 ] || r=1
    eq  "the picker is up in a popup"        "0" "$r"
    small="$(gwidest)"
    ok  "…drawn at the width of an 80-column terminal (${small:-?})"

    # Now GROW the terminal, which is the case tmux never handles.
    stty -F "$GTTY" cols 160 rows 45 2>/dev/null
    n=0; while [ "$n" -lt 100 ] && [ "$(gwidest)" = "$small" ]; do n=$((n+1)); sleep 0.1; done
    big="$(gwidest)"
    r=0; [ "${big:-0}" -gt "${small:-0}" ] 2>/dev/null || r=1
    eq  "the popup comes back wider when the terminal grows" "0" "$r"
    [ "$r" = 0 ] || no "…it stayed at $small" "expected more than $small, got ${big:-none}"
    # …and it is a LIVE picker, not a frame left on screen by the old one.
    n=0; while [ "$n" -lt 60 ] && [ "$(galive)" = 0 ]; do n=$((n+1)); sleep 0.1; done
    r=0; [ "$(galive)" -ge 1 ] || r=1
    eq  "…and a picker is running in it"     "0" "$r"
    # 80% of 160 columns, less the border: the geometry the binding would use at
    # this width, not the one the old popup was opened with.
    r=0; [ "${big:-0}" -ge 120 ] 2>/dev/null || r=1
    eq  "…at the geometry the binding would choose now" "0" "$r"
  fi
  gtmux kill-server 2>/dev/null
  kill "$GCLIENT" 2>/dev/null
  pkill -f "$GT/bin/taimux" 2>/dev/null
fi

# ============================================================================
section "the picker's mouse: the wheel, a click, and a double-click"
# ============================================================================
# fzf handled the mouse by default and the port never asked the terminal for
# mouse events at all, so the wheel and the click were both dead. Same shape as
# the page keys above: nothing referenced the behaviour, so nothing pointed at
# its absence.
#
# tmux has no "send a click", so this injects the report a terminal would have
# sent: `ESC [ < Cb ; Cx ; Cy M`, with Cb 0 for a left press, 64 and 65 for the
# wheel, and Cx/Cy 1-based. Its own server again, `-f /dev/null` again.
MSOCK="taimux-mousetest-$$"
mtmux() { tmux -f /dev/null -L "$MSOCK" "$@"; }
if [ ! -x "$KBIN" ]; then
  skip "the picker's mouse (no taimux built; run just build)"
elif ! command -v tmux >/dev/null 2>&1; then
  skip "the picker's mouse (no tmux here to drive it in)"
else
  MT="$TMP/mouse"; mkdir -p "$MT/run"
  cp "$KT/feed" "$MT/feed"

  mcursor() {
    mtmux capture-pane -p 2>/dev/null |
      sed -n 's/.*▶ .*\(row-[0-9][0-9]\).*/\1/p' | head -n 1
  }
  mwait() {  # $1 = the row wanted
    local n=0
    while [ "$n" -lt 60 ]; do
      [ "$(mcursor)" = "$1" ] && return 0
      n=$((n+1)); sleep 0.05
    done
    return 1
  }
  mexpect() { if mwait "$2"; then ok "$1"; else no "$1" "expected [$2] got [$(mcursor)]"; fi; }
  # The bytes themselves, one hex value per character of the report.
  sgr() {   # $1 = everything after the ESC
    local h=1b i
    for (( i=0; i<${#1}; i++ )); do h="$h $(printf '%02x' "'${1:i:1}")"; done
    # shellcheck disable=SC2086
    mtmux send-keys -H $h 2>/dev/null
  }
  click() { sgr "[<0;10;$1M"; sgr "[<0;10;$1m"; }   # $1 = the screen row

  mtmux new-session -d -x 100 -y 24 \
    "TAIMUX_SELF=$MT/feed TAIMUX_SEARCH=0 TAIMUX_SESSIONS=0 TAIMUX_REMOTE=0 \
     XDG_RUNTIME_DIR=$MT/run $KBIN tui >$MT/chosen 2>$MT/err" 2>/dev/null
  mexpect "the picker opens on the first row" "row-01"

  # The list occupies screen rows 4 to 11 of a 24-line window.
  sgr '[<65;10;7M'; mexpect "the wheel moves the cursor down"  "row-02"
  sgr '[<65;10;7M'; mexpect "…one row per notch"               "row-03"
  sgr '[<64;10;7M'; mexpect "…and back up"                     "row-02"
  # It wraps, because the wheel IS Up and Down: stopping where the key it
  # stands in for cycles would make it the odd one out.
  sgr '[<64;10;7M'; mwait row-01
  sgr '[<64;10;7M'; mexpect "the wheel wraps at the top"       "row-40"
  sgr '[<65;10;7M'; mexpect "…and at the bottom"               "row-01"

  click 7; mexpect "a click puts the cursor on the row clicked" "row-04"
  # A click BELOW the last row must land on nothing rather than off the end, so
  # the list has to be shorter than the area it is drawn in.
  mtmux send-keys 'row-07' 2>/dev/null; mwait row-07
  click 9; sleep 0.3
  eq "a click past the last row is ignored"  "row-07" "$(mcursor)"
  mtmux send-keys C-u 2>/dev/null; mwait row-01
  click 3; sleep 0.3
  eq "a click above the list is ignored"     "row-01" "$(mcursor)"

  # Two clicks on one row, far enough apart, are two single clicks.
  click 6; mwait row-03
  sleep 0.7
  click 6; sleep 0.4
  eq "two slow clicks do not accept"  "" "$(cat "$MT/chosen" 2>/dev/null)"
  r=0; mtmux has-session 2>/dev/null || r=1
  eq "…and the picker is still up"    "0" "$r"

  # Together, they accept, which is fzf's double-click and the reason this
  # section ends here: the picker exits.
  click 6; click 6
  n=0; while [ "$n" -lt 60 ] && [ ! -s "$MT/chosen" ]; do n=$((n+1)); sleep 0.05; done
  eq "a double-click accepts that row" "%03" "$(tr -d '\n' < "$MT/chosen" 2>/dev/null)"
  eq "…and said nothing on stderr"     ""    "$(cat "$MT/err" 2>/dev/null)"
  mtmux kill-server 2>/dev/null
fi

# ============================================================================
section "the preview: shift-up and shift-down scroll it"
# ============================================================================
# fzf points these at preview-up and preview-down. Here the preview is a window
# onto something longer: a live pane's whole screen, or the last turns of an
# ended conversation. The default view is anchored at the BOTTOM for a pane,
# because what a session is doing is the last thing on it.
#
# This one needs a real pane with real content behind the row, so it makes one
# rather than feeding synthetic rows alone. Two traps, both met while writing
# it, both invisible in the result:
#   · sending the lines to `cat` makes the pane hold each one TWICE, because the
#     terminal echoes the input and cat prints it again;
#   · the pane id starts with `%`, so it goes into printf as an ARGUMENT. In the
#     format string, printf eats it and the row silently disappears.
SSOCK="taimux-scroll-$$"
stmux() { tmux -f /dev/null -L "$SSOCK" "$@"; }
if [ ! -x "$KBIN" ]; then
  skip "the preview's scroll (no taimux built; run just build)"
elif ! command -v tmux >/dev/null 2>&1; then
  skip "the preview's scroll (no tmux here to drive it in)"
else
  ST="$TMP/scroll"; mkdir -p "$ST/run"
  stmux new-session -d -s host -x 100 -y 40 \
    'i=1; while [ $i -le 38 ]; do echo "line-$i"; i=$((i+1)); done; sleep 999' 2>/dev/null
  sleep 0.8
  SPANE="$(stmux list-panes -t host -F '#{pane_id}' 2>/dev/null | head -1)"
  {
    printf '#!/usr/bin/env bash\n[ "${1:-}" = _panes ] || exit 1\n'
    printf "printf '%%s\\\\thost:1.1\\\\t/tmp/a\\\\tclaude\\\\t2.1.229\\\\tidle\\\\t-\\\\trow-01\\\\n' '%s'\n" "$SPANE"
    printf "printf '%%s\\\\thost:9.9\\\\t/tmp/b\\\\tclaude\\\\t2.1.229\\\\tidle\\\\t-\\\\trow-02\\\\n' '%%999'\n"
  } > "$ST/feed"
  chmod +x "$ST/feed"

  stmux new-window -d -n pick \
    "TAIMUX_SELF=$ST/feed TAIMUX_SEARCH=0 TAIMUX_SESSIONS=0 TAIMUX_REMOTE=0 \
     XDG_RUNTIME_DIR=$ST/run $KBIN tui >$ST/chosen 2>$ST/err" 2>/dev/null

  # The first and last screen line the preview is showing.
  slo() { stmux capture-pane -p -t pick 2>/dev/null | grep -o 'line-[0-9]*' | head -1; }
  shi() { stmux capture-pane -p -t pick 2>/dev/null | grep -o 'line-[0-9]*' | tail -1; }
  swait() {
    local n=0
    while [ "$n" -lt 60 ]; do
      [ "$(slo)" = "$1" ] && return 0
      n=$((n+1)); sleep 0.05
    done
    return 1
  }
  sexpect() { if swait "$2"; then ok "$1"; else no "$1" "expected [$2] got [$(slo)]"; fi; }
  skey() { stmux send-keys -t pick "$1" 2>/dev/null; }

  swait line-21
  eq "the preview opens on the END of a pane's screen" "line-38" "$(shi)"
  skey S-Up
  sexpect "shift-up moves it one line towards the start" "line-20"
  for _ in 1 2 3 4 5; do skey S-Up; done
  sexpect "…one line per press, no more"                 "line-15"
  # Well past the top. It must stop, AND the offset must stop with it: an
  # offset that keeps counting past the end takes as many presses to come back,
  # with nothing moving on screen for any of them.
  for _ in $(seq 1 25); do skey S-Up; done
  sexpect "it stops at the top rather than wrapping"     "line-1"
  skey S-Down
  sexpect "…and ONE shift-down comes straight back"      "line-2"
  # The offset belongs to the row it was measured against.
  for _ in 1 2 3 4 5 6 7 8; do skey S-Up; done
  swait line-1
  skey Down; sleep 0.3
  skey Up
  swait line-21
  eq "moving the cursor puts the preview back"           "line-38" "$(shi)"
  eq "…and nothing was written to stdout"                "" "$(cat "$ST/chosen" 2>/dev/null)"
  stmux kill-server 2>/dev/null
fi

# ============================================================================
section "layout: the frozen golden file, which is the specification now"
# ============================================================================
# tests/golden/rows.expected is the bash layout's output over a fixed fixture,
# captured 2026-09-02 immediately before the fzf code was deleted: 504 cases over
# 7 widths, 3 cursor positions, 6 state filters and 4 queries. It cannot be
# regenerated, which is exactly why it exists. The 440-combination differential
# that proved the port had no other way to outlive the implementation it was
# measured against, and without this the layout's only remaining definition would
# be the code that implements it.
GBIN="$HERE/../target/release/taimux"
if [ -x "$GBIN" ]; then
  GOUT="$TMP/golden.actual"
  TAIMUX_DAEMON_BIN="$GBIN" "$HERE/golden/replay.sh" > "$GOUT" 2>/dev/null
  r=0; cmp -s "$GOUT" "$HERE/golden/rows.expected" || r=1
  eq "the laid-out rows are byte-identical to the frozen bash output" "0" "$r"
  [ "$r" = 0 ] || diff "$HERE/golden/rows.expected" "$GOUT" | head -12
  eq "…over every case the differential covered" "504" \
     "$(grep -c '^###' "$HERE/golden/rows.expected")"
else
  skip "golden layout replay (no taimux built; run just daemon-build)"
fi

# ============================================================================
printf '\n────────────────────────────\n'
if [ "$FAIL" -eq 0 ]; then
  printf '\033[32mall %d checks passed\033[0m\n' "$PASS"
  exit 0
else
  printf '\033[31m%d passed, %d FAILED\033[0m\n' "$PASS" "$FAIL"
  exit 1
fi
