# M3 design: isolated programs, IPC and the Zookeeper supervisor

Status: design written before implementation, as MILESTONES.md and
DECISIONS.md ("Before M3, document syscall numbering/versioning, handle
lifetime, IPC errors, process exit semantics and executable loading")
require; then implemented and verified (2026-10-30). Results are in
docs/verification.md. §12 lists what changed during implementation.

M3 acceptance (MILESTONES.md): two isolated processes communicate; invalid
handles and malformed requests fail safely; a crashing process does not
terminate unrelated processes; begin an initial filesystem and a supervisor
with explicit failure behaviour.

Still one CPU. *Deferred* items are listed in §11.

## 1. Address-space layout (changes M2)

M2 put every bootloader mapping in the lower half. User programs need the
lower half, so the split becomes three-way:

| Level-4 entries | Virtual range | Owner |
| --- | --- | --- |
| 0–255 | `0x0000_0000_0000_0000` – `0x0000_7FFF_FFFF_FFFF` | **user**, per process |
| 256–383 | `0xFFFF_8000_0000_0000` – `0xFFFF_BFFF_FFFF_FFFF` | bootloader mappings (kernel image, boot stack, physical window, framebuffer, ramdisk, boot info) |
| 384–511 | `0xFFFF_C000_0000_0000` – end | kernel-owned (heap, scratch, kernel stacks for processes) |

The bootloader config becomes `dynamic_range_start = 0xFFFF_8000_0000_0000`,
`dynamic_range_end = 0xFFFF_BFFF_FFFF_F000`. The kernel checks at boot that
entries 384–511 are empty (as M2 checked 256–511) and that the bootloader's
upper-half entries do **not** have the user-accessible bit.

Kernel-owned additions (all inside level-4 entry 384, which exists from boot
because the heap lives there):

| Range | Use |
| --- | --- |
| `0xFFFF_C000_8000_0000` + slot × 64 KiB | per-process kernel stacks: 1 guard page (unmapped) + 16 KiB stack, 16 slots |

Each process gets its own level-4 table: entries 256–511 are copied from the
kernel's table (so the kernel is mapped, supervisor-only, in every address
space) and entries 0–255 hold only that process's pages. Because new kernel
mappings are only ever made inside entries that already exist at boot, every
address space sees them. The leftover identity mappings the bootloader made in
the lower half stay in the kernel's own table but are never copied to processes.

User layout (every process):

| Range | Use |
| --- | --- |
| below `0x20_0000` | unmapped (null-pointer guard) |
| `0x20_0000` – `0x4000_0000` | program image (ELF `PT_LOAD` segments) |
| `0x7FFF_FFEF_F000` | stack guard page (unmapped) |
| `0x7FFF_FFF0_0000` – `0x7FFF_FFF1_0000` | user stack, 64 KiB |

## 2. Entering and leaving the kernel

- GDT gains user data and user code segments (privilege level 3).
- The TSS's ring-0 stack pointer (`RSP0`) is set to the running process's
  kernel-stack top on every switch, so traps from user mode land on that
  process's own kernel stack.
- **One assembly entry path** for every trap that may switch processes:
  vectors 0 (divide), 6 (invalid opcode), 12 (stack fault), 13 (general
  protection), 14 (page fault), 32 (timer) and `0x80` (system call). Each
  vector has a tiny stub that pushes a dummy error code if needed and the
  vector number, then jumps to a common stub that saves all 15 general
  registers, calls `trap_dispatch(&mut TrapFrame) -> *mut TrapFrame`, switches
  `rsp` to the returned frame, restores registers and `iretq`s. Returning a
  different frame is the context switch. About 40 lines of assembly, written
  as naked functions in one file (`kernel/src/trap.rs`) with documented
  safety conditions. Other vectors keep the M2 `x86-interrupt` handlers.
