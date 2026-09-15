# Changelog

## [0.11.0](https://github.com/pdecat/taimux/compare/v0.10.0...v0.11.0) (2026-09-15)


### Features

* report a turn from its tool calls, not just its boundaries ([ee72a99](https://github.com/pdecat/taimux/commit/ee72a9912e1b589625a0b3f1a01ec6e50d14133f))


### Bug fixes

* a working screen overrules a stale hook `idle` ([c0804e2](https://github.com/pdecat/taimux/commit/c0804e2bdd1481fa6dd42683160eeafb43c62ef5))
* **ci:** keep the workspace dependency versions in step with a release ([cbc5841](https://github.com/pdecat/taimux/commit/cbc5841fe7db2ba174c82709c0b463338f995277))

## [0.10.0](https://github.com/pdecat/taimux/compare/v0.9.0...v0.10.0) (2026-09-11)


### Features

* taimux, a picker for the AI coding-agent sessions in your tmux ([a3c73bf](https://github.com/pdecat/taimux/commit/a3c73bf9b9e97da50b6e8af64876b415dadf4688))

## Changelog

This repository starts at 0.9.0. That is not a typo and not a squashed
history: taimux was called jumpmux for its first two and a half months, and
that era lives in a separate, private archive rather than here. The version
continues rather than restarting because the binary was already deployed at
0.9.0 and a host should never watch its version go backwards.

Releases from here on are cut by
[release-please](https://github.com/googleapis/release-please) from the commit
messages, and land below.
