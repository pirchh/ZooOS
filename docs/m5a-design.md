# M5a design: floating point, user memory and file mapping

Status: designed first, then implemented and verified (2026-10-30). First of three
M5 steps (MILESTONES.md "M5 — local assistant prototype"); M5b is the
inference engine, M5c the assistant. Results go in docs/verification.md.

M5's model runtime needs things no ZooOS program has had: floating-point
arithmetic, heap memory that grows, and zero-copy access to a large file (the
weights). These are also POSIX-layer building blocks (owner decision,
DECISIONS.md), so they are designed as general facilities.

## 1. Floating point and SSE in user programs

- **Kernel stays soft-float** (`x86_64-unknown-none`): it never touches
  x87/SSE registers, so user register state only has to be saved when
  switching between user tasks.
- CPU setup at boot: CR0.EM = 0, CR0.MP = 1, CR0.TS = 0; CR4.OSFXSR = 1 and
  CR4.OSXMMEXCPT = 1. A pristine FPU state (`fninit`, default MXCSR) is
  captured once with `fxsave64` as the template for new tasks.
- Each user task owns a 512-byte, 16-byte-aligned FXSAVE area. When the
  scheduler switches away from a user task it runs `fxsave64` into that
  task's area; when it resumes a user task it runs `fxrstor64` from it.
  Eager, simple and correct; lazy switching is deferred.
- x87 (#MF, vector 16) and SIMD (#XM, vector 19) exceptions go through the
  M3 trap path, so they end the faulting process like other user faults.
  Default MXCSR masks all SIMD exceptions.
- **User target:** `user/x86_64-zoo.json`, the `x86_64-unknown-none` spec with
  SSE/SSE2 enabled and the normal (SSE) float ABI, built with `-Zbuild-std`
  (the toolchain is already a pinned nightly with `rust-src`). It also fixes
  `relocation-model: static`, replacing M3's environment variable.
- *Deferred:* AVX state (`xsave`), lazy FPU switching.

## 2. User heap memory (`mem_alloc`, syscall 17)

- `mem_alloc(len)` maps zeroed, writable, no-execute pages at the end of the
  caller's heap region (`0x0000_1000_0000_0000` upward) and returns the start.
  The region only grows. Pages are freed with the process.
- **Bounded:** a process may hold at most 1 GiB of heap; beyond that,
  `NOMEM`. Length 0 or over the limit is `INVAL`.
- `zoo_rt` registers a global allocator (`linked_list_allocator`) that starts
  with 1 MiB and extends itself with `mem_alloc` when it runs out, so user
  programs can use `alloc` (`Vec`, `String`, `Box`).

## 3. Mapping initial-filesystem files (`fs_map`, syscall 18)

- `fs_map(index, out)` maps the `index`-th initrd file **read-only** (user,
  no-execute) into the caller's mapping region (`0x0000_2000_0000_0000`
  upward) and writes its address and length. Zero-copy: the pages are the
  ramdisk's own, never given to the frame allocator, and unmapped (not freed)
  when the process ends, as for the framebuffer (M4).
- Tar data starts on 512-byte boundaries, so the mapping covers whole pages
  around the file; neighbouring archive bytes on the first and last page are
  readable too. The initrd is public, read-only data, so this exposes nothing.
- *Deferred:* writable or private mappings, unmapping before exit.

## 4. Machine size

Guest RAM goes from 512 MiB to **2 GiB**, enough for the ~430 MB model plus
working memory in M5b. Verification stays on software emulation (TCG); WHPX
works on the development host (checked 2026-10-30) and may be used for
inference benchmarks, but is never required.

## 5. Verification

ABI version 3. A new program, **BeaverHandler**, is added to Zookeeper's
start-up plan (after HamsterHandler):

- Zookeeper computes two floating-point reference values itself (a long
  `sqrt`/multiply series with two different parameters), then starts two
  BeaverHandlers, sending each a parameter. Both compute the same series at
  the same time, long enough to be preempted many times, keeping
  intermediate values in a growing `Vec` (exercising `mem_alloc`). Each sends
  back its result's exact bits; Zookeeper compares them with its references.
  **If FPU state leaked between processes, the results would differ.**
- Zookeeper also maps `bin/zookeeper` with `fs_map` and checks the ELF magic
  and that the length matches `fs_stat`.
- New kernel markers: two `PROC start … beaver-handler` and their two exits
  (unordered: they run concurrently). The leak check at the end covers the
  heap pages and mappings.
- **Negative control:** with `fxsave`/`fxrstor` disabled, the float check
  must fail.
