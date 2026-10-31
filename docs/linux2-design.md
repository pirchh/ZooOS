# Linux step 2 (L2): memory, read-only files and time

Status: implemented 2026-10-31 (version 0.40.0). Follows L1
(docs/linux1-design.md) on the owner's route 3: Linux programs that need
more than `printf`.

## 1. Goal

`c/linux/eider-test.c` (an Arctic name, like the boot tests) is ordinary
Linux C: `malloc`/`free` of many small blocks and one 4 MiB block, `mmap`
and `munmap` directly, `/etc/services` read with `fopen`/`fgets` and with
`open`/`fstat`/`read`/`lseek`/`close`, `ENOENT` for a missing file,
`clock_gettime` and a 200 ms `nanosleep`. Built with the standard
`x86_64-linux-musl` target; it runs on ZooOS unmodified.

## 2. Design

- **Memory** on ZooOS's own reserve/commit calls (docs/posix2-design.md):
  - `mmap`: anonymous private memory at an address the kernel chooses
    (a reservation, committed read/write unless `PROT_NONE`); fixed
    addresses and file mappings are `EINVAL` for now.
  - `munmap`: a whole reservation is released, part of one decommitted.
  - `brk`: one 64 MiB reservation made on first use; the break moves
    within it by committing or decommitting pages; an impossible request
    returns the unchanged break, as Linux does.
  - `mprotect` (readable protections), `madvise` (accepted, ignored).
- **Files: read-only, from the initrd.** `open`/`openat` (read-only flags,
  else `EROFS`) map `/etc/services` to the initrd's `etc/services`;
  relative paths start at `/`, and `.`/`..` are refused. Each process has
  up to 64 descriptors from 3 up (initrd data and position). `read`,
  `readv` (musl's stdio uses it), `lseek`, `close`, and `fstat`, `stat`,
  `newfstatat` (a read-only regular file; the standard descriptors look
  like character devices). Standard input reads as empty.
- **Time:** `clock_gettime` (realtime and the monotonic clocks, from
  docs/posix5b1-design.md), `nanosleep` and relative `clock_nanosleep`,
  rounded up to timer ticks, blocking like ZooOS's own sleep.
- `sched_getaffinity` reports one CPU (musl asks).
- **A fix for L1:** a Linux call that blocks is re-run on wake-up; the
  re-run now goes back to the Linux handler (it used ZooOS's).

## 3. Verification

`linux-eider` case: `linux eider-test` prints `memory ok`,
`/etc/services 180 bytes, 5 lines`, `slept 200 ms`, `unix=…` (within 120 s
of the host clock), `all checks passed`, and exits 0, with no unsupported
calls. `linux-hello` passes unchanged.

Negative controls:

| Change | Result |
| --- | --- |
| `mmap` always fails | `FAILED small malloc (errno 12)`, exit 10: musl's allocator takes even small blocks from `mmap` |
| `fstat` reports one byte too many | `FAILED read size`, exit 24 |
| `nanosleep` returns at once | `FAILED slept too little`, exit 32 |

## 4. Not in step 2

Writing files and the home folder (BadgerHandler), directories
(`getdents64`), file-backed and fixed `mmap`, `fork`/`execve`, signals,
threads (`clone`), pipes, an environment, absolute-time sleeps.
