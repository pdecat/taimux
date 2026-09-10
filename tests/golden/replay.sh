#!/usr/bin/env bash
# Replay the golden layout cases against taimux and print what it produced, in
# the same shape as tests/golden/rows.expected.
#
# That expected file is bash `_fzf_rows` output, frozen 2026-09-02 just before the
# fzf code was deleted. It cannot be regenerated, and that is the point: it is the
# layout specification now, and the 440-combination differential that produced it
# has no other way to outlive the implementation it was measured against.
set -u
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BIN="${TAIMUX_DAEMON_BIN:-$HERE/../../target/release/taimux}"

# Refuse to run against a binary that is not there, rather than replaying
# nothing and letting `cmp` decide what that means. This caught a real one: the
# variable was still spelled for a former name of the tool while the caller had
# moved on, so the fallback path was taken silently and it happened to find a
# STALE binary of that former name left in target/. The golden file compared
# clean against a build nobody had made for hours.
if [ ! -x "$BIN" ]; then
  echo "replay.sh: no binary at $BIN (build it, or set TAIMUX_DAEMON_BIN)" >&2
  exit 1
fi
FIX="$HERE/rows.tsv"
HOME_FIX=/home/testuser
SNIPS=$'%11\t…we rewrote the auth layer here…\n%12\t…nothing to do with it…\nha:%6\t…said something remote…'

sed -n '1,3p' "$HERE/rows.expected"
for width in 0 39 60 99 100 130 200; do
  for cur in '' %13 'ha:%6'; do
    for only in '' input run idle dead note; do
      for q in '' auth 'auth layer' AUTH; do
        printf '### width=%s cur=%s only=%s q=%s newver=%s\n' \
               "$width" "${cur:-.}" "${only:-.}" "${q:-.}" 2.1.258
        # The short-query gate lives in the CALLER, not in the layout: under
        # TAIMUX_SEARCH_MIN characters no snippets are looked up at all. The
        # golden file was generated with that gate applied, so it is applied here.
        snips=''
        [ "${#q}" -ge "${TAIMUX_SEARCH_MIN:-3}" ] && snips="$SNIPS"
        TAIMUX_Q="$q" TAIMUX_SNIPS="$snips" \
          "$BIN" rows --cur "$cur" --width "$width" --only "$only" \
                      --home "$HOME_FIX" --newver 2.1.258 < "$FIX"
      done
    done
  done
done
