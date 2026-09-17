# Changelog

## [0.11.1](https://github.com/pdecat/taimux/compare/v0.11.0...v0.11.1) (2026-09-17)


### Bug fixes

* a question whose own footer wrapped read as working ([af29254](https://github.com/pdecat/taimux/commit/af2925476183e535dcde1ff0afd813f19f5d3904))
* a session idle at the prompt no longer reads as working ([9e7a282](https://github.com/pdecat/taimux/commit/9e7a282e4374fcf4e70dde728a1c8a17448eee55))
* two guards that were not guarding, found by the same CI run ([6691eb3](https://github.com/pdecat/taimux/commit/6691eb34665c97c9e3fc192b42174bce6f644afe))

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
