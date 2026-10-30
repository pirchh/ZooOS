# Initial decisions

Status: initial implementation defaults, with feasibility checks noted below.

## Settled for M0/M1

- Independent kernel, not a Windows or Linux kernel fork.
- Rust-first kernel, no_std and no_main; minimal architecture-specific assembly.
- Architecture: x86-64, one virtual CPU, UEFI; QEMU q35 machine, 512 MiB RAM
  (2 GiB since M5a).
- Kernel format: ELF for x86_64-unknown-none. Not a UEFI application pretending
  to be a kernel; execution must occur after firmware boot services have ended.
- Bootloader: bootloader crate 0.11.17 with matching bootloader_api 0.11.17 as the
  initial candidate. Confirm availability and compiler requirements in M0 before
  locking. A necessary version change must be documented with its reason.
- Firmware: OVMF/EDK II, separate code and per-run writable variable-store copy.
- Test execution: software CPU emulation as the portable default. Hardware
  acceleration can be optional, never required to reproduce the first boot.
- Logging: QEMU COM1 serial output, versioned startup and panic markers.
- Panic: abort semantics; report via serial, then halt. No unwinding or reboot.
- M1 memory ownership: consume the bootloader's documented BootInfo interface.
  Preserve bootloader mappings; no custom allocator or new page-table manager yet.
  (Superseded in M2; see "Resolved in M2".)
- Interrupts: keep disabled until deliberately implemented in M2. A halted M1
  guest is expected and must be terminated by the bounded host test harness.
- Host tooling: a small Cargo xtask program exposes build/run/verify commands.
  Do not require Bash on a Windows development host.

## Resolved in M0/M1 (2026-10-30)

Evidence for each item is in docs/verification.md.

- Toolchain: `nightly-2026-10-01` (rustc 1.101.0-nightly, 2026-09-30), pinned
  in rust-toolchain.toml with rust-src, llvm-tools, rustfmt, clippy and target
  x86_64-unknown-none. Nightly is needed only because bootloader 0.11.17's
  build script compiles its UEFI stage with `-Zbuild-std=core`. The kernel
  uses no unstable features and does not use build-std (x86_64-unknown-none
  ships a prebuilt `core`). The date was chosen as a recent nightly after the
  bootloader release (2026-07-27) and was confirmed by building and booting.
- Bootloader: bootloader 0.11.17 and bootloader_api 0.11.17, pinned with `=`.
  Only the `uefi` feature is enabled; the BIOS path is unused. Disk images are
  made with `bootloader::UefiBoot::create_disk_image` (GPT + FAT ESP).
- Host prerequisite: MSVC linker (already present). WSL and Bash are not
  required; all commands run from PowerShell.
- QEMU 11.1.0 installed with winget (user-approved system install). Firmware is
  the EDK II build bundled with it; see tools/firmware/README.md.
- QEMU machine: `-machine q35 -accel tcg -cpu qemu64 -smp 1 -m 512M`, firmware
  as two pflash drives (code read-only, per-run variable copy), `-nic none`
  (avoids PXE delays), `-display none`, `-monitor none`, `-no-reboot`.
- Serial: a small polled 16550 driver in the kernel (kernel/src/serial.rs)
  rather than an extra crate; port I/O asm is confined to kernel/src/cpu.rs.
  Waits for the UART are bounded.
- Normal and panic kernels are built with separate Cargo target directories
  (`target/kernel-normal`, `target/kernel-panic`) and image directories, and
  the panic kernel is selected by the `panic-test` Cargo feature. The kernel
  also prints `ZOO_OS INFO mode=…`, which the verifier checks, so a
  mixed-up image fails.
- The kernel prints an extra `ZOO_OS INFO` line with memory-map data from
  BootInfo. This is not part of the BUILD.md marker contract; it is evidence
  that kernel code ran with real boot data.
- Verifier: QEMU writes COM1 to a per-case file (`-chardev file`) and the
  harness polls it. Rules are in docs/verification.md. Negative cases: the
  panic image checked against normal expectations (must be rejected for a
  wrong marker) and the normal image with an extra marker that never appears
  (must be rejected by timeout, default 5 s).