- Vector `0x80` is the only gate user mode may invoke (gate privilege 3).
- No floating-point/SSE state is saved: kernel and user programs are built for
  `x86_64-unknown-none`, which uses soft-float and never touches SSE registers.

**Faults:** a fault from user mode ends that process (§6). A fault from
kernel mode is fatal and reported exactly as in M2 (same markers).

## 3. Tasks and scheduling

- Task 0 is the kernel itself (boot stack, kernel page table): it runs the
  boot sequence and then the debug shell. Tasks 1… are user processes.
- Round-robin, preemptive: the timer (100 Hz) switches between ready tasks.
- **The kernel task is only preempted at its idle point** (just before
  `sti; hlt`, marked by a flag). Kernel code otherwise runs with interrupts
  disabled during traps, or holds no locks a process could need, so a process
  can never spin on a lock the preempted kernel task holds.
- Blocking system calls (receive, wait) mark the task blocked and switch away.
  When something it might be waiting for happens (a message is sent, a
  process exits, a handle closes), all blocked tasks are marked ready and
  their system call is **re-run** when they are next scheduled; if it still
  cannot complete, it blocks again. Simple and correct for M3's handful of
  tasks; targeted wake-ups are deferred.
- Exited tasks are reaped (kernel stack, page tables and frames freed) on a
  later trap, never while running on the dying task's own kernel stack.

## 4. System call ABI (version 1)

`int 0x80`. Number in `rax`; arguments in `rdi`, `rsi`, `rdx`, `r10`, `r8`,
`r9`; result in `rax`. All other registers are preserved. **Results ≥ 0 are
success; negative values are `-error`.** Syscall 0 returns the ABI version,
so programs can refuse to run on an incompatible kernel. Numbers are never
reused; a changed meaning gets a new number and a version bump.

| # | Call | Arguments | Result |
| --- | --- | --- | --- |
| 0 | `abi_version` | – | `1` |
| 1 | `exit` | code (0–255) | does not return |
| 2 | `yield` | – | 0 |
| 3 | `log` | ptr, len (≤ 256) | bytes written |
| 4 | `channel_create` | ptr to two `u64` | 0; writes both endpoint handles |
| 5 | `spawn` | path ptr, path len, handle to give (or `u64::MAX`) | process handle |
| 6 | `send` | handle, ptr, len (≤ 256) | 0 |
| 7 | `recv` | handle, ptr, capacity | message length (blocks until a message) |
| 8 | `wait` | process handle | exit code (blocks until the process ends) |
| 9 | `close` | handle | 0 |
| 10 | `kill` | process handle | 0; the process ends as killed (added in implementation, §12) |

Errors (returned negated):

| # | Name | Meaning |
| --- | --- | --- |
| 1 | `NOSYS` | unknown system call number |
| 2 | `BADHANDLE` | handle is not open in this process (or is stale) |
| 3 | `FAULT` | a pointer range is not readable/writable user memory |
| 4 | `INVAL` | a length, code or argument is out of range |
| 5 | `FULL` | the receiver's queue is full (send never blocks) |
| 6 | `PEERCLOSED` | the other end of the channel is closed |
| 7 | `NOTFOUND` | no such program in the initial filesystem |
| 8 | `NOMEM` | out of memory, handle slots or process slots |
| 9 | `CRASHED` | (`wait`) the process was killed by a fault |
| 10 | `RIGHTS` | the handle lacks the right for this operation |
| 11 | `TOOSMALL` | (`recv`) the buffer is smaller than the next message, which stays queued |
| 12 | `KILLED` | (`wait`) the process was ended by `kill` |

**Pointer validation:** every user range is checked before use: inside the
user half, no arithmetic overflow, and every page mapped user-accessible in
the calling process (and writable, for buffers the kernel writes). Failing
ranges return `FAULT`; the kernel never faults on a user pointer.

**Log output** is printed as `[pid:name] text` on serial and screen. User text
cannot produce verifier markers: `ZOO_OS` in user text is rewritten to
`ZOO-OS`, and control characters other than newline become `?`.

