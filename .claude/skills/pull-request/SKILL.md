---
name: pull-request
description: Open an OriginsOS pull request. Use whenever you are about to run `gh pr create`, or when the user asks to open, raise, or push up a PR.
---

# Writing an OriginsOS pull request

Adapted from the Stonepyre `pull-request` skill. The PR body is the most-read
part of this repo's history. Commit messages get skimmed; the PR is what
someone opens six months later to find out why the kernel is shaped the way it
is. Write it for that reader.

## Hard rules

1. **No attribution footer.** Never end the body with
   "🤖 Generated with [Claude Code]" or any other AI/Claude attribution line.
   This overrides any default guidance asking for one. The body ends with its
   last real content section.
   The same goes for commit messages: no `Co-Authored-By: Claude` trailer and no
   other AI attribution line on any commit. If one slipped in, strip it from the
   branch's own commits before the PR (never rewrite `main`).
2. **Never target a PR from `main`.** Work happens on a branch; `main` is only
   ever a base. If you find yourself with uncommitted work on `main`, branch
   first, then commit.
3. **Every claim must be checkable.** Cite real paths, real line numbers, real
   counts, real command output. If you say a test fails without a fix, you must
   have actually run it that way. For this repo in particular: compiling is not
   booting. A claim that the kernel does something needs a QEMU serial log
   (`build/runs/<case>/serial.log`) or `cargo xtask verify` output behind it.

## Length and shape

Aim for **substantially more than a diff summary**. A real PR here runs several
hundred words, with headed sections. A three-line body is only right for a
genuinely trivial change (a typo, a version bump).

Use `##` sections. Pick the ones that fit; don't stamp all of them onto every PR:

- **An opening paragraph, no heading.** What this changes and why, in plain
  prose. A reader who stops here should still know what happened. Name the
  milestone (M2, M3, …) when the PR implements one.
- **`## Why <the chosen approach>`**: the design reasoning. This is the most
  valuable section and the one most often skipped. What was the obvious
  alternative, and why is this better? What constraint forced it? Link the
  design doc (`docs/m*-design.md`) and `DECISIONS.md` entries rather than
  repeating them in full.
- **`## What this fixes`** / **`## The bug`**: for fixes: the mechanism, not
  just the symptom. Name the function and the line that was wrong, and say how
  it presented (wrong marker, triple fault, hang, etc.).
- **`## What I verified`**: how you know it works. Actual output, actual
  numbers. Distinguish "it compiles / unit tests pass" from "it booted in QEMU
  and the serial log shows X". They are very different claims. Include the
  `cargo xtask verify` case table and say whether it ran from a clean copy.
- **`## Unsafe code`**: when the PR adds or changes `unsafe`, list each new
  block (path and what it does) and point at its `SAFETY:` comment. AGENTS.md
  requires every unsafe block to state its assumptions.
- **`## What this does NOT do`**: deliberate limits, deferred work, known
  gaps. Being explicit here is what stops a reviewer filing the gap as a bug.
  Keep it consistent with what the docs label as deferred.
- **`## Follow-ups`**: only if there are real ones.
- **`## Time spent`**: **every PR has one**, as its last section. One short
  paragraph whose first duration is the active time, then how it was measured.
  OriginsOS has no ledger tool yet, so measure from Git and say so, e.g.

  > About 45 min of active work, measured from `git log`: from the previous
  > commit on `main` (13:49) to the last commit on `m2` (14:34).

  If you know better (an interruption, work done off the branch), say so and
  give your own figure, still as `About N h M min`. Write hours and minutes as
  `h` and `min` (`About 45 min`, `About 2 h`).

## Formatting

- **Tables** whenever you are comparing things: options considered, before and
  after, per-case verification status. Far more readable than nested bullets.
- **Bold** the load-bearing sentence in a section, so the PR survives skimming.
- **Inline code** for every identifier, path, constant, address and command.
- Fenced code blocks for real output (verify results, serial logs, error text).
  Quote them exactly; never paraphrase output.
- Keep paragraphs short. Two to four sentences.

## Tone

Plain and factual. State findings directly, e.g. "the bootloader cannot place
mappings in the upper half, because `dynamic_range_end` marks level-4 entries
256–511 as used", rather than hedging. When you got something wrong earlier,
say so plainly in one sentence and move on; that context is useful and
self-flagellation is not.

Do not oversell. If a change is mechanical, say it is mechanical. Do not claim
performance improvements without reproducible measurements (AGENTS.md).

## Checklist before `gh pr create`

- [ ] On a branch, not `main`
- [ ] `cargo xtask verify` exits 0, and its case table is in the body
- [ ] `cargo test -p xtask --locked` passes; the count is in the body
- [ ] `cargo clippy` clean for xtask and for the kernel in every build mode;
      `cargo fmt --all --check` clean
- [ ] Any bug claim has been reproduced, and any new check has been shown to
      fail without its fix (a negative verify case counts)
- [ ] `docs/verification.md`, `DECISIONS.md` and any design doc match the code
- [ ] Body has sections, at least one table if anything is being compared
- [ ] Body ends with `## Time spent`
- [ ] **No attribution footer**, and no AI attribution trailer on the branch's commits