- xtask parses its own arguments and uses only `bootloader` and `sha2` (for
  firmware checksums in doctor), to keep the dependency set small.

## Resolved in M2 (2026-10-30)

Full design: docs/m2-design.md. Evidence: docs/verification.md.

- Address-space ownership: the bootloader is configured with
  `dynamic_range_end = 0x0000_7FFF_FFFF_F000`, so all of its mappings stay in
  the lower half. Level-4 entries 256–511 belong to the kernel; the kernel
  checks they are empty at boot and panics otherwise.
- Kernel virtual layout: heap at `0xFFFF_C000_0000_1000` (1 MiB, fixed size)
  with unmapped guard pages on both sides; scratch test pages at
  `0xFFFF_C000_4000_0000`. These are internal kernel addresses, not an ABI.
- Physical memory: a bitmap frame allocator owns `Usable` frames at or above
  1 MiB. `Bootloader` and firmware regions are preserved, not reclaimed.
  Double frees and foreign frames are rejected with errors.
- Single owner, no globals: one `Memory` value holds the page mapper and frame
  allocator and is passed by `&mut`. Handlers never touch it.
- Heap: `linked_list_allocator` as the global allocator. This lifts M1's
  "no allocator" rule for the kernel, as M2 requires.
- Interrupt handlers use the unstable `abi_x86_interrupt` feature (via
  `x86_64`), so the kernel now depends on nightly itself, not only the
  bootloader build. Chosen over hand-written assembly entry stubs to keep
  assembly minimal. The kernel has no inline assembly of its own.
- Timer: legacy 8259 PIC + PIT at ~100 Hz. Local APIC/IOAPIC deferred.
- Double faults run on a dedicated 20 KiB IST stack so stack overflows are
  reported, not turned into a triple fault.
- Test modes: one Cargo feature per mode, mutually exclusive via
  `compile_error!`; one target dir and image per mode. The panic test now
  panics after all self-tests, with interrupts enabled.
- Crate version bumped to 0.2.0, so the BOOT marker reads `version=0.2.0`.
- Pins (exact): x86_64 0.15.5 (latest stable; 0.16 is a release candidate),
  pic8259 0.11.0, linked_list_allocator 0.10.6, spin 0.12.3 (0.10.0 was
  yanked).

## Interim console, pulled forward from M4 (2026-10-30)

The user chose to make the system visible and interactive before M3. Design:
docs/console-design.md. This is a kernel-mode debug console, not the desktop
or a user-space shell.

- Text drawn into the UEFI framebuffer with `noto-sans-mono-bitmap` 0.3.2
  (crate MIT, glyphs from Noto Sans Mono under the SIL OFL), regular 16 px,
  grey on black, 8 px margin, 18 px lines. The bootloader already depends on
  this font family.
- `kprintln!` writes to COM1 and the screen; markers stay on serial for the
  verifier. Panic and fault handlers use `try_lock` on the console so a fault
  while drawing cannot deadlock the report.
- PS/2 keyboard on IRQ1; the handler only queues bytes in a lock-free
  single-producer/single-consumer ring. Decoding uses `pc-keyboard` 0.9.0
  (scancode set 1, US layout) in the shell loop.
- The in-kernel shell owns the `Memory` value, so `mem` reports live state.
- Verification drives the real guest through QMP (`send-key`, `screendump`)
  and checks screenshots pixel for pixel against text re-rendered on the host
  with the same font. A wrong string is checked in the same run to prove the
  comparison can fail.
- `cargo xtask run` now opens a QEMU window (keyboard input goes there);
  `verify` stays headless.
- Version 0.3.0. Normal boots add `ZOO_OS SHELL ready` after `READY`.

## Owner decisions (2026-10-30)

- M5 uses a real instruct model, Qwen2.5-0.5B-Instruct (GGUF `q4_0`,
  Apache-2.0); the download was approved (tools/models/README.md).
  Software emulation is too slow for it; WHPX acceleration is available on
  the development host and may be used for inference, never required by
  verify.
- Opera/WoW stay as long-term goals, reached by building a POSIX layer
  milestone by milestone. GPU reference hardware and test bed undecided.
- The owner asked for autonomous progress: implement, verify, merge and
  continue without asking until told to stop.

