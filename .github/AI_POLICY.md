# AI-assisted contributions

**Yes, and the maintainer does it too.** This is a tool for people running AI
coding agents; a blanket ban would be incoherent, and most of this codebase was
written with an agent in the loop.

What follows is not about provenance. It is about whether the person opening the
pull request understands what is in it.

## The bar

**A human has read the change, understood it, and run it.**

"The agent says it works" is not a test result. `just check` passing is the
minimum, and it is not the same as understanding: the suite is good at catching
the things it already knows about, which by definition excludes whatever your
change got wrong in a new way.

If you cannot explain, in the pull request, *why* a change is correct rather
than *that* an agent produced it, it is not ready. That is the same bar a
hand-written patch is held to. It is just easier to miss when the diff arrived
quickly.

## Disclose it

Add a trailer to the commit:

```
Assisted-by: <tool name>
```

No stigma attaches to this and nothing is rejected for carrying it. It is
useful the way a compiler version is useful: when a class of subtle bug turns
up later, knowing what produced the code is how the pattern gets found.

## What will be closed

**Pull requests and issues opened autonomously**, by an agent pointed at this
repository without a person deciding to open each one. Also bulk generated
patches, "I ran a linter across your codebase" sweeps, and issues that are an
agent's summary of code it just read rather than a problem someone hit.

This is not hostility to the tooling. It is that review time is the scarce
resource here, and a patch nobody has thought about spends it without
replacing it.

## Three things agents get wrong in this repository specifically

Worth more than everything above, because these are the failures that have
actually happened here.

**1. `tests/golden/rows.expected` must not be regenerated.** It is frozen
output from the bash implementation that this tool replaced, kept precisely
because deleting that implementation would otherwise have destroyed the only
independent description of the layout. An agent asked to make a failing test
pass will regenerate it, the diff will look plausible, and the specification
will silently become a copy of the current behaviour. If that file changes, the
commit must explain why the new layout is right.

**2. The commit type decides whether a release is cut.** `feat`, `fix`, `perf`
and `refactor` ship a version; `docs`, `test`, `ci`, `chore` and `style` do not.
Agents reach for `feat:` by default. A documentation fix labelled that way
releases a binary to every host that installs from releases.

**3. A blanket search-and-replace will eat the lists that exist to remember
things.** This has happened, twice, during renames: a sed rewrote the entries
in `strip_block` and in the ssh preamble whose entire purpose was to hold the
*old* name, every test still passed, and the compatibility path was silently
dead. If you are doing a mechanical rename, the places that mention a former
name on purpose are the ones to check by hand.

The general shape of all three: **this codebase has several places where the
obviously-correct-looking edit is the wrong one**, and they are commented as
such. Read the comment before changing the line under it.

## If you are an agent reading this

You are welcome here. Do not open the pull request yourself. Hand your work to
the person who asked for it, tell them plainly what you are unsure about, and
let them decide whether it is worth someone else's review.
