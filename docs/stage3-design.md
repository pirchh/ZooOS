# Stage 3: XSAVE, CPU features and an AVX2 backend

Status: implemented 2026-11-03 (version 0.85.0). Part of the Bella roadmap
(docs/bella-roadmap.md). Goal: inference that uses AVX2 and FMA where the
CPU has them, on a kernel that saves the 256-bit registers safely. The
same binary still runs everywhere.

## 1. The kernel saves AVX state (`kernel/src/fpu.rs`)

**Before:** `fxsave64` and `fxrstor64` saved x87 and SSE, 512 bytes per
task. The upper halves of the YMM registers were never saved, so two
programs using AVX would corrupt each other. CR4.OSXSAVE was never set,
so AVX instructions faulted (#UD) instead. Safe, but no AVX.

**Now:**
- **`fpu::init` reads CPUID.** Leaf 1: XSAVE is ECX bit 26, AVX bit 28.
- **With XSAVE:**
  - it sets CR4.OSXSAVE;
  - it writes XCR0 = x87 | SSE | AVX (AVX only if the CPU has it);
  - it reads the image size for exactly those components from CPUID leaf
    0xD (EBX), 832 bytes on current CPUs;
  - it switches tasks with `xsave64` and `xrstor64`.
- **The save area** is 1,024 bytes, 64-byte aligned, as XSAVE requires.
  If the CPU reports a larger image, `init` stays with FXSAVE and leaves
  AVX off.
- **The first 512 bytes keep the FXSAVE layout.** XSAVE's standard
  format starts with it, so the Linux layer's signal frames still carry
  those 512 bytes.
- **State built from a signal frame** (`from_user`) gets an XSAVE header
  saying x87 and SSE come from the frame and AVX from its initial state
  (zeros).
- **The boot log** prints, as an ordinary line rather than a marker:
  `cpu: xsave=… avx=… register state N bytes`.

**Programs check for themselves:** AVX2 is in CPUID leaf 7, FMA and
OSXSAVE in leaf 1, and whether the OS saves YMM state is in XCR0, which
user code reads with `xgetbv` once OSXSAVE is set. No system call is
needed.

## 2. The AVX2 backend (`infer/src/avx2.rs`, `infer/src/cpu.rs`)

- **One switch at the existing seam:** `quant::dot` uses `avx2::dot` when
  `cpu::avx2()` is true, else `dot_portable` (the `core::simd` code: SSE
  on ZooOS's user target).
- **`cpu::avx2()`** checks AVX, FMA and OSXSAVE in CPUID leaf 1, AVX2 in
  leaf 7, and XCR0 bits 1 and 2 once, then remembers the answer.
- **`avx2::dot`** covers F32, Q4_0, Q8_0 and Q6_K:
  - 256-bit loads;
  - nibbles or bytes widened to 32-bit integers and converted to floats;
  - fused multiply-adds;
  - the block scale applied once per block.
  It sums in a different order from the portable code, so results differ
  only by rounding.
- **Unsafe code:** the engine crate was `forbid(unsafe_code)`. It is now
  `deny`, with `allow` only on `avx2`, `cpu`, and the one call in
  `quant::dot`. Each block has a `SAFETY:` comment.
- **`--portable`** (host `infer`, `infer-check`, `assist-check`) forces
  the portable code, to compare the two.

## 3. Checks

**Host** (this PC: Intel Core i9-12900K, AVX2 and FMA; no AVX-512):
- **Unit test `avx2_agrees_with_portable`:** random Q4_0, Q8_0 and Q6_K
  rows and an F32 row agree within 1e-4 relative.
- **`infer-check`** against the NumPy reference, with AVX2: `PASS`, top-5
  logits within 0.00005, against the 0.02 limit.
- **`assist-check`** with AVX2: the same 10 of 14 and 2 of 8, the same
  margins.

| Host, Qwen2.5-0.5B | Portable (SSE) | AVX2 |
| --- | --- | --- |
| System prompt (76 tokens) | 4.7 s | **2.1 s** |
| "Find my pancake recipe" | 1.0 s | **0.5 s** |
| "Make a folder called Projects/2026" | 1.5 s | **0.7 s** |

**Guest** (`--accel whpx`, the virtual CPU of §4):
- **The boot log:** `cpu: xsave=true avx=true register state 576 bytes`.
  WHPX reports 576 from CPUID 0xD, lower than the 832 an AVX image
  needs. The 1,024-byte area holds either.
- **NarwhalTest,** which runs twice in parallel at every boot, now fills
  all sixteen YMM registers with a pattern of its own, spins through many
  task switches, and checks every bit. Both copies log `AVX registers
  kept over 12 ticks`; the test exits with code 5 if any bit changed.
  - **Negative control:** the kernel saving only x87 and SSE with XSAVE →
    both copies `AVX registers changed across task switches`, `FAIL
    normal` (`code=5`).
- **`inference`** (WHPX, AVX2 in the guest): `tokens match the host
  engine exactly`.
- **ElephantHandler** logs `engine avx2`.

| Guest (WHPX) | SSE (Stage 2) | AVX2 |
| --- | --- | --- |
| 0.5B: warm-up, command prompt (76 tokens) | 4,930 ms | **1,823–2,292 ms** |
| 0.5B: warm-up, reply prompt (61 tokens) | 3,102 ms | **1,477–1,856 ms** |
| 0.5B: "Find my pancake recipe", Enter to answer | 1,080–1,102 ms | **520 ms** |
| 0.5B: "whats your name", reply in words | 1,333 ms | **747 ms** |
| 1.5B: warm-up, command prompt | 17,591 ms | **8,368 ms** |
| 1.5B: a request (engine total) | 3,658 ms | **1,970 ms** |

- **Inference is about 2 to 2.5 times faster in the guest.**
- **1.5B now takes about 2 s per request:** twice the 0.5B's time before
  Stage 3, four times its time now. 0.5B stays the default; the owner can
  choose `--bella 1.5b`.

**Also run with XSAVE active** (WHPX), all `OK`:
- `wine-cmd`, `wine-version`, `wine-home-1` and `wine-home-2`;
- the eleven `linux-*` cases, whose signal frames go through `from_user`;
- `bella-live`, `bella-talk`, `everyday-model` and `responsiveness`.

## 4. The virtual CPU

QEMU's default CPU model (`qemu64`) has no AVX, so the guest would never
take the AVX2 path.
- **Tried first:** `-cpu host` under WHPX. OVMF stops with #GP in
  PlatformPei, as `-cpu max` did before.
- **Used:** `qemu64,+xsave,+avx,+avx2,+fma,+f16c`, the plain model plus
  exactly the features the AVX2 path needs. The host CPU must have them;
  this PC (i9-12900K) and the test PC (i7-11700K) do.
- **TCG cases** keep plain `qemu64`, so the portable path stays tested.
- **`ZOO_QEMU_CPU`** overrides the model, for experiments.