## POSIX layer step 2: address space (2026-10-30)

Design: docs/posix2-design.md.

- Windows-shaped calls (reserve, commit, protect, decommit, release, query)
  rather than `mmap` first: they map one-to-one onto what Wine needs, and
  `mmap` can be layered on them later.
- A fixed window `[0x4000_0000, 0x1000_0000_0000)`; 64 KiB-aligned
  kernel-chosen reservations; ≤ 256 reservations and ≤ 1 GiB committed per
  process, checked before allocating.
- Page state is kept only in the page tables (the kernel heap is too small
  for per-page records); a subtree-skipping walker makes large ranges cheap.
- Guard (no-access) pages wait for exception delivery (step 3).
- Calls 32–37, ABI version 6. Version 0.11.0.

## POSIX layer step 1: threads (2026-10-30)

Design: docs/posix1-design.md. First step of the owner-chosen strategy A
(docs/feasibility/wine-hosting.md).

- Processes and threads are split: a `Proc` holds the address space,
  handles and memory cursors; each thread is a scheduler `Task`.
- Calls 27–31 (`thread_create`, `thread_exit`, `futex_wait`, `futex_wake`,
  `set_fs_base`), error 18 `TIMEDOUT`. ABI version 5. Version 0.10.0.
- Any thread's `exit`, crash or kill ends the whole process; the last
  `thread_exit` ends it too.
- Futex waits may return spuriously (the kernel's general wake-ups restart
  blocked calls); callers re-check. `futex_wake` also counts waiters that a
  general wake-up already made runnable.
- Kernel stack slots 16 → 32.
- Threads come before a libc: they need no host tools, and Wine needs them.

## Resolved in M5c-2 (2026-10-30)

Design: docs/m5c2-design.md.

- Guided decoding instead of free-form tool calls: the model picks one of
  four tool words by next-token score and writes the argument with tokens
  restricted to path characters. A 55-token system prompt is computed once
  per model service and reused by rewinding the cache. Chosen because a
  conventional tool prompt (≈ 300 tokens) is minutes per answer even with
  WHPX and close to an hour under TCG.
- Typed commands bypass the model; `create` always shows the broker's plan
  and waits for `y`.
- New programs: ParrotHandler (the Assistant window); ElephantHandler gains a
  service mode. The model service never holds a directory handle.
- The window server now runs three windows: Files on the left, Chimp and the
  Assistant stacked on the right (existing screen checks keep their
  positions). The session Zookeeper passes a home handle down; `fs_root`
  is closed right after narrowing.
- Verify gained `assistant` (TCG, no model), `assistant-model` (TCG, model
  image: responsiveness and cancellation) and `assistant-live` (model
  choices end to end, only with `--accel`, otherwise `SKIP`). The model's
  choices are also checked on the host by `cargo xtask assist-check`.
- OtterHandler's `search` matches all query words against path or contents.
- Version 0.9.0. ABI unchanged (version 4).

## Resolved in M5c-1 (2026-10-30)

Design: docs/m5c1-design.md.

- In-memory filesystem in the kernel: `/bin` (read-only) and `/home/user`
  (seeded from `seed/home/user/`, writable). Not persistent; a disk driver
  and crash consistency stay deferred.
- Directory capabilities with rights READ/CREATE/REMOVE. Paths are always
  relative to a handle; empty, `.` and `..` components and absolute paths are
  rejected, so no path can leave its handle's subtree. No symbolic links.
- `fs_root` only for processes the kernel started (boot Zookeeper, session
  Zookeeper, programs the shell `run`s), once each.
- Calls 19–26; directory calls take an `FsArgs` struct pointer. ABI
  version 4. Version 0.8.0.
- The action broker is a separate process (OtterHandler) holding the home
  handle the assistant will act through; its `undo` removes only folders its
  last `create` made, and only while empty. No delete or file-write tool.
- `seed/` files are stored byte-exact (`.gitattributes`: `-text`) so their
  sizes do not depend on the checkout's line-ending settings.

## Resolved in M5b (2026-10-30)

Design: docs/m5b-design.md.