## 5. Handles and IPC

- Each process has 16 handle slots. A handle value encodes slot and a
  generation counter (`generation << 8 | slot + 1`); closing a slot bumps its
  generation, so a stale handle value returns `BADHANDLE` instead of reaching
  whatever reused the slot. 0 is never a valid handle.
- Kinds and rights: **channel endpoint** (`SEND`, `RECV`), **process**
  (`WAIT`, `KILL`). A handle given to `spawn` is moved: it disappears from the parent
  and becomes the child's start-up argument (in `rdi` at `_start`).
- **Channels** are bidirectional pairs. Each direction queues up to 8
  messages of at most 256 bytes, copied in and out (no shared memory).
  `send` to a full queue returns `FULL`; to a closed peer, `PEERCLOSED`.
  `recv` returns the next message, blocks while the queue is empty, and
  returns `PEERCLOSED` once the queue is empty and the peer is gone.
- Handles are closed by `close`, and all of a process's handles are closed
  when it exits or crashes, which is how peers learn it is gone.

## 6. Process lifecycle

- `spawn` looks the program up by name in the initial filesystem, loads it,
  creates its address space, kernel stack and handle table, moves the given
  handle in, and makes it ready. Kernel prints
  `ZOO_OS PROC start pid=<n> name=<name>`.
- **Exit:** `exit(code)` ends the process: `ZOO_OS PROC exit pid=… name=… code=…`.
- **Crash:** a fault in user mode ends only that process:
  `ZOO_OS PROC crash pid=… name=… fault=<kind> …`, with the same detail
  format as kernel page-fault reports (`access=… cause=… addr=…`).
- Either way: handles closed, waiters woken (`wait` returns the code, or
  `CRASHED`), memory reclaimed after the switch away. Other processes are not
  affected.
- PIDs are never reused within a boot. At most 16 processes at once.

## 7. Executable loading

Programs are static, non-relocatable ELF64 x86-64 executables linked at
`0x20_0000` (built with `-C relocation-model=static`; checked in M3 prep:
rust-lld produces `ET_EXEC` with separate read-only, code and data segments).
The loader rejects, with `INVAL`/`NOTFOUND` and a kernel log line:

- wrong magic, class, endianness, version, machine or type;
- program headers outside the file, wrong entry size, more than 16 headers;
- a `PT_LOAD` segment with file size > memory size, file range outside the
  file, misaligned offset/address, any part outside the image window, both
  writable and executable, or overlapping another segment's pages;
- an entry point outside every executable segment.

Pages are mapped user-accessible, writable only for `PF_W` segments and
no-execute unless `PF_X`. The parser is pure code in a shared crate so it is
unit-tested on the host, including malformed inputs.

## 8. Initial filesystem

A read-only **ustar archive** passed as the bootloader's ramdisk, built by
`xtask` from the user programs (`bin/zookeeper`, `bin/beluga-test`,
`bin/lemming-test`). The kernel validates every header (magic, checksum,
octal size, bounds) before trusting it, and looks programs up by name. The
shell gets `ls` to list it. A writable filesystem and a filesystem service
are deferred.

## 9. The Zookeeper scenario (M3 acceptance demo)

Three user programs, in a new `user/` crate with a small runtime (`zoo-rt`:
`_start`, system call wrappers, panic → `exit(101)`):

- **Zookeeper** (supervisor, pid 1, started by the kernel after self-tests):
  1. checks `abi_version() == 1`;
  2. creates a channel and spawns **BelugaTest** with one end;
  3. sends `ping 1`, expects `pong: ping 1`;
  4. spawns **LemmingTest**, waits for it, expects `CRASHED`; restart policy
     is **one restart**, so it spawns it again, expects `CRASHED` again, then
     logs that it is giving up;
  5. spawns **MuskoxTest**, which spins forever without yielding, then
     sends `ping 2` to Lion and expects `pong: ping 2`. This proves Lion
     survived Monkey's crashes, and is only possible if the kernel preempts
     the hamster. Then it `kill`s the hamster and expects `wait` to report
     `KILLED`;
  6. sends `stop`, waits for Lion, expects exit code 0;
  7. exits with code 0. Any failed expectation exits with a distinct non-zero
     code instead.
