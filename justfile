# taimux tasks: run `just` (or `just --list`) to see recipes.

set shell := ["bash", "-eu", "-c"]

# What is left of the shell: the plugin entry point, the release downloader, the
# test harness and the demo. taimux itself is a Rust workspace: core/ has no
# external dependencies, daemon/ adds the socket and the indexer, cli/ is the
# only crate that links a terminal library, and src/ is the one binary.
scripts := "taimux.tmux scripts/install-release.sh tests/run.sh demo/demo.sh"
bin := "target/release/taimux"

# list available recipes
default:
    @just --list

# build it. everything else needs this first
build:
    cargo build --release

# One artefact for every host, and no glibc version to match: 2.43 on one box
# and 2.41 on another, so a plain build here does not run there. CI builds it
# refuses to publish it if it comes out dynamically linked.
# Needs once: rustup target add x86_64-unknown-linux-musl

# build one statically linked binary that runs on any x86_64 Linux
build-static:
    cargo build --release --target x86_64-unknown-linux-musl
    @ls -la target/x86_64-unknown-linux-musl/release/taimux

# How a host is SUPPOSED to get taimux: release-please cuts a release from the
# commit messages, CI attaches the static binary to it, and this downloads that.
# `--force` re-downloads when the version already matches. The dotfiles installer
# calls the same script, so this is also how to test what it will do.

# install the latest release here, the way every host gets it
install-release *args:
    ./scripts/install-release.sh {{ args }}

# The way to test an UNRELEASED build on another host. Everyday installs go
# through `install-release` above; this exists for the change that is not
# committed yet. It lands at ~/.local/bin/taimux with the launcher symlinked at
# it, which is exactly where the release path puts it too.

# There used to be an `ln -sfn` here linking the launcher at the binary. Both
# sides of it became the same path in the jumpmux -> taimux rename, so it was
# `ln`-ing a file to itself, which errors and (with `set -e`) failed the recipe
# AFTER the binary had already landed. The launcher symlink is `taimux install`'s
# job on each host anyway, not this one's.

# push the working-tree build to a host, e.g. `just ship ha`
ship host: build-static
    scp -q target/x86_64-unknown-linux-musl/release/taimux \
        {{ host }}:/tmp/taimux.new
    ssh {{ host }} 'set -e; mkdir -p ~/.local/bin; \
        mv -f /tmp/taimux.new ~/.local/bin/taimux; \
        chmod +x ~/.local/bin/taimux; \
        printf "%s: %s rows from list\n" "$(hostname)" "$(~/.local/bin/taimux list | wc -l)"'

# Not the same as `install-release`: this is the tip of main, which is AHEAD of
# the latest release whenever anything has landed since. Useful for checking what
# the next release will contain; not how a host should be installed.

# download the static artefact from the last green CI run on main
fetch-artefact:
    gh run download --repo pdecat/taimux \
        "$(gh run list --repo pdecat/taimux --workflow build --branch main \
            --status success --limit 1 --json databaseId -q '.[0].databaseId')" \
        -n taimux-x86_64-static -D /tmp/taimux-ci
    @chmod +x /tmp/taimux-ci/taimux && ls -la /tmp/taimux-ci/taimux

# the crate's own tests: parsing, layout, the ladder, the guards
unit:
    cargo test --workspace --quiet

# integration: the binary, in fixtures, asserted on what it produced
test: build
    bash tests/run.sh

# --all-targets so the TESTS are linted too: without it clippy never sees the
# test modules, and adding it immediately found two files with code appended
# after one, which is a real ordering smell rather than a nit.
#
# .github/workflows/ci.yml runs these same commands rather than calling `just`,
# so that workflow stays readable and needs nothing installed to run them. Keep
# the two in step.

# fmt + clippy on the crate, bash -n + shellcheck on what shell is left
lint:
    cargo fmt --all --check
    cargo clippy --workspace --release --all-targets --quiet -- -D warnings
    for f in {{ scripts }}; do bash -n "$f"; done
    shellcheck -S warning {{ scripts }}

# everything, run this before committing
check: lint unit test

# install the launcher + tmux bindings (prefix+a and F1)
install: build
    ./{{ bin }} install

# register the turn-boundary hook in Claude Code's settings.json
install-hooks: build
    ./{{ bin }} install-hooks

# print detected agent sessions (machine-readable, tab-separated)
list: build
    ./{{ bin }} list-local

# print which conversation each claude pane is on (read-only; restarts nothing)
print-cmds: build
    ./{{ bin }} print-cmds

# one indexer pass, now, whatever the rate limit says
index: build
    ./{{ bin }} index --force

# interactive demo: isolated tmux server with synthetic agent sessions
demo: build
    ./demo/demo.sh

# non-interactive, leak-free demo snapshot (rows, a preview, print-cmds)
snapshot: build
    ./demo/demo.sh --snapshot

# re-record the README's demo.gif (needs vhs + ttyd)
demo-record: build
    vhs demo/demo.tape
    @ls -la demo/demo.gif

# tear down a leftover demo server
demo-clean:
    ./demo/demo.sh --clean