- Inference engine `zoo-infer`: `no_std` + `alloc`, no unsafe code, one
  dependency (`libm`). The same code runs in the guest and on the host.
- Supported formats: F32, Q4_0, Q8_0 (all the chosen GGUF uses). Weights are
  read in place from the `fs_map`ped file; only norms and biases are
  expanded.
- Correctness evidence: unit tests, plus an independent NumPy reference
  (`tools/reference/`, project-local venv, numpy 2.3.4) compared token for
  token by `cargo xtask infer-check`, plus the in-guest `inference` case
  that must reproduce the host engine's tokens exactly.
- The model travels in a separate model image; other cases keep the small
  image. Missing model: verify stops unless `--skip-model` (case reported
  as not run).
- Verify options `--accel tcg|whpx|kvm` (default tcg; accelerators optional)
  and `--case NAME` (iteration only). Under accelerators QEMU's default CPU
  model is used (`-cpu max` made OVMF fault under WHPX).

## Resolved in M5a (2026-10-30)

Design: docs/m5a-design.md.

- User programs are built for `user/x86_64-zoo.json` (SSE enabled, normal
  float ABI, static, small code model) with `-Zbuild-std`. The kernel stays
  soft-float on `x86_64-unknown-none`.
- Eager per-task FXSAVE/FXRSTOR when switching between user tasks; x87 and
  SIMD faults end only the faulting process.
- `mem_alloc` (syscall 17): grow-only heap at `0x0000_1000_0000_0000`,
  1 GiB per process. `fs_map` (18): read-only, zero-copy initrd file
  mappings at `0x0000_2000_0000_0000`. ABI version 3.
- Guest RAM 2 GiB (was 512 MiB).
- Version 0.7.0.

## Feasibility outcomes (2026-10-30)

Desk studies: docs/feasibility/wine-hosting.md, docs/feasibility/gpu.md.

- Wine hosting: BLOCKED. Wine's Unix side needs a POSIX-like host (libc,
  threads, memory mapping, exceptions to user handlers, sockets, a writable
  filesystem, a display driver). Recommended path if kept: grow a POSIX layer
  and port Wine (as Haiku did), not a native re-implementation.
- GPU and graphics translation: BLOCKED. WoW needs DXVK/vkd3d-proton over a
  Vulkan driver over a kernel GPU driver; none exist, and stock QEMU on a
  Windows host offers no 3D test bed. Recommended reference GPU if pursued:
  current AMD Radeon (open driver stack).
- **Open for the owner:** whether to keep Opera/WoW as goals with that
  programme, which test bed and reference GPU, or to narrow the goals.

## Resolved in M4 (2026-10-30)

Design: docs/m4-design.md. Evidence: docs/verification.md.

- The desktop is user space: PeacockHandler (window server) owns the
  framebuffer and keyboard through two new system calls; applications are
  separate processes talking a small text protocol over channels.
- ABI version 2: calls 11–16 (`ticks`, `sleep`, `try_recv`,
  `display_acquire`, `input_read`, `fs_stat`), errors 13 `BUSY` and 14
  `AGAIN`. Existing numbers unchanged.
- One display owner at a time; the framebuffer is mapped at
  `0x0000_4000_0000_0000` in the owner and unmapped (never freed) when it
  ends. While owned, the kernel console stops drawing and the shell leaves
  the keyboard alone.
- The window server polls input and app channels and sleeps one tick when
  idle, rather than a multi-object wait (deferred).
- Back-buffered drawing (whole frames copied to the framebuffer); text is
  blended with an exact integer formula the verifier reproduces.
- Supervision in two layers: the window server restarts crashed apps (3
  times each); Zookeeper restarts a crashed window server (3 times per
  session). Escape ends the session; F12 is a documented test key that makes
  the window server fault.
- The shell command `desktop` starts the session through a kernel-owned
  control channel; boot still runs the M3 scenario and ends in the shell.
- Verifier: multi-step QMP interactions, several screenshots per case,
  unordered marker groups for concurrent events.
- Version 0.6.0.

## Resolved in M3 (2026-10-30)

Design: docs/m3-design.md. Evidence: docs/verification.md.

