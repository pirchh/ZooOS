# POSIX layer, step 1: threads, futexes and thread-local storage

Status: implemented and verified 2026-10-30 (results in docs/verification.md).
First step of strategy A in docs/feasibility/wine-hosting.md (grow ZooOS
until Wine's Unix side can be ported), chosen by the owner. Wine maps every
Windows thread to a host thread, so threads come first; this step needs no
new host tools (a C toolchain and libc port come later and need a download).

## 1. Scope

- Several threads per process, preemptively scheduled like processes.
- Futex wait/wake on a 32-bit word, with an optional timeout.
- A per-thread FS base for thread-local storage.
- Not in this step: thread priorities, signals, suspending another thread,
  per-thread CPU affinity (one CPU), `clone` flags, user-chosen stacks for
  the first thread.

## 2. Kernel model

| | Per process (`Proc`) | Per thread (`Task`) |
| --- | --- | --- |
| State | address space, handle table, heap and mapping cursors, file mappings, display pages, `fs_root` permission | saved registers and FPU state, kernel stack, run state, sleep deadline, FS base, futex wait |

- A process ends when `exit` is called by any thread, when it crashes (a
  fault in any thread) or is killed, or when its last thread calls
  `thread_exit`. Ending a process stops all its threads; its memory is freed
  once every thread is reaped (off their own kernel stacks).
- The first thread's id is the process id; further threads get ids from a
  separate range starting at 65536, so process ids (and the verifier's
  expectations) do not depend on how many threads earlier programs made.
  Kernel stack slots were raised from 16 to 32.
- The kernel writes the FS base MSR whenever it resumes a user thread; the
  kernel itself does not use FS.

## 3. System calls (ABI version 5)

| # | Call | Notes |
| --- | --- | --- |
| 27 | `thread_create(entry, stack_top, arg)` | user-half addresses, 16-byte aligned stack; the thread starts at `entry` with `arg` in `rdi`; returns a thread handle (WAIT right) |
| 28 | `thread_exit(code)` | ends the calling thread; the last thread's exit ends the process with `code` |
| 29 | `futex_wait(addr, expected, timeout_ticks)` | blocks while the aligned `u32` at `addr` equals `expected`; returns 0 when woken or when the value differs (spurious returns allowed); `TIMEDOUT` (new error 18) after `timeout_ticks` if non-zero |
| 30 | `futex_wake(addr, n)` | wakes up to `n` waiters of the same process on `addr`; returns how many |
| 31 | `set_fs_base(base)` | sets the calling thread's FS base |

`wait` on a thread handle returns that thread's exit code. `kill` on a
process handle ends all its threads.

## 4. Runtime (`zoo_rt::thread`)

`spawn(closure)` (64 KiB heap stack, the closure's `u8` result is the exit
code), `exit`, `futex_wait`/`futex_wake`, `set_fs_base`, `fs_read_u64`, and
a futex `Mutex<T>` (0 unlocked, 1 locked, 2 contended). Thread stacks are
not freed before the process ends (documented limitation).

## 5. Verification

AntHandler (started by Zookeeper during the start-up checks, `normal` case):

1. Four threads × 20,000 locked increments of one counter; each thread
   sleeps while holding the lock every 5,000 rounds, so the others must wait
   on the futex. The counter must be exact and the lock must have been
   contended at least once (otherwise the check proves too little).
2. Each thread (and the main thread) installs its own TLS block and checks
   FS:[8] holds its own id every 500 rounds and at the end.
3. Each worker's exit code (10 + id) must come back through `wait`.
4. `futex_wait` on a value that never changes must time out.
5. A thread blocked in `futex_wait` on a value that never changes must be
   released by `futex_wake`, which must report exactly one waiter.
6. The main thread exits while another thread is still running: the process
   must end and `TEST process-cleanup ok` (no leaked frames) must hold.

Negative controls (recorded in docs/verification.md): no FS-base switching,
and a `futex_wake` that wakes nobody.