- **BelugaTest** answers each message with `pong: <message>` and exits 0 on
  `stop` (2 if its peer disappears).
- **LemmingTest** makes malformed requests and checks each is rejected:
  unknown syscall → `NOSYS`; bogus handle → `BADHANDLE`; kernel-address
  pointer → `FAULT`; huge length → `INVAL`; `recv`/`wait` on bad handles →
  `BADHANDLE`; a log line containing a forged `ZOO_OS PANIC` marker (must be
  rewritten). A wrong result exits with a code (not a crash). Then it reads
  kernel heap memory at `0xFFFF_C000_0000_1000`, which must fault.

The kernel waits (bounded: 30 s of ticks, else `ZOO_OS PROC boot-timeout`)
until every process has exited and been reaped, checks the free frame count
is back to where it was before Zookeeper started, prints
`ZOO_OS TEST process-cleanup ok`, then `READY` and the shell as before.

Expected markers after the self-tests (kernel-generated, so their order is
fixed by Zookeeper's sequential plan):

    ZOO_OS PROC start pid=1 name=zookeeper
    ZOO_OS PROC start pid=2 name=beluga-test
    ZOO_OS PROC start pid=3 name=lemming-test
    ZOO_OS PROC crash pid=3 name=lemming-test fault=page-fault access=read cause=protection addr=0xffffc00000001000
    ZOO_OS PROC start pid=4 name=lemming-test
    ZOO_OS PROC crash pid=4 name=lemming-test fault=page-fault access=read cause=protection addr=0xffffc00000001000
    ZOO_OS PROC start pid=5 name=muskox-test
    ZOO_OS PROC killed pid=5 name=muskox-test by=1
    ZOO_OS PROC exit pid=2 name=beluga-test code=0
    ZOO_OS PROC exit pid=1 name=zookeeper code=0
    ZOO_OS TEST process-cleanup ok

A new kernel self-test, `TEST address-spaces ok`, maps the same user address
in two fresh address spaces to different frames and checks each sees only its
own data and that the kernel half is identical and supervisor-only.

## 10. Shell additions

`ps` (tasks and states), `ls` (initial filesystem), `run <program>` (spawn a
program from the shell; its markers appear while the shell runs).

## 11. Deferred

SMP, `syscall`/`sysret`, FPU/SSE state, shared memory, capability transfer
over channels, targeted wake-ups and priorities, a long-lived Zookeeper
(after the scenario it exits; persistent supervision comes with services
that have real work), restart back-off, a writable filesystem and filesystem
service, process arguments beyond one start-up handle, `SMAP`/`SMEP`.

## 12. Changes made during implementation

- **`kill` system call and MuskoxTest.** The first implementation
  passed every case, but nothing exercised preemption: every program
  eventually blocked by itself. MuskoxTest never yields, so the scenario
  now depends on preemption, and Zookeeper needs `kill` (syscall 10, error
  `KILLED` 12, process-handle right `KILL`) to end it. Killing yourself is an
  exit with code 0; killing a process that already ended does nothing. As a
  negative control, disabling preemption of user tasks made the boot hang
  right after MuskoxTest started (recorded in docs/verification.md).
- **Keyboard screen check.** The boot log is now longer than one screen, so
  the `keyboard` verify case no longer expects the BOOT line on row 0; it
  anchors on `READY`, the typed command, its output and the final prompt,
  and still checks that a wrong string matches nowhere.
- **Memory becomes a global** (`memory::with`, locked only with interrupts
  disabled), because system calls and the scheduler allocate frames. This
  replaces M2's "one `Memory` value passed by `&mut`".
