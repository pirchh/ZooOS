# POSIX layer, step 3: exception delivery to user handlers

Status: implemented and verified 2026-10-30 (results in docs/verification.md).
Third step of strategy A (docs/feasibility/wine-hosting.md). Until now a
user fault ended the process. Windows structured exception handling, Wine's
signal handlers, debuggers, guard pages and on-demand commit all need the
program to see the fault and decide.

## 1. Model

- **Per process:** one handler entry (`set_exception_handler`). Wine
  installs one `SIGSEGV`-style handler and dispatches internally.
- **Per thread:** an optional exception stack (`set_exception_stack`,
  like `sigaltstack`). Without one, the context goes below the interrupted
  stack pointer minus a 128-byte red zone.
- On a user fault (divide, invalid opcode, stack segment, general
  protection, page fault, x87, SIMD) with a handler installed, the kernel
  writes an `ExceptionContext` (vector, error code, fault address, `rip`,
  `rsp`, `rflags`, all general registers, FS base and the 512-byte FXSAVE
  image) to that stack and starts the handler with a pointer to it, with
  interrupts enabled and other flags cleared.
- The handler ends with `exception_return(ctx, how)`:
  - `EXC_RESUME`: the kernel reloads every register from the (possibly
    edited) context. It checks that `rip`, `rsp` and the FS base are in the
    user half, that MXCSR has no bits the CPU rejects, and keeps only the
    arithmetic and direction flags from `rflags`.
  - `EXC_UNHANDLED`: the process crashes with the same crash line as
    without a handler.
- A fault while a thread is in its handler (before `exception_return`)
  always crashes the process. `exception_return` outside a handler is
  `INVAL`.

## 2. System calls (ABI version 7)

| # | Call |
| --- | --- |
| 38 | `set_exception_handler(entry)`: 0 removes it |
| 39 | `set_exception_stack(top)`: 16-byte aligned, 0 for none |
| 40 | `exception_return(ctx, how)` |

`zoo_rt::exception` wraps them: `set_handler(fn(&mut ExceptionContext) ->
bool)` (true resumes, false is unhandled), `use_own_stack(size)`.

## 3. What this enables

- **Guard pages and demand commit**, built in user space: leave pages
  reserved, commit them from the handler on first touch and resume (shown
  by the test below). This is how Windows grows thread stacks.
- Skipping or emulating faulting instructions by editing `rip` and
  registers.

Not in this step: breakpoint and single-step exceptions (vectors 1 and 3),
delivering to another thread, and suspending other threads.

## 4. Verification

WolverineTest, run by Zookeeper at boot (`normal` case):

1. `exception_return` outside a handler is refused.
2. Demand commit: 16 first touches each fault once and are resumed after
   the handler commits the page; contents round-trip; a second pass causes
   no faults.
3. Divide by zero: the handler skips `div rcx` and supplies `rax = 777`.
   It also deliberately clobbers r12, r13 and xmm0, which must come back
   unchanged.
4. A second thread's faults are handled on that thread's own stack.
5. Child processes, matched exactly by the verifier:
   - an unhandled fault crashes with the normal crash line;
   - a fault inside the handler crashes (the child uses a fixed exception
     stack, so a kernel that re-delivered would loop instead);
   - a handler returning a kernel-half `rip` is refused, and the runtime
     exits with 103.
