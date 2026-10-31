# Fast files step 1 (F1): shared-memory bulk transfers

Status: implemented 2026-10-31 (version 0.42.0, ABI version 17); §5 has
the results. Owner priority: speed first. Measurement came first.

## 1. Problem (measured)

Every file request to BadgerHandler is one channel message of at most 256
bytes, so a reply carries at most 248 file bytes, and each round trip costs
two context switches and two copies. `bowhead-test` (a private
BadgerHandler; 1 MiB written with `file_write`, read with `file_read`)
under QEMU/WHPX on the development PC, four runs:

| Run | Write 1 MiB | Read 1 MiB |
| --- | --- | --- |
| 1 | 127 Mcycles (40 ms) | 107 Mcycles (40 ms) |
| 2 | 125 Mcycles (40 ms) | 72 Mcycles (20 ms) |
| 3 | 123 Mcycles (40 ms) | 40 Mcycles (10 ms) |
| 4 | 133 Mcycles (40 ms) | 103 Mcycles (40 ms) |

About 25 MB/s, for data that is already in memory. Milliseconds are at the
10 ms tick; cycles are `rdtsc`.

## 2. Change

- **Kernel: shared memory.** New calls (ABI version 17): `shm_create(pages)`
  (zeroed pages, at most 256, as a handle), `shm_map(handle)` (maps all of
  them read/write into the caller, returns the address), `shm_unmap(addr)`.
  Handles move between processes like any other (`send_handle`). The pages
  are reference-counted: each handle and each mapping holds a reference,
  and the last one queues them to be freed at the next process reap (where
  the memory lock is free). Mappings are unmapped, never freed with the
  address space.
- **Protocol:** a client may send a shared buffer on a folder channel
  (handle message, tag `TAG_SHM`); BadgerHandler maps it for that session
  and unmaps it when the session ends. Two new requests, `ReadBulk` and
  `WriteBulk`: offset in `arg`, length (u32) in the data, the bytes in the
  buffer; one round trip moves up to 64 KiB. Requests and replies
  alternate, so only one side touches the buffer at a time. Without a
  buffer the session answers `AGAIN`.
- **`zoo_rt`:** `file_read` and `file_write` use the bulk path for more than
  2 KiB, attaching a 64 KiB buffer to the folder handle on first use; small
  transfers keep the message path. Files, Notes and every native program
  get it without changes.

Target: the same benchmark at least 10 times faster; it is reported
whatever it is.

## 3. Verification

- `file-speed` case (and three WHPX runs for the numbers, before and
  after); the existing file, Notes, persistence and Assistant cases.
- A shared-memory leak check: the boot checks' frame count before and
  after must still match (the kernel's `process-cleanup` test).
- Negative controls: the bulk read copying one byte short (the benchmark's
  comparison must fail); shared pages never freed (the leak check must
  fail).

## 4. Not in F1

A kernel file cache (repeat reads without any round trip), the Linux file
path (L4 moves onto this next), zero-copy mapping of file contents.

## 5. Results

The same benchmark, after (WHPX, `bowhead-test` now also times a second
"warm" write over the same megabyte):

| | Before | After |
| --- | --- | --- |
| Read 1 MiB | 40–107 Mcycles | 0.37–0.59 Mcycles (about 100–250 times less) |
| Write 1 MiB, file growing | 123–133 Mcycles | 6.9–7.8 Mcycles (about 17 times less) |
| Write 1 MiB again (warm) | — | 0.31–0.50 Mcycles |

A first-time write costs more because BadgerHandler grows the file (and
its heap) as it goes; that is the next thing to look at.

**A scheduler stall found by this benchmark.** Before the last change,
some runs took about 30 Mcycles (one 10 ms tick) longer, in reads and
warm writes alike. On a timer tick the scheduler picked the next task in
plain round-robin, including the kernel's own task even when it was
idle; an idle kernel task halts until the next tick, so a ready program
lost up to 10 ms. Now the timer passes over the idle kernel task when a
program is ready (as voluntary switches already did); a busy kernel task
still gets its turn. Six runs after the change (twelve timed transfers)
showed no stall. This affects every program, not only file transfers.

**Checks:**
- `ptarmigan-test` (a boot check) maps shared memory twice through a
  passed handle, checks both mappings see the same pages, that they
  outlive the closed handle, unmapping, and the size limits; the kernel's
  boot leak check then confirms the pages were freed.
- Negative controls: BadgerHandler's bulk read one byte short → the boot
  check `arcticfox-test` (which reads files over 2 KiB) already fails
  (exit 41); shared pages never freed → `PANIC self-test process-cleanup:
  520754 frames free, expected 520790`.
