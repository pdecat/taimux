#!/usr/bin/env bash
# tpm / tpack entry point. A tmux plugin manager runs every executable `*.tmux`
# file at the root of a plugin's checkout, on every tmux launch and reload, so
# this is taimux's install step when it is installed that way:
#
#   set -g @plugin 'pdecat/taimux'      # in ~/.tmux.conf(.local), then prefix+I
#
# All it does is bind the picker in the RUNNING server, by this checkout's own
# absolute path, so nothing has to be on PATH and nothing is written anywhere:
# removing the plugin removes the plugin, with no config block left behind.
#
# Keys, read from tmux options at bind time, so they belong in the same config
# that loads the plugin (set them BEFORE the plugin manager's run line):
#
#   set -g @taimux-key       'a'      # prefix binding, '' to not bind one
#   set -g @taimux-root-key  'F1'       # no-prefix binding, '' likewise
#
# taimux is a compiled binary (`taimux`), so a plugin checkout has to be built
# once before it can bind anything. That is deliberately NOT done here: a plugin
# manager runs this on every launch and reload, and a cargo build on the path
# that starts your tmux is not a trade worth making. It says what to run instead.
#
# The rest of taimux is a CLI (`restart`, `resurrect`, `print-cmds`,
# `install-hooks`, and the `list` another host's picker asks for over ssh), none
# of which a key binding can reach. `taimux install` from this checkout puts it
# on PATH for those: run from a plugin checkout it symlinks and leaves your tmux
# config alone, since the bindings are already this file's job.
set -u

HERE="$(cd -- "$(dirname -- "$(readlink -f -- "${BASH_SOURCE[0]:-$0}")")" && pwd)"
BIN="$HERE/target/release/taimux"

if [ ! -x "$BIN" ]; then
  # A plugin manager swallows stdout, so this goes to stderr where it stays
  # visible: an unbuilt plugin binding nothing at all would otherwise be silent.
  printf 'taimux: not built. Run: cd %s && cargo build --release\n' \
         "$HERE/daemon" >&2
  exit 1
fi

# stdout only: a plugin manager runs this through `run-shell`, where a line of
# output can land in a tmux message or a window. Anything on stderr is a real
# failure and stays visible.
exec "$BIN" bind >/dev/null
