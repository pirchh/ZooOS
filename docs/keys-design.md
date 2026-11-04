# Keys under load (2026-11-03)

Status: implemented. With 8 processors, keys typed into Bella while the
desktop starts (the chat and embedding models load) were lost: Bella
received "find m" + the end of the sentence. `bella-live` had shown the same
once ("Find my ake recipe", docs/verification.md, version 0.98.0).

## 1. The cause, measured

The guess recorded with 0.98.0 was the model-disk read (B10). Measured with
temporary timing lines (time-stamp counter around the scheduler lock, and
the gap between the boot processor's timer ticks), it was not:

| Measured | Before |
| --- | --- |
| Longest hold of the scheduler lock | 7,845 Mcycles, by `mem_alloc` (call 17) of the model service; 3,854 Mcycles by the embedding service's |
| Every other processor meanwhile | spinning for the lock with interrupts off, the boot processor too: 7,831–7,851 Mcycles each |
| Boot processor tick gap | 9,110 Mcycles with no timer tick |
| Keyboard interrupts (IRQ1) | 65 for 110+ key bytes; 16 in a burst after the gap, then the rest lost |
| `animation open Bella` | 4 frames in 4104 ms |

**`mem_alloc` maps and zeroes every page of a request with the scheduler
lock held**: the model service's 1.2 GB heap, at once. A model-disk read
is 16 blocks (64 KiB) per call and never held it that long.

The keyboard interrupt goes to the boot processor only. While it waits for
the lock, QEMU's PS/2 controller queues 16 bytes (8 keys, down and up) and
drops the rest. The kernel's own 256-byte ring never filled.

## 2. The fix: large allocations in passes

`mem_alloc` maps at most `ALLOC_STEP` pages (256, 1 MiB; measured at
about 20 Mcycles per pass with 8 processors, docs/usb-model-design.md)
per pass. A request
larger than that:

1. takes its whole address range at once (`heap_next` moves first), so
   another thread's call can never overlap it;
2. maps one step and records `(size, start, done)` on the thread
   (`Task::alloc_progress`);
3. moves `rip` back to the `int 0x80` (RAX still holds the call number) and
   returns `Outcome::Resume`.

The thread runs the call again; the lock is free in between, so pending
interrupts are taken and other processors get their turn. A pass that
continues matches the recorded size; any other call starts afresh. Out of
memory part-way frees the pages mapped so far and gives the range back if
it is still the last one.

The program sees one call that returns once, as before. 8 MiB steps (2048
pages, about 13 ms each) were tried first: 4 of 5 runs passed, one lost a
key, because two services allocating at once took turns with the lock and
the boot processor could miss several.

## 3. Held keys in the window server

Once interrupts arrived, the next limit showed: Bella's 8-message queue is
full while Bella is busy at start, and PeacockHandler
dropped a key it could not send. Now each app has up to 256 held keys
(`HELD_KEYS`), delivered in order before any new one; the main loop polls
every tick while keys are held instead of waiting for input. A burst logs
`PeacockHandler: Bella was busy; held up to N keys, all delivered`. With
8 MiB steps it held up to 25 keys and delivered them all; with 1 MiB steps
no run needed it, and it stays as a guard for a busy app.

## 4. Verification

`smp-keys` (8 processors, model disk): at desktop start, tab to Bella and
type the 53-character "find my pancake recipe please and thank you very
much" while the models load; pass only if ElephantHandler logs the whole
request.

| Build | `smp-keys` runs | Bella received |
| --- | --- | --- |
| Before (negative control) | 5 `FAIL` of 5 | e.g. "find m and thank you very much", "find mhank you very much" |
| Ticket lock (not kept) | 5 `FAIL` of 5 | the same kind of gap after "find m" |
| Held keys alone | 5 `FAIL` of 5 | "find mvery much", "find mplease and thank you very much", … (the hold log line never appeared) |
| 8 MiB passes + held keys | 4 `OK`, 1 `FAIL` | once "…pleae and…" |
| **1 MiB passes + held keys** | **10 `OK` of 10** | the whole sentence each time |

With 8 MiB passes the longest lock hold left was 139 Mcycles, most about
50; 1 MiB passes cut a pass to an eighth of that. `animation open
Bella` took 222–275 ms (7–11 frames) instead of 4104 ms (4 frames). The
model read took 1381–2386 ms for 1077 MB, as before (1763–2053 ms).

## Not done

- The scheduler lock is still one lock and a spin lock without fairness
  (docs/smp-design.md); a ticket lock alone did not fix this (5 of 5 runs
  still lost keys) and was not kept.
- A process exiting frees its pages under the lock: measured in 0.101.0
  (docs/usb-model-design.md), killing the 1.2 GB model service held it
  no more than 20 Mcycles. Freeing is fast; zeroing was the slow part.
- Keys still go through the boot processor's IRQ1 only.
