# Linux step 1 (L1): a static musl program runs unmodified

Status: implemented 2026-10-31 (version 0.39.0); §5 records the
implementation. First step of the owner's
chosen route (docs/feasibility/libc-route.md, route 3): ZooOS accepts
Linux x86-64 system calls, so programs built for Linux with musl run as
they are.

## 1. Goal

One program, `c/linux/hello.c`, built on the host with the pinned Zig
toolchain as an ordinary Linux binary (`zig cc -target x86_64-linux-musl
-static -fno-PIE`), runs on ZooOS and prints its arguments through
`printf`. Nothing in it knows about ZooOS.

## 2. Design

- **Entry.** Linux programs enter the kernel with the `syscall`
  instruction. The kernel enables it (EFER.SCE, LSTAR, STAR, SFMASK
  clearing IF/TF/DF/AC). The entry stub switches to the task's kernel
  stack (a copy of the TSS stack pointer kept for it), pushes the same
  frame an interrupt from user mode would (user SS, RSP, RFLAGS from R11,
  CS, RIP from RCX, error 0, vector `VEC_LINUX`), and joins the existing
  `trap_common`, so saving, scheduling and the `iretq` return are shared.
  Native ZooOS programs keep `int 0x80`.
- **Starting a Linux program.** Programs packed under `linux/` in the
  initrd start with the System V process stack Linux gives: `argc`,
  `argv` pointers, an empty environment, and the auxiliary vector musl
  reads (`AT_PHDR`, `AT_PHENT`, `AT_PHNUM`, `AT_PAGESZ`, `AT_ENTRY`,
  `AT_RANDOM` with 16 bytes from the time-stamp counter, `AT_NULL`), RSP
  16-byte aligned at `argc`. The kernel shell's `linux <program> [args]`
  starts one.
- **System calls in step 1** (Linux numbers and errors), the set a static
  musl `printf` program uses:

  | Call | Behaviour |
  | --- | --- |
  | `write` (1), `writev` (20) | file descriptors 1 and 2 go to the program's log, line by line (`[pid:name] text`); others `EBADF` |
  | `ioctl` (16) | `ENOTTY` (no terminal yet), which makes stdout fully buffered |
  | `arch_prctl` (158) `ARCH_SET_FS` | sets the thread pointer, as ZooOS's own call does |
  | `set_tid_address` (218) | returns the thread id |
  | `getpid` (39), `gettid` (186) | the ids |
  | `exit` (60), `exit_group` (231) | end the process with the code |
  | anything else | `ENOSYS`, logged once per number (`ZOO_OS INFO linux unsupported syscall nr=N`) |

## 3. Verification

Case `linux-hello`: `linux hello alpha` → `Hello from Linux musl: argc=2
argv[0]=hello argv[1]=alpha` in the log, exit code 0. Negative controls:
`writev` unsupported (no output: the case must fail); `argc` written
wrong in the start-up stack (the printed line differs).

## 4. Not in step 1

Files, `mmap`/`brk` (memory allocation), signals, threads (`clone`),
time calls, an environment, dynamic linking. Each comes as a later step
when a real program needs it.

## 5. As implemented

- `kernel/src/trap.rs`: `init_syscall` (MSRs) and `syscall_entry`; the
  kernel stack copy is kept by `gdt::set_kernel_stack`. Dispatch tag
  `VEC_LINUX`, handled by `process::on_linux_syscall`.
- `kernel/src/process.rs`: `spawn_as(…, linux)` loads from `linux/` and
  writes the start-up stack (`linux_stack`); `linux_syscall` implements
  the table in §2. A `writev` is logged as one write (musl sends a line's
  text and its newline as two pieces; the first version logged an empty
  extra line). Writes are limited to 64 KiB.
- `xtask`: `LINUX_PROGRAMS` built with `zig cc -target x86_64-linux-musl
  -static -fno-pie -no-pie -s` and checked by the kernel's ELF rules;
  packed as `linux/<name>`. The program is loaded at 16 MiB, inside the
  user image window.
- The program used no call outside §2's table (no `unsupported syscall`
  line).
- Negative controls: `writev` returning `ENOSYS` → nothing printed, `FAIL
  linux-hello`; argc written one too high → musl printed `argc=3 …
  argv[2]=(null)`, `FAIL linux-hello`.
