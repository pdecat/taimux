# Contributing

Thanks for looking. This is a small tool with a small surface, and most changes
are welcome. A few things here are load-bearing in ways that are not obvious
from reading the diff, so they are written down rather than left to be
discovered in review.

## The gate

```sh
just check
```

That is `lint` + `unit` + `test`: `cargo fmt --all --check`, clippy over the
whole workspace with `-D warnings`, ~270 unit tests, `shellcheck` on what shell
is left, and a 1800-line bash integration suite that drives the real binary in
fixtures. CI runs the same commands rather than calling `just`, so the two have
to be kept in step.

Everything must be green before you push. If a check is wrong, change the check
and say why in the commit.

## Commit messages decide releases

[Conventional Commits](https://www.conventionalcommits.org/), and not as a
style preference: release-please reads them to work out the next version.

| type | effect |
|---|---|
| `feat`, `fix`, `perf`, `refactor` | **cuts a release** |
| `docs`, `test`, `ci`, `build`, `chore`, `style` | rides along in the next one |

So labelling a typo fix `feat:` ships a version, and labelling a real fix
`chore:` means nobody gets it. `refactor` is deliberately in the releasing list,
because the change that replaced the entire bash implementation was a refactor
and a host needed it.

Breaking changes take a `!` and a `BREAKING CHANGE:` footer.

## Three things that will bite you

**`tests/golden/rows.expected` cannot be regenerated.** It is 504 cases of
frozen bash output, captured immediately before the bash implementation was
deleted, and it is now the layout specification: the suite replays it through
the current binary and demands byte equality with `cmp -s`. If your change reds
it, the question is whether the new layout is correct, and the answer belongs in
the commit message. Do not overwrite the file to make the test pass. Doing that
converts the only independent description of the layout into a copy of whatever
the code currently does.

**The comments carry the reasoning.** Many of them record something that was
measured, or a wrong turn that cost real time to find. A patch that strips them
as noise is not a cleanup and will be asked to put them back. If a comment is
*wrong*, that is worth fixing on its own.

**The crate boundaries are enforced, not conventional.** `core` has zero
external dependencies, `daemon` depends only on `core`, and `cli` is the only
crate allowed to link a terminal library:

```
core/     pane scanning, transcripts, agent versions, paths.  No deps at all
daemon/   the socket, the protocol, the background indexer
cli/      the picker, layout, restart, resurrect, ssh federation
src/      the dispatcher, and the one binary it all ships as
```

`cargo tree -p taimux-daemon` prints two lines, and it should stay that way. If
something in `core` needs to reach `cli`, the layering is wrong rather than the
rule.

## Layout and tooling

```sh
just              # list every recipe
just build        # everything else needs this first
just unit         # the crates' own tests
just test         # integration: the binary, in fixtures
just lint         # fmt + clippy + shellcheck
just demo         # try the picker against synthetic sessions, in an isolated
                  # tmux server, without touching your real ones
```

Rust is the only toolchain needed. `mise.toml` pins `just` and `rust` if you use
mise; otherwise a stable Rust and `just` are enough.

## Reporting something instead

A bug report that includes `taimux version`, `tmux -V`, which agent and its
version, and your distro is worth more than a patch that guesses. The issue
template asks for exactly those.

## AI-assisted contributions

Welcome, with conditions worth reading: see [AI_POLICY.md](.github/AI_POLICY.md).

## Licence

By contributing you agree that your work is dual licensed under MIT and
Apache-2.0, as described at the end of the README.
