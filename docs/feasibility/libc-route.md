# Decision needed: which C library route (POSIX step 5b-3)

Status: **proposal for the owner to decide** (2026-10-31). Nothing here is
implemented. POSIX steps 5b-1 (arguments, clock) are done; file
descriptors (5b-2) and the C library itself (5b-3) depend on this choice,
so they were not started.

## Why it matters

The C library is how ordinary programs, and eventually Wine's Unix side
(docs/feasibility/wine-hosting.md, strategy A), reach the operating
system. The choice decides whether ZooOS grows its own program interface
or also speaks Linux's, which the owner's goal of "Linux compatibility"
touches directly.

## Routes

| Route | What it means | For | Against |
| --- | --- | --- | --- |
| **1. A small libc (picolibc or newlib) on ZooOS's own calls** | Implement the library's ~20 low-level hooks (`read`, `write`, `open`, `sbrk`, …) with ZooOS calls; programs are rebuilt for ZooOS | Smallest step; keeps ZooOS's own clean interface | Embedded-style libraries: thin threads, no `dlopen`; real-world programs and Wine need far more; no existing Linux program runs |
| **2. musl, ported to ZooOS calls** | Replace musl's system-call layer with ZooOS calls; programs are rebuilt for ZooOS | A complete, production libc (Alpine Linux uses it); Wine builds against musl | musl assumes Linux calls everywhere; the port is a translation layer in all but name, maintained by us |
| **3. A Linux system-call personality** | The kernel also accepts Linux x86-64 system calls (the `syscall` instruction, Linux numbers and structures) for programs marked as Linux, as FreeBSD's Linux layer does; stock musl and static Linux binaries run unmodified | Existing static Linux programs run as they are; standard toolchains (`zig cc -target x86_64-linux-musl`) work today; the Wine path gets Linux's well-trodden build | The Linux interface is large and its semantics (`fork`, signals, `mmap`, file descriptors) must be faithful; it shapes the kernel around Linux's design |

## Recommendation

**Route 3**, grown the same way as everything else: a first step that runs
one static "hello" built with `zig cc -target x86_64-linux-musl` (needs
`write`, `exit_group`, `arch_prctl`, `set_tid_address`, `brk`/`mmap`), with
a test case and negative controls, then more calls as real programs need
them. It best serves "run Linux programs, then Windows programs through
Wine", and it can coexist with ZooOS's own calls (native programs keep
`int 0x80`).

Route 1 is the safe choice if the owner prefers ZooOS to have only its
own interface. Route 2 is not recommended: it costs nearly as much as
route 3 and gains no existing programs.

## What the owner is asked

Choose route 1, 2 or 3 (or something else). Until then POSIX work stays
paused and the desktop and hardware tracks continue.
