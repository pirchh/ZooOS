# Verification record

Status: M0 and M1 verified on the host below on 2026-10-30. Logs are generated
under `build/` (ignored by Git); rerun the commands to reproduce them.

## Host and versions

| Item | Version |
| --- | --- |
| Host OS | Windows 11 Home 10.0.26200 (x86-64), PowerShell + Git Bash |
| Toolchain | `nightly-2026-10-01` → rustc 1.101.0-nightly (21b707e3f 2026-09-30), cargo 1.101.0-nightly (f3865b2a4 2026-09-29), host `x86_64-pc-windows-msvc` |
| Components | rust-src, llvm-tools, rustfmt, clippy; target `x86_64-unknown-none` |
| Linker (host) | MSVC `link.exe` from the existing Visual Studio Build Tools install |
| Crates | bootloader 0.11.17 (UEFI feature only), bootloader_api 0.11.17, sha2 0.11.1 (full set in `Cargo.lock`) |
| QEMU | 11.1.0, firmware as recorded in `tools/firmware/README.md` |
| Kernel ELF | 11,504 bytes (release); disk images 2,162,688 bytes each |

## Exact QEMU invocation (verify cases)

    "C:\Program Files\qemu\qemu-system-x86_64.exe" -machine q35 -accel tcg -cpu qemu64 -smp 1 -m 512M
      -drive if=pflash,format=raw,unit=0,readonly=on,file=<OVMF_CODE>
      -drive if=pflash,format=raw,unit=1,file=build\runs\<case>\OVMF_VARS.fd
      -drive format=raw,media=disk,file=build\<mode>\origins-os-<mode>-uefi.img
      -nic none -display none -monitor none -no-reboot
      -chardev file,id=com1,path=build\runs\<case>\serial.log -serial chardev:com1

`cargo xtask run` uses the same machine with `-serial stdio` instead of the
chardev. Commas in paths are escaped as `,,`. The exact command for each run
is saved in `build/runs/<case>/qemu-command.txt`.

## What the harness checks

- COM1 output is written to `serial.log` by QEMU itself, so only guest output
  can satisfy it. The harness polls that file every 50 ms.
- Every line containing `ORIGINS_OS ` is a marker (except `INFO` lines without
  `mode=`). Markers must match the expected sequence exactly and in order; any
  other marker fails immediately. Normal: `BOOT version=0.1.0`,
  `INFO mode=normal …`, `READY`. Panic: `BOOT version=0.1.0`,
  `INFO mode=panic-test …`, `PANIC intentional-test`.
- After the last marker it watches for 1 s more (`--settle-ms`) so a trailing
  marker (e.g. READY after PANIC) still fails. A halted guest is then killed;
  that is a pass, not a timeout.
- Timeout (default 30 s, `--timeout-secs`), QEMU exiting by itself, or a wrong
  marker all fail. QEMU is always killed and reaped (bounded 10 s wait).
- The `INFO` line reports the number of memory regions and usable bytes read
  from the bootloader's `BootInfo` at runtime.

## Results

### M0: doctor

| Command | Result |
| --- | --- |
| `cargo xtask doctor` | `ready: all prerequisites found`, exit 0 |
| `QEMU_X86_64=C:/nope/qemu.exe cargo xtask doctor` | 3 `[FAIL]` lines (QEMU, and both firmware files, which are found relative to QEMU), each with a fix; `not ready`, exit 1 |

### M1: boot verification

Run from a fresh copy of only the tracked files, at
`…\OriginsOS\build\clean checkout, spaces\` (a path with spaces and a comma),
with empty `target/` and `build/` directories:

    cargo xtask doctor     -> ready, exit 0
    cargo xtask verify     -> exit 0

    OK    normal                 expected=pass              got=pass                 2.7s  qemu terminated by harness
    OK    panic                  expected=pass              got=pass                 2.7s  qemu terminated by harness
    OK    negative-wrong-marker  expected=unexpected-marker got=unexpected-marker    1.8s  qemu terminated by harness
          detail: got `ORIGINS_OS INFO mode=panic-test memory_regions=105 usable_bytes=530128896` while expecting `ORIGINS_OS INFO mode=normal …`
    OK    negative-timeout       expected=timeout           got=timeout              5.0s  qemu terminated by harness
          detail: no `ORIGINS_OS NEVER-EMITTED` within 5s
    [verify] PASS: 2 positive cases passed, 2 negative cases were rejected as intended

A second `verify` in the same copy also exited 0. Afterwards: SHA-256 of
every tracked file was unchanged, both firmware templates still matched the
recorded hashes, and `tasklist` showed no `qemu-system-x86_64.exe` running.

Tail of the normal serial log (bootloader lines, then kernel output):

    INFO : Jumping to kernel entry point at VirtAddr(0x10000001bc0)
    ORIGINS_OS BOOT version=0.1.0
    ORIGINS_OS INFO mode=normal memory_regions=104 usable_bytes=530132992
    ORIGINS_OS READY

Tail of the panic serial log:

    INFO : Jumping to kernel entry point at VirtAddr(0x10000001cd0)
    ORIGINS_OS BOOT version=0.1.0
    ORIGINS_OS INFO mode=panic-test memory_regions=105 usable_bytes=530128896
    ORIGINS_OS PANIC intentional-test
    ORIGINS_OS INFO panic_location=kernel\src\main.rs:40

### Additional checks

| Check | Result |
| --- | --- |
| `cargo test -p xtask --locked` (marker logic) | 14 passed |
| `cargo clippy -p xtask`; `cargo clippy -p origins-os-kernel --target x86_64-unknown-none` (with and without `panic-test`) | no warnings |
| `cargo fmt --all --check` | clean |
| `cargo xtask run --timeout-secs 10` | COM1 markers printed on the terminal; QEMU stopped after 10 s; none left running |
| Early exit: `OVMF_CODE=<12-byte file> cargo xtask verify` | QEMU exited 1 (`pflash0 has invalid size`); all 4 cases `early-exit` in 0.2 s; verify exit 1 |

## Known limitations

- **Windows path length.** On this host `LongPathsEnabled` is 0. Cargo's
  build directory for this nightly is deeper than before, and a checkout whose
  root path is roughly 120+ characters makes MSVC `link.exe` fail with
  `LNK1104` on a 260-character path while building host dependencies
  (observed at exactly 260 characters). Use a shorter checkout path or set
  `CARGO_TARGET_DIR` to a short directory. This is a host limit; enabling
  long paths is a host configuration change and was not made.
- Only TCG (software emulation) is tested. WHPX/KVM acceleration is unused.
- Only the Windows host is tested. Linux firmware fallbacks are untested.
- `bootloader`'s build script runs `cargo install bootloader-x86_64-uefi
  --locked` from crates.io, so the first build needs network access.
- QEMU timings above are functional observations, not performance results.
