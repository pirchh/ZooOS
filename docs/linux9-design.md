# Linux step 9 (L9): the address-space layout Wine needs

Status: implemented 2026-10-31 (version 0.49.0). Follows L8
(docs/linux8-design.md). Wine's 64-bit preloader is linked at
`0x7d7d00000000`, reserves the low 2 GiB for Windows programs (which
expect addresses such as `0x400000`), and then maps and unmaps freely
inside that range. Before this step, a ZooOS Linux process had to place
its program between 2 MiB and 1 GiB, kept its mappings between 1 GiB and
16 TiB, treated a reservation as all-or-nothing, and had a fixed 64 KiB
stack.

## 1. Goal

`c/linux/redpoll-test.c` (the redpoll is an Arctic finch) is built like
Wine's preloader: no C library, its own entry point, raw system calls, and
linked at `0x7d7d00000000`. It checks:
- kernel-chosen mappings land high (at or above `0x7f0000000000`);
- the low 2 GiB can be reserved from 64 KiB up with
  `MAP_FIXED_NOREPLACE`;
- a `MAP_FIXED` mapping at `0x400000` works, and `MAP_FIXED_NOREPLACE`
  over it fails with `EEXIST`;
- `munmap` of one page inside it can be mapped again, while the
  neighbouring pages keep their data;
- the program cannot map over its own image (`EEXIST`);
- a recursion uses 4 MiB of stack, with every 64 KiB frame live at once;
- a forked child sees the same layout and data;
- releasing the low reservation frees it again.

## 2. Design

- **A window per process.** Each process has its own window for
  reservations, its own starting point for kernel-chosen ones, and its own
  limit on how many it may have. ZooOS programs keep the `vm` window
  (1 GiB to 16 TiB, 256 reservations). Linux programs get:
  - the whole lower half, from 64 KiB up to the stack;
  - kernel-chosen mappings from `0x7f0000000000` upward (falling back to
    the bottom of the window), so the low 2 GiB stays free unless the
    program asks for it;
  - up to 4096 reservations.
- **The program and its loader are reservations.** At start and at
  `execve`, each image's span becomes a reservation and its pages count
  as committed, so `MAP_FIXED`, `MAP_FIXED_NOREPLACE`, `munmap` and
  `mprotect` treat them like any other mapping. Linux programs may lie
  anywhere from 64 KiB to the stack. Position-independent programs go at
  `0x555555550000` and the dynamic loader at `0x7ff000000000`, as on
  Linux; they were at 16 MiB and 768 MiB in L8.
- **An 8 MiB stack.** It is a reservation below `0x7ffffff10000`; its top
  64 KiB is committed at start, the rest when first touched (L6's lazy
  memory).
- **Carving, as Linux does.**
  - `munmap` frees exactly the range given: its pages are unmapped and
    freed, reservations around it are cut (split in two if need be), and
    lazy ranges are dropped.
  - `MAP_FIXED` carves its range the same way and then reserves it, so
    it can cover several mappings and free space at once (L8 refused
    that).
  - `mprotect` accepts a range covered by several adjacent reservations.
- `fork` copies the window; `execve` sets up the layout again.

## 3. Verification

`linux-redpoll` requires the program's lines, its forked child exiting
0, and `RedpollTest: all checks passed`. All earlier Linux cases
(`linux-hello` … `linux-dovekie`) pass unchanged.

Negative controls (each restored after):

| Change | Result |
| --- | --- |
| Kernel-chosen mappings start low | `FAIL`: exit 10 |
| `munmap` leaves the reservation in place (L8's behaviour) | `FAIL`: exit 41, the carved page cannot be mapped again |
| The program is not a reservation | `FAIL`: the program's own text is replaced (execute fault at `0x7d7d0000165d`) |
| The stack is not extended as used | `FAIL`: write fault below the first 64 KiB (`0x7fffffeffe80`) |

The last control first passed: the compiler had turned the recursion into a
loop that reused one 64 KiB frame, so the test never used more stack.
Each frame now passes a pointer to itself down, keeping all of them live,
and the control fails as it should.

## 4. Building the test

Linking a freestanding program at `0x7d7d00000000` needs the large code
model (`-mcmodel=large`). With the small model, the compiler emits 32-bit
absolute addresses, which cannot reach that high. musl's start-up file
cannot be linked there either, which is why Wine's preloader, like this
test, has none.

## 5. Not in step 9

- Building and running Wine itself (next step, in a WSL Alpine build
  environment the owner set up).
- `mremap`, `MAP_GROWSDOWN`, `personality`.
- Address-space limits (`RLIMIT_AS`) and overcommit accounting for lazy
  memory.
