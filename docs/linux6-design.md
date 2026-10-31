# Linux step 6 (L6): signal handlers, threads and lazy memory

Status: implemented 2026-10-31 (version 0.46.0). Follows L5
(docs/linux5-design.md) and F2 (docs/f2-design.md). Wine depends on all
three: it handles faults in signal handlers by editing the saved context,
runs many threads, and reserves large address ranges it touches sparsely.

## 1. Goal

`c/linux/kittiwake-test.c` (the kittiwake is an Arctic gull) is ordinary
Linux C. It checks:
- **Signal handlers:** `SA_SIGINFO` handlers with `si_code` and `si_pid`
  (`raise` and `kill`); a blocked signal waiting in `sigpending` until it
  is unblocked; `SIGSEGV` caught twice and left with `siglongjmp`; a
  `SIGILL` handler that moves `REG_RIP` past `ud2` (Wine's method); a
  handler on a `sigaltstack`.
- **Interrupted calls and process signals:** `EINTR`, then `SA_RESTART`,
  on a blocked `read`; `pause`; a threaded child ended by `SIGTERM`; `ud2`
  without a handler ending the program with `SIGILL`.
- **Threads:** 4 pthreads × 20,000 increments under a mutex, with distinct
  tids; a condition variable; a raw `clone` thread checking
  `PARENT_SETTID` and `CHILD_CLEARTID` with a futex wait, which glibc's
  `pthread_join` uses.
- **Lazy memory:** a 64 MiB mapping, with `read`, `clock_gettime` and
  `uname` writing into pages the program never touched.
- **Timings.**

## 2. Design

### Signals

- **State:**
  - Handlers (`rt_sigaction`) belong to the process.
  - Each thread has its own mask (`rt_sigprocmask`), its own pending set
    (for `tkill`/`tgkill`) and its own alternate stack (`sigaltstack`).
  - The process has a pending set for `kill`, and remembers the sender of
    each signal for `si_pid`.
  - `SIGKILL` and `SIGSTOP` cannot be blocked.
- **Sending:**
  - Ignored signals are dropped.
  - A default action that ends the program acts at once, unless every
    thread that could take the signal blocks it.
  - Otherwise the signal is marked pending, and blocked threads that do
    not block it are woken.
- **Delivery** happens whenever a Linux thread is about to return to user
  mode: after a system call (including `rt_sigprocmask` unblocking one,
  and `rt_sigreturn`), when a thread is resumed after a switch, and after
  an interrupted call. The lowest pending unblocked signal either runs
  its default action or gets a handler frame.
- **The handler frame** follows Linux's x86-64 layout, so `ucontext_t`,
  `REG_*` and `siginfo_t` mean what programs expect:
  - the return address (`sa_restorer`);
  - a `ucontext` with the alternate-stack state, all general registers,
    flags, CS/SS, the error code, trap number, old mask and CR2, a pointer
    to the x87/SSE state, and the mask to restore;
  - a `siginfo`;
  - the 512-byte FXSAVE image (64-byte aligned).

  The frame goes below the 128-byte red zone, or on the alternate stack
  for `SA_ONSTACK`. The handler starts with `rdi`, `rsi` and `rdx` set to
  the signal, the `siginfo` and the `ucontext`. The signal (unless
  `SA_NODEFER`) and `sa_mask` are added to the mask; `SA_RESETHAND` is
  honoured.
- **`rt_sigreturn`** restores the registers from that frame, including any
  the handler edited, along with the x87/SSE state and the mask.
  - Only arithmetic and direction flags come from the program.
  - The instruction and stack pointers must be user addresses.
  - A bad frame ends the program with `SIGSEGV`.
- **Interrupted calls:** a blocked call of a thread with a deliverable
  signal ends with `EINTR`. With `SA_RESTART`, the instruction pointer
  steps back to the `syscall` instruction instead, so the call runs again
  after the handler. `pause`, `nanosleep` and `clock_nanosleep` never
  restart, as on Linux. Calls waiting for BadgerHandler or a `vfork`
  child are not interrupted, like Linux's uninterruptible waits.
- **Faults:**
  - CPU faults in Linux threads become signals with Linux's codes: page
    fault → `SIGSEGV` (`SEGV_MAPERR`/`SEGV_ACCERR`, `si_addr` = CR2);
    #UD → `SIGILL`; #DE/x87/SIMD → `SIGFPE`; #DB/#BP → `SIGTRAP`;
    #AC → `SIGBUS`; others → `SIGSEGV`.
  - With a handler that the thread does not block, the handler runs.
  - Otherwise the program ends with that signal: the crash line is still
    printed, and `wait4` reports the signal.

### Threads

- `clone` with musl's `pthread_create` flags creates a thread in the same
  process: the parent's frame with `rax` = 0, the given stack, and the
  TLS from `CLONE_SETTLS`. `PARENT_SETTID`, `CHILD_SETTID` and
  `CHILD_CLEARTID` are supported.
- `exit` ends one thread, `exit_group` the process. A thread's exit clears
  its clear-tid word and wakes one futex waiter there.
- **`futex`** (private): `WAIT` with a relative timeout, `WAIT_BITSET`
  with an absolute one (all bits), `WAKE`, `WAKE_BITSET`, `REQUEUE` and
  `CMP_REQUEUE`. It reuses the native futex wait fields.
- `set_robust_list` is accepted, `sched_yield` yields, `gettid` returns
  the thread's id, and `tgkill` targets a thread.
- Threads share the process's BadgerHandler channel, so the kernel lets
  one request at a time be outstanding, including the gap between an
  `Open` reply and its handle during `fork`.
- Linux thread starts and exits are not logged; each log line costs about
  0.65 Mcycles (docs/f2-design.md).

### Lazy anonymous memory

The first thread timing was 2.6 Mcycles per `pthread_create` + `join`.
The cause: this toolchain gives threads 8 MiB stacks, and `mmap` +
`mprotect` committed and zeroed all 2,049 pages, which `munmap` then
freed again. Linux maps such memory lazily, and Wine relies on that for
large reservations.

- `mmap` with access, and `mprotect` inside a reservation, now record a
  lazy range: start, length, access.
- A not-present fault there maps a zeroed page with that access and
  retries the instruction.
- When the kernel itself meets an inaccessible user range, for example
  `read` or `clock_gettime` into a fresh buffer, the call returns its
  error, the range's lazy pages are committed, and the call runs again
  (at most 8 times). BadgerHandler replies are replayed, not requested
  again. A few helpers commit directly: pipe reads, signal frames and
  thread ids.
- `munmap` removes lazy ranges. `fork` copies them. `exec` clears them.
- `mprotect(PROT_NONE)` makes untouched pages fault, which is how
  musl's guard pages work. Committed pages stay readable, as before.

## 3. Verification and measurements

`linux-kittiwake` requires every process line in order: four children,
including `signal=15` and `signal=4`, and the crash line of the `ud2`
child. It also checks `KittiwakeTest: all checks passed` and exit 0.

**Measured** (WHPX, averages, three runs each):

| | Cycles |
| --- | --- |
| `raise` + handler + `rt_sigreturn` (`raise` blocks and unblocks all signals around `tkill`) | 1725–1949 |
| `pthread_create` + `join`, lazy memory | 20–21 thousand |
| `pthread_create` + `join`, before lazy memory | 2570–2767 thousand |
| fork + wait (L5 test, unchanged) | 1762–2142 thousand |

Negative controls (each restored after):

| Change | Result |
| --- | --- |
| `rt_sigreturn` does not restore the mask | `FAIL`: exit 12 (`kill`: `SIGUSR1` stays blocked after the first handler) |
| `SA_RESTART` ignored | `FAIL`: exit 62, `SA_RESTART (errno 4)` |
| A thread's exit does not clear its clear-tid word | `FAIL` (timeout): the futex waiter never sees 0 |
| No commit-and-retry for lazy memory | `FAIL`: exit 124, `clock_gettime into an untouched page (errno 14)` |

A control that removed only the clear-tid futex wake still passed. The
kernel wakes every blocked task when a thread ends (`wake_all`), so the
waiter rechecks the word, finds it cleared, and continues. Real Linux
would leave it asleep. The clearing is what the test can observe, and
that control is the third one above.

## 4. Not in step 6

- `rt_sigsuspend`, `sigwaitinfo`, `signalfd`, real-time signal queueing
  (one pending instance per signal), `SA_SIGINFO` values from
  `sigqueue`, timers (`setitimer`, `timer_create`, `alarm`).
- The remaining time in `nanosleep`'s `rem` after `EINTR`.
- XSAVE state beyond FXSAVE (AVX registers are not saved in frames).
- Robust futex cleanup, `FUTEX_WAKE_OP`, PI futexes, bitset matching,
  shared (cross-process) futexes.
- `mprotect(PROT_NONE)` on committed pages.
- Copy-on-write `fork`.
