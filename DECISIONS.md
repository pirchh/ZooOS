# Initial decisions

Status: initial implementation defaults, with feasibility checks noted below.

## Settled for M0/M1

- Independent kernel, not a Windows or Linux kernel fork.
- Rust-first kernel, no_std and no_main; minimal architecture-specific assembly.
- Architecture: x86-64, one virtual CPU, UEFI; QEMU q35 machine, 512 MiB RAM.
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
  also prints `ORIGINS_OS INFO mode=…`, which the verifier checks, so a
  mixed-up image fails.
- The kernel prints an extra `ORIGINS_OS INFO` line with memory-map data from
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
- Version 0.3.0. Normal boots add `ORIGINS_OS SHELL ready` after `READY`.

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

