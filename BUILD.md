# Build and verification contract

Status: implemented for M0/M1 and verified on Windows 11 (2026-10-30). See
docs/verification.md for results and tools/firmware/README.md for setup.

## Environment

Tested host: Windows 11, MSVC linker, rustup with the nightly pinned in
rust-toolchain.toml, QEMU 11.1.0 with its bundled EDK II firmware. Exact
versions are recorded in docs/verification.md. Keep the checkout path short on
Windows hosts without long-path support (see the limitations in that file).

M0 must document prerequisites and exact tested versions, pin the compiler in
rust-toolchain.toml, pin Rust dependencies and commit Cargo.lock. Record firmware
source/version/license and checksum in tools/firmware/README.md. Keep downloaded
firmware out of Git. Use QEMU_X86_64, OVMF_CODE and OVMF_VARS_TEMPLATE environment
variables or equivalent documented configuration. Do not overwrite templates.

The toolchain must distinguish host utilities from the freestanding kernel
(target x86_64-unknown-none). Define target-specific configuration so host xtask
builds do not accidentally inherit the kernel target or linker flags.

## Required repository layout

- kernel/: freestanding Rust kernel and panic handler.
- xtask/: host build, image, run and verification utility.
- .cargo/: aliases and target-specific configuration.
- tools/firmware/README.md: firmware setup and provenance.
- docs/verification.md: actual commands, versions and results.
- build/: generated boot images, per-run firmware state and logs; ignored by Git.
- rust-toolchain.toml, Cargo.toml, Cargo.lock and .gitignore.

## Command interface to implement

Run from the repository root after documented prerequisites are installed:

    cargo xtask doctor
    cargo xtask build
    cargo xtask run
    cargo xtask verify

An alias in .cargo/config.toml should make cargo xtask invoke the host utility.
Doctor checks prerequisites, configured firmware paths and versions without
installing anything. Build creates the kernel and a UEFI-bootable disk image.
Run boots it in QEMU and sends guest COM1 output to the terminal.
Verify runs all M1 cases headlessly with independent serial logs and a finite
configurable timeout (default 30 seconds per case). Paths with spaces must work.
Missing prerequisites produce an actionable error and nonzero exit code.

## Guest and harness contract

Normal guest markers, emitted by kernel code after kernel entry:

    ORIGINS_OS BOOT version=0.1.0
    ORIGINS_OS READY

A separately built deliberate-panic mode must emit the startup marker and then:

    ORIGINS_OS PANIC intentional-test

Normal mode must not emit PANIC. Panic mode must not emit READY after the panic.
Both guests halt after their expected state. The verifier observes output,
terminates QEMU, cleans up the child process, and returns success only for the
correct ordered markers. A timeout, early exit, wrong marker or unexpected panic
fails. Kernel halt is not itself a timeout failure once correct markers have
been observed. A host output stub must not satisfy guest verification.

The harness must show its negative case fails as intended. Preserve serial logs
for review, with separate normal and panic builds so feature/artifact reuse cannot
accidentally select the wrong mode. Document the precise QEMU arguments and
firmware setup actually tested. Do not equate compilation with boot verification.

## Reproducibility

Validate from a clean checkout after prerequisites are installed. Use locked
dependency resolution for supported commands. Network may be needed for initial
dependency setup; document it rather than claiming an offline build. Repeated
build/verify runs must not mutate tracked sources or shared firmware variables.

