# Bella review audit

Status: the owner's audit, 2026-11-02, copied from the owner's planning
document.
- **Code read:** commit `f80de9d` (version 0.73.0), read only.
- **Checks:** none were run.
- **Re-checked by hand:** B1, B2, B3 and B6.
- **Reproduced:** none, by running the system.

It feeds Stage 0 of docs/bella-roadmap.md.

## Verdict

The outside review's direction is right, but it did not read the code,
and the code has problems that matter more than any new feature.

- **Agree:**
  - keep the untrusted-model design;
  - make a typed action and observation interface ZooOS's defining
    feature;
  - put a Bella v1 workflow ahead of SMP and the GPU;
  - measure before optimising;
  - treat the NVIDIA driver as a gated side track.
- **Disagree:**
  - voice cannot move early: there is no audio stack;
  - a full execution-graph runtime, four memory types and a third-party
    SDK are premature;
  - AVX2 belongs before Bella v1, because a planner needs a bigger model
    and a bigger model needs the speed.
- **What it missed:** these become Stage 0.
  - one data-loss bug;
  - two filesystem security bugs;
  - a memory feature that learns wrong answers and can be fed commands;
  - supervision that leaves the desktop without files after a
    filesystem restart.
- **A trade-off it glossed over:** today the model never sees file names
  or contents, which is why prompt injection through files is impossible.
  Summarising notes gives that up, so it needs its own design.

## A. The review's recommendations against the code

| Recommendation | Status | Evidence | Call |
| --- | --- | --- | --- |
| Model as an untrusted service | Implemented | ElephantHandler holds one channel, no folder handle; the model sees only the typed request | Keep |
| Granular capabilities | Partial | Rights only narrow on `Open`; every app and the broker get all rights; no revocation | The broker gets read, create, remove only |
| Filesystem authorization | Partial | Handle-relative paths; `..`, absolute paths, NUL refused; freed slots reused under old handles (B2) | Fix in Stage 0 |
| Guided decoding is not security | Implemented | File names and contents never reach the model; the injection route is the memory file (B4) | Agree |
| Provenance and audit | Missing | Only `remember` is logged; broker calls have no deadline | One audit line per action, and a deadline |
| Confirm by side effect | Partial | `create` always asks; remembered `undo`, `remember`, `quit` run unasked | Agree |
| System event stream | Missing | Peacock tracks focus, open, close, minimise but only logs them | A subscriber list in Peacock |
| Four memory types | Partial | Request-to-command pairs only, no provenance | Fix A2, then provenance and a preferences file |
| Intent registry | Missing | Apps declare nothing; fixed five-app `CATALOG` | Static manifests first |
| Plan, act, check | Missing | One tool choice per request | Fixed two- and three-step workflows first |
| Three execution levels | Partial | Typed commands and the 0.5B model; no "none of these" | A confidence threshold first |
| Inference metrics | Partial | Tick counts on serial; no end-to-end time, peak memory or responsiveness | Agree |
| Voice earlier | Missing | No audio driver or service | Disagree: a large driver project |
| Backend abstraction | Partial | The seam exists: `matvec` and `quant::dot` | One trait at that seam |
| Bella v1 before SMP and GPU | n/a | SMP is large: one global scheduler lock | Agree; XSAVE and AVX2 before it |
| Gated NVIDIA track | Planned | Already the GPU stage | Agree |
| Fix FXSAVE before AVX2 | Missing | `kernel/src/fpu.rs` uses `fxsave64`; CR4.OSXSAVE never set | Small kernel change |

## B. Problems found in the code

| # | Severity | Problem | Evidence | Fix in |
| --- | --- | --- | --- | --- |
| B1 | Critical | Notes cuts every file over 8 KiB: loads at most 8,192 bytes, then saves by truncating and writing back only that | `user/src/bin/notes.rs` | Stage 0 |
| B2 | High | A removed folder's slot is reused by the next node created anywhere; an old handle then reaches it with the old rights | `badger-handler.rs` (reuse, free, sessions keep a bare node number) | Stage 0 |
| B3 | High | Memory learns wrong answers: `search` with no matches counts as success, a recall re-learns itself, a typed correction after it is never learned | `otter-handler.rs`, `assistant.rs` | Stage 0 |
| B4 | Medium | Planted memory runs commands: any app with home write access can add lines; recalled commands skip the filter, so `undo`, `quit`, `remember` run unconfirmed | `assistant.rs` | Stage 0 |
| B5 | High | After a BadgerHandler restart the desktop keeps a dead home handle; restart limits never reset | `zookeeper.rs` | Stage 0 |
| B6 | Medium | BadgerHandler trusts the client's shared-buffer length up to 64 KiB without checking the mapping | `badger-handler.rs` | Stage 0 |
| B7 | Medium | One process can use all 62 filesystem sessions | `badger-handler.rs` | Stage 0 |
| B8 | Medium | Memory matching on shared words can run a remembered command on the wrong folder | `otter-handler.rs` | Stage 0 |
| B9 | Medium | No confidence threshold or "none of these"; the eval has only positive cases | `assistant.rs`, `xtask/src/infer.rs` | Stage 1 |
| B10 | Medium | Each disk request busy-polls with interrupts off and the scheduler lock held | `kernel/src/virtio_blk.rs` | Stage 2 |
| B11 | Medium | No disk flush, so the copy-on-write commit order is not guaranteed with a write cache | `virtio_blk.rs` | Stage 7 |
| B12 | Medium | No scheduler priorities: inference competes with the desktop | `process.rs` | Stage 2 |
| B13 | Medium | Fixed 8 MiB kernel heap; running out likely panics the kernel (not tested) | `kernel/src/memory/mod.rs` | Stage 2 |
| B14 | Low | The assistant freezes during broker calls: blocking receive, no deadline, no cancel | `assistant.rs` | Stage 2 |
| B15 | Low | docs/m5c1-design.md still describes the filesystem as kernel syscalls, no links, no races | `abi/src/lib.rs` | Stage 0 |

## C. Missed by the review, and overreaches

**Missed:**
- **Correctness before features.** B1 to B8 are real and cheap to fix.
- **Summarising gives up today's strongest property.** It needs a
  separate model call with no tool choice, content marked as data, and
  the result shown before anything is written.
- **The chain to planning.** Multi-step plans need a model larger than
  0.5B; that needs AVX2. Bella v1 uses templates the small model fills.
- **Responsiveness has no mechanism.** It needs priorities and a
  non-blocking broker (B10, B12).
- **"Works after reboot" fails on real hardware.** Only QEMU's disk
  persists; USB mass storage comes before real-hardware acceptance.
- **Event channels are small.** Channels hold 8 messages of at most
  1 KiB, and sends never block; an event stream must drop or coalesce.
- **Peacock caps the desktop at five apps.**
- **The model sits in RAM permanently** in the 411 MB initrd; loading it
  from the data disk also frees that memory.

**Overreaches:**
- **An inference graph layer.** One trait at `matvec` is enough until a
  GPU needs more.
- **Four memory types now.** Provenance plus a preferences file covers
  the next year.
- **Full provenance records on every request.** One audit line per
  action gives the value.
- **A third-party SDK.** There are no third-party apps yet.
- **Voice before SMP.** Speech recognition on one SSE core would be too
  slow.
