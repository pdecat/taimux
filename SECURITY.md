# Security

taimux reads your coding agents' conversations and can run a binary on other
machines over ssh. Both are worth understanding before you install it, so this
describes what it actually touches rather than only where to send a report.

## Reporting a vulnerability

Use GitHub's [private vulnerability
reporting](https://github.com/pdecat/taimux/security/advisories/new) on this
repository. That opens a private advisory only maintainers can see.

Please do not open a public issue for anything exploitable. There is no bounty
and no SLA: this is one person's tool, and you will get an honest answer rather
than a fast one.

## What it reads

- **`/proc`**, to find which process each tmux pane is running and which
  version of the agent that process actually is. Only your own processes.
- **tmux**, via `list-panes` and `capture-pane`. Reading a pane's screen is how
  the state column knows a session is waiting for you.
- **agent transcript files on disk**, under the agent's own config directory
  (for Claude Code, `~/.claude/projects/…`), to show what a session is about and
  to search what it said.
- **`$XDG_RUNTIME_DIR/taimux/`**, where the optional turn-boundary hook leaves
  one line per pane.

## What leaves your machine

Nothing, except over ssh, and only to hosts you are already connected to.

There is no telemetry, no analytics, no crash reporting and no network call of
any kind on the path that draws the picker. The only outbound traffic the
project makes at all is `scripts/install-release.sh` fetching a release from
GitHub, which you run deliberately.

**The multi-host feature runs `taimux list` over ssh**, on hosts discovered from
your own panes: if a pane is sitting in `ssh somebox -t tmux attach`, that box
gets asked about itself. It is not configured and cannot be pointed anywhere
you are not already logged in. The rule the implementation follows is *federate,
never reach in*: each host is asked about itself and answers about its own
panes only, so nothing chains from one machine through another.

## What is deliberately not indexed

The search index is built from prose: what you typed, and what the agent wrote
back. **Tool results are excluded**, which matters more for privacy than for
size, because a tool result is the content of whatever file the agent just read.

That exclusion is a tested property, not an intention. `core/src/transcript.rs`
drops any record carrying `toolUseResult`, and both the unit suite and the
integration suite feed it a transcript containing the sentinel
`SECRETFILEBODY` inside a tool result and assert the string does not appear in
the indexed output.

Also excluded: subagent sidechains, harness-injected turns, and
`<system-reminder>` blocks.

The index lives under `$XDG_CACHE_HOME/taimux` with your user's permissions. It
is a cache and can be deleted at any time.

## Things worth knowing before you trust it

- **Reading a pane's screen means the picker sees whatever is on that screen.**
  If an agent has just printed a secret, the state reader has looked at it. It
  is not stored, but the preview will show it, exactly as your terminal does.
- **`restart` and `resurrect` reconstruct command lines** and, in the case of
  `restart`, type into live panes. They refuse to act on a pane that is busy or
  has a draft in its prompt, and `restart -n` prints the plan without doing
  anything. Read the plan the first time.
- **The release binary is not signed.** It is built by GitHub Actions from the
  tagged commit, and the workflow refuses to publish an artefact that is not
  statically linked or whose `taimux version` disagrees with the tag. That
  guards against a mistake, not against a compromised runner.

## Supported versions

The latest release. This is a young project with one maintainer; fixes go
forward, not back.