- Address space split three ways: user (level-4 entries 0–255, per
  process), bootloader mappings (256–383, via `dynamic_range_start/end`),
  kernel-owned (384–511). Replaces M2's "bootloader in the lower half".
- System calls via `int 0x80` (ABI version 1, numbers 0–10, negative errors
  1–12). `syscall`/`sysret` deferred.
- One assembly entry path (`kernel/src/trap.rs`, naked functions) for the
  timer, system calls and user-reachable faults; a context switch is
  returning another task's saved frame.
- Preemptive round-robin at 100 Hz. The kernel task is preempted only at its
  idle point; blocked system calls are re-run when the task is next
  scheduled after any wake-up.
- `Memory` moves to a global used only with interrupts disabled (replaces
  M2's single owner passed by `&mut`).
- Handles: 16 slots per process, values carry a generation so stale handles
  are rejected. Channels: bidirectional, 8 × 256-byte messages per direction,
  non-blocking send.
- Programs: static ELF at `0x200000` (`-C relocation-model=static`), loaded
  from a ustar initrd passed as the bootloader ramdisk; parsers in the new
  dependency-free `zoo-abi` crate, unit-tested on the host.
- Zookeeper (supervisor) runs a fixed start-up plan and exits; restart
  policy is one restart. LionHandler, MonkeyHandler and HamsterHandler are
  the first handlers. A long-lived Zookeeper is deferred.
- User log text is prefixed `[pid:name]` and sanitised (`ZOO_OS` →
  `ZOO-OS`) so user programs cannot forge verifier markers.
- Version 0.5.0.

## Rename to ZooOS (2026-10-30)

- Project name ZooOS, identifier `zoo-os` (crates `zoo-os-kernel`, images
  `zoo-os-<mode>-uefi.img`), serial markers `ZOO_OS …`, shell prompt `zoo> `.
  Replaces the placeholder OriginsOS everywhere except past verification
  results, which are kept as recorded, and the local checkout folder name.
- Component naming theme: the M3 runtime/service supervisor will be
  **Zookeeper**; services and workers are animal-named handlers
  (`LionHandler`, `ZebraHandler`, …). Names are given only to components that
  exist; none exist yet.
- Name check (web, GitHub, crates.io): no OS or product called ZooOS;
  `zooos` and `zoo-os` free on crates.io. Apache ZooKeeper (a distributed
  coordination service) shares the runtime's name; accepted by the user,
  since searches target "ZooOS" and Zookeeper is an internal component.
  No legal trademark search has been done.
- Version 0.4.0, because the serial marker contract changed.

## M0 must resolve and record

(Resolved above; kept for reference.)

- Exact Rust release or dated nightly supported by the selected bootloader build
  path. Do not guess a version; test it and add rust-toolchain.toml.
- Exact required components and targets, and whether bootloader construction
  needs nightly or build-std. Limit unstable features to demonstrated needs.
- Host prerequisites, linker requirements and tested host OS. Windows PowerShell
  is the user's current environment; a Linux development host would not change
  the guest foundation, but do not silently make WSL mandatory.
- QEMU version, OVMF provenance, licensing, filenames and checksums. Resolve
  configuration through documented paths, not a developer's absolute paths.
- Exact image-generation procedure supported by the bootloader version.

## Deliberately deferred

Kernel structure beyond M1; scheduler policy; user executable format; persistent
filesystem; syscall ABI; IPC serialization; GPU and driver model; UI toolkit;
Windows compatibility host interface; model runtime and hardware acceleration.
Do not invent fixed addresses or public ABIs in this document before validating
the boot contract. Future memory maps must distinguish physical/virtual addresses,
reserved regions, guard pages, ownership and access permissions.

Before M3, document syscall numbering/versioning, handle lifetime, IPC errors,
process exit semantics and executable loading. Before exposing writable storage,
document crash recovery and filesystem consistency. Before loading device drivers,
document interrupt, DMA and ownership boundaries.

## Source references

- https://docs.rs/bootloader/0.11.17/bootloader/
- https://docs.rs/bootloader_api/0.11.17/bootloader_api/
- https://doc.rust-lang.org/rustc/platform-support/x86_64-unknown-none.html

These references justify the starting direction, not a tested local toolchain.

