# Changelog

## [0.18.0](https://github.com/pdecat/taimux/compare/v0.17.0...v0.18.0) (2026-09-24)


### Features

* ctrl-s sorts idle sessions by their last message ([88d7eb1](https://github.com/pdecat/taimux/commit/88d7eb1f9d6465af0e1226ec4df7f29d19595a57))
* ctrl-s sorts past sessions by date ([277536d](https://github.com/pdecat/taimux/commit/277536d0935248f157a72ae28191ff3c3ac0ea87))


### Bug fixes

* a daemon left running by an earlier build is sent away ([51ca605](https://github.com/pdecat/taimux/commit/51ca6056bc93f59e32b384badaacedb814e099e8))
* a scan that lands after Tab no longer rewrites the past list ([e72e58a](https://github.com/pdecat/taimux/commit/e72e58aa14e0cf78d44c3e614a1911b14cab9d64))

## [0.17.0](https://github.com/pdecat/taimux/compare/v0.16.0...v0.17.0) (2026-09-22)


### Features

* taimux state explains why a pane reads the way it does ([dcd4e49](https://github.com/pdecat/taimux/commit/dcd4e499aed221edc12c946935081338712c3cf8))


### Bug fixes

* the state follows interrupts, background work and hidden dialogs ([58e303f](https://github.com/pdecat/taimux/commit/58e303f5c82146485b212f05adfe6af89c1a0fbd))

## [0.16.0](https://github.com/pdecat/taimux/compare/v0.15.1...v0.16.0) (2026-09-21)


### Features

* a URL survives the tool call it was an argument to ([6e473f1](https://github.com/pdecat/taimux/commit/6e473f19d38fcdf6dcff337c1d67291852a7ec37))
* shift-tab walks the list ring backwards ([d5a64af](https://github.com/pdecat/taimux/commit/d5a64af1f2fb63852b9527d7e856ca9e136237b8))
* the picker ignores case, whichever side carries the capital ([54cadbb](https://github.com/pdecat/taimux/commit/54cadbbd9c2fb4c5159cd4c09042cd8e6b8a1e0b))

## [0.15.1](https://github.com/pdecat/taimux/compare/v0.15.0...v0.15.1) (2026-09-20)


### Bug fixes

* a pane too short to show a prompt box is read, not skipped ([62c8f01](https://github.com/pdecat/taimux/commit/62c8f0101780c2e93b94a462157248949a3dbe9a))

## [0.15.0](https://github.com/pdecat/taimux/compare/v0.14.0...v0.15.0) (2026-09-20)


### Features

* the picker starts its own daemon, the way atuin does ([854c91a](https://github.com/pdecat/taimux/commit/854c91a7a4161060b243af3934046ce0f110a5ab))

## [0.14.0](https://github.com/pdecat/taimux/compare/v0.13.0...v0.14.0) (2026-09-20)


### Features

* refresh the list once a second, not once every three ([ae6eb59](https://github.com/pdecat/taimux/commit/ae6eb5989370102d151c16d24ffccd3fa24cbd36))
* the picker asks a running daemon for its rows ([6ec5df4](https://github.com/pdecat/taimux/commit/6ec5df44b866c01201fa749beec4854e6e9aab08))

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
