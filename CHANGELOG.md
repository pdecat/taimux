# Changelog

## [0.13.0](https://github.com/pdecat/taimux/compare/v0.12.0...v0.13.0) (2026-09-20)


### Features

* open on the nearest session when the pane has no agent ([ae79d04](https://github.com/pdecat/taimux/commit/ae79d043c467879730c18a3d2b3298cf7a6c7c14))

## [0.12.0](https://github.com/pdecat/taimux/compare/v0.11.1...v0.12.0) (2026-09-20)


### Features

* carry a conversation into a different agent ([afec72d](https://github.com/pdecat/taimux/commit/afec72d05b09521428d9d2c053fd5e8cea97ad2d))
* list every agent's past conversations, not just claude's ([27bf638](https://github.com/pdecat/taimux/commit/27bf6387de78eb9d44410e6feb49c227593e4a9c))


### Bug fixes

* **ci:** the musl build needs a C compiler now, and a cache that knows it ([b5b80e0](https://github.com/pdecat/taimux/commit/b5b80e04a3965e0cd3b0fe48cba40df204e96311))
* **just:** `ship` linked the launcher to itself and failed after landing ([c7cedd2](https://github.com/pdecat/taimux/commit/c7cedd27cd2177a13ea23162d223586e26c7b314))
* **rows:** a summary too long for the window took the table with it ([36126a2](https://github.com/pdecat/taimux/commit/36126a27612ab382620db8be7925ed493ac5d127))


### Performance

* read a JSON string in one pass, not three over the line ([1752187](https://github.com/pdecat/taimux/commit/17521876266f4055f4545b9d13430f55e4cd1f14))
* read the SQLite stores with rusqlite, not turso ([e87c094](https://github.com/pdecat/taimux/commit/e87c094b1bd83bc1844f8fd5e6a11937201165f9))

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
