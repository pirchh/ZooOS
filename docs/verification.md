# Verification record

Status: M0, M1 and M2 verified on the host below on 2026-10-30. Logs are
generated under `build/` (ignored by Git); rerun the commands to reproduce
them. The M2 section is the current state; the M1 section records the
version 0.1.0 results.

## Host and versions

| Item | Version |
| --- | --- |
| Host OS | Windows 11 Home 10.0.26200 (x86-64), PowerShell + Git Bash |
| Toolchain | `nightly-2026-10-01` → rustc 1.101.0-nightly (21b707e3f 2026-09-30), cargo 1.101.0-nightly (f3865b2a4 2026-09-29), host `x86_64-pc-windows-msvc` |
| Components | rust-src, llvm-tools, rustfmt, clippy; target `x86_64-unknown-none` |
| Linker (host) | MSVC `link.exe` from the existing Visual Studio Build Tools install |
| Crates (host) | bootloader 0.11.17 (UEFI feature only), sha2 0.11.1 |
| Crates (kernel) | bootloader_api 0.11.17, x86_64 0.15.5, pic8259 0.11.0, linked_list_allocator 0.10.6, spin 0.12.3 (full set in `Cargo.lock`) |
| QEMU | 11.1.0, firmware as recorded in `tools/firmware/README.md` |
| Kernel ELF | M1: 11,504 bytes. M2: normal 60,512 bytes, fault modes 40–46 KB (release) |

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

## M2 results (version 0.2.0)

Design: docs/m2-design.md. Six kernel builds (normal, panic and four test
modes), each with its own Cargo feature, target directory and image.

Clean copy of the tracked and new files at
`…\OriginsOS\build\clean checkout, spaces\`, empty `target/` and `build/`:

    cargo xtask doctor     -> ready, exit 0
    cargo xtask verify     -> exit 0   (run 1, 40 s including builds)
    cargo xtask verify     -> exit 0   (run 2, 24 s)

Both runs, identical outcomes:

| Case | Expected | Got | Time (run 1 / 2) |
| --- | --- | --- | --- |
| normal | pass | pass | 2.9 s / 2.9 s |
| panic | pass | pass | 2.9 s / 2.9 s |
| fault-write-protect | pass | pass | 2.8 s / 2.8 s |
| fault-heap-guard | pass | pass | 2.7 s / 2.8 s |
| fault-stack-overflow | pass | pass | 2.9 s / 2.9 s |
| negative-wrong-marker | unexpected-marker | unexpected-marker | 1.8 s / 1.8 s |
| negative-timeout | timeout | timeout | 5.0 s / 5.0 s |
| negative-triple-fault | early-exit | early-exit (QEMU exited, exit code 0) | 1.9 s / 1.9 s |

Afterwards: tracked files unchanged, firmware templates matched their
recorded hashes, no `qemu-system-x86_64.exe` running. An earlier run in the
main checkout gave the same eight outcomes.

Kernel output from the normal case:

    ORIGINS_OS BOOT version=0.2.0
    ORIGINS_OS INFO mode=normal memory_regions=105 usable_bytes=525803520
    ORIGINS_OS INFO memory phys_offset=0x28000000000 stack_bottom=0x18000001000 stack_len=0x20000 heap=0xffffc00000001000+0x100000
    ORIGINS_OS TEST breakpoint ok
    ORIGINS_OS INFO frames total=128202 free=127943
    ORIGINS_OS TEST frames ok
    ORIGINS_OS TEST paging ok
    ORIGINS_OS INFO heap size=1048576 churned_bytes=12582912 used=0
    ORIGINS_OS TEST heap ok
    ORIGINS_OS INFO timer hz=100 ticks=10 spins=534033
    ORIGINS_OS TEST timer ok
    ORIGINS_OS READY

Fault cases (marker line, then details):

    fault-write-protect:
    ORIGINS_OS EXCEPTION page-fault access=write cause=protection addr=0xffffc00040000000
    ORIGINS_OS INFO exception rip=0x0000010000003b10 rsp=0x0000018000020ea0 error=0x3

    fault-heap-guard:
    ORIGINS_OS EXCEPTION page-fault access=read cause=not-present addr=0xffffc00000101000
    ORIGINS_OS INFO exception rip=0x00000100000037fe rsp=0x0000018000020ed0 error=0x0

    fault-stack-overflow:
    ORIGINS_OS EXCEPTION double-fault stack-guard-hit
    ORIGINS_OS INFO double_fault cr2=0x0000018000000f48 stack_bottom=0x0000018000001000

    negative-triple-fault: output stops after the INFO memory line; QEMU exits.

What these show:

- The bootloader kept all its mappings in the lower half (kernel at
  0x100_0000_0000, stack at 0x180_0000_1000, physical window at
  0x280_0000_0000), so the kernel-owned upper half was empty as required.
- 259 frames are in use after the self-tests: 256 heap pages plus 3
  page-table frames created for the upper-half mappings. Test allocations were
  all returned.
- The overflow's faulting address 0x18000000f48 lies in the guard page
  [0x18000000000, 0x18000001000) directly below the stack.
- Error codes match the cause: 0x3 = present + write; 0x0 = not present, read.
- No case rebooted: fault kernels stay halted until the harness stops QEMU,
  and the deliberate triple fault is reported as `early-exit`.

### M2 additional checks

| Check | Result |
| --- | --- |
| `cargo test -p xtask --locked` | 16 passed (adds exact fault-marker and self-test-panic cases) |
| `cargo clippy` on the kernel in all six modes, and on xtask | no warnings |
| Kernel built with two test features at once | rejected: `at most one test-mode feature may be enabled` |
| `cargo fmt --all --check` | clean |

## What the harness checks

- COM1 output is written to `serial.log` by QEMU itself, so only guest output
  can satisfy it. The harness polls that file every 50 ms.
- Every line containing `ORIGINS_OS ` is a marker (except `INFO` lines without
  `mode=`). Markers must match the expected sequence exactly and in order; any
  other marker fails immediately. Every case starts with
  `BOOT version=<crate version>` and `INFO mode=<mode> …`. M2 tails:
  normal: five `TEST <name> ok` lines, then `READY`; panic: the same five,
  then `PANIC intentional-test`; fault modes: one exact `EXCEPTION …` line
  (listed in docs/m2-design.md §9). A self-test failure prints `PANIC
  self-test …`, which never matches.
- After the last marker it watches for 1 s more (`--settle-ms`) so a trailing
  marker (e.g. READY after PANIC) still fails. A halted guest is then killed;
  that is a pass, not a timeout.
- Timeout (default 30 s, `--timeout-secs`), QEMU exiting by itself, or a wrong
  marker all fail. QEMU is always killed and reaped (bounded 10 s wait).
- The `INFO` line reports the number of memory regions and usable bytes read
  from the bootloader's `BootInfo` at runtime.

## M1 results (version 0.1.0)

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
  The timer self-test's `spins` count is not a performance measure.
- M2 timer uses the legacy 8259 PIC and PIT. It works on QEMU q35 with OVMF;
  real hardware will need the local APIC/IOAPIC (deferred).
- The double-fault IST stack is a static array with no guard page of its own.
- Out-of-memory in the heap is not exercised by a test case; by design it
  panics (Rust's default `no_std` allocation-failure handling).
