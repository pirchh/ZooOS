# Linux step 5 (L5): processes and pipes

Status: implemented 2026-10-31 (version 0.44.0). Follows L4
(docs/linux4-design.md). Linux programs can start other programs, wait
for them, connect them with pipes and stop them, which shells, build tools,
`system()`/`popen()` and Wine's launcher need.

## 1. Goal

`c/linux/guillemot-test.c` (guillemot, an Arctic seabird) is ordinary
Linux C: `fork` and `waitpid` with an exit code, `getppid`, separate
memory after `fork`, a pipe from a child read to its end, `dup2` +
`execvp` (a `PATH` search) with the output captured, `posix_spawn` with a
file action and of a missing program (`ENOENT`), `WNOHANG`, `kill` with
`SIGTERM`, `SIGPIPE` both by default and ignored (`EPIPE`), 256 KiB
through a 64 KiB pipe, a child writing in the home folder, and timings.

## 2. Design

- **`fork`** copies every user page into a new address space
  (`copy_address_space`). Copy-on-write sharing would make large programs
  fork faster. It is deferred until a measurement shows it matters: 4 MiB
  adds about 1–3 Mcycles (docs/f2-design.md §3).
- **`vfork` and `clone(CLONE_VM | CLONE_VFORK)`** (musl's `posix_spawn`)
  copy nothing. The child runs in the parent's address space on its own
  stack, and the parent's call blocks until the child calls `execve` or
  ends. If the parent is killed first, the child ends with it, because it
  is running in the parent's memory. This is the cheap path for starting
  programs.
- **The child** gets a copy of the descriptor table, the working
  directory, the `brk` area, the signal settings and its own home-folder
  session. The kernel sends BadgerHandler's existing `Open` request on the
  parent's channel, so the two programs' requests never share a channel.
  Its first frame is the parent's, with `rax` = 0.
- **`execve`** loads `linux/<name>` from the initrd into a new address
  space with the given arguments and environment (at most 64 of each,
  fitting the stack's top page). It closes close-on-exec descriptors,
  resets caught signals to their defaults and frees the old space; a
  `vfork` child's borrowed space is left to its parent. Nothing changes
  unless every step succeeds. Programs in the home folder cannot be run
  (`EACCES`).
- **`wait4`** reports a child's Linux wait status: code << 8, or the
  signal number. Crashes report `SIGSEGV`, kills from ZooOS `SIGKILL`.
  `WNOHANG` and `ECHILD` are supported. There is one process group, so
  pid 0 and negative pids mean any child.
- **Descriptors 0–2 are real table entries.** Standard input is at its
  end and output goes to the program's log, as before, so `dup2` can
  replace them. Also new: `dup`, `dup2`, `dup3`, `fcntl` `F_DUPFD`,
  `F_DUPFD_CLOEXEC`, `F_GETFD`/`F_SETFD` (close-on-exec), `F_GETFL`, and
  `O_CLOEXEC` on `open`.
- **Pipes** (`pipe`, `pipe2` with `O_CLOEXEC`) hold a 64 KiB ring of 16
  physical pages, outside the kernel's 1 MiB heap. Writes of up to 4 KiB
  go in at once or wait; larger ones go in as room allows. Reads wait
  while the pipe is empty and has writers, and return 0 once it has none.
  A write with no readers raises `SIGPIPE`. Each end counts the
  descriptors open on it, through copies, `dup` and exit.
- **Signals, default actions only:**
  - `kill`, `tkill` and `tgkill` work between Linux programs. Signal 0
    checks that the program exists.
  - Most signals end the program. `SIGCHLD`, `SIGCONT`, `SIGURG`,
    `SIGWINCH` and the stop signals are ignored; there is no job control
    yet.
  - `rt_sigaction` and `rt_sigprocmask` record what programs set and give
    it back. `SIG_IGN` is honoured.
  - A program's own handler is not run yet (L6). The kernel logs
    `ZOO_OS INFO linux signal handler not run`, and the signal is ignored.
- Also: `getppid` returns the parent, and `user_cstr` checks a user
  string once per page instead of once per byte.

## 3. Verification and measurements

`linux-guillemot` requires every process line, in order: the program,
then 120 children, each started, some exec'ing `hello`, and each ending
with the expected code or signal (`code=7`, `code=127`, `signal=15`,
`signal=13`, …). The output line `exec output: Hello from Linux musl:
argc=2 argv[0]=hello argv[1]=from-guillemot` is also checked.

**Measured** (WHPX, averages per child, three runs):

| | kcycles |
| --- | --- |
| fork + exit + wait | 9430–12476 |
| fork with 4 MiB touched + exit + wait | 9199–9710 (invalid, see below) |
| posix_spawn of `hello` + wait | 8201–8746 |

The 4 MiB row is invalid: the compiler removed the unread buffer, so
nothing extra was copied. F2 fixed the test and measured it properly.

These numbers are dominated by kernel log output, not by process
creation. Measured inside one run:
- `fork()` alone took 7242 kcycles and `waitpid` 2461. A system call
  without logging measured 0.
- One `ZOO_OS PROC …` line took 2168–4315 kcycles.
- Each child prints two or three such lines. Every line goes to the
  polled serial port, waiting on it for each byte, and to the text
  console, which scrolls by copying the framebuffer.
- Skipping the child's home-folder session changed nothing measurable
  (10189–10552).

Making log output cheap is the next speed step (F2), with before/after
timings on this test.

Negative controls (each restored after):

| Change | Result |
| --- | --- |
| The child does not get 0 from `fork` | `FAIL`: child runs the parent's path, `exit … code=11` |
| Closing a pipe's write end is not counted | `FAIL` (timeout): the reader never sees the end |
| `SIGPIPE` ignored by default | `FAIL`: `exit pid=34 … code=0` instead of `signal=13` |

## 4. Not in step 5

- Running a program's own signal handlers, and blocked signals waiting
  until they are unblocked (L6).
- Threads (`clone` with `CLONE_THREAD`), sessions, process groups and
  job control.
- Shared file positions between processes after `fork` or `dup`: each
  descriptor keeps its own position. Pipes are shared, as they should be.
- `O_NONBLOCK`, `poll`/`select` on pipes.
- Copy-on-write `fork`.
- Running programs from the home folder.
