# Verification record

Status: M0–M4, M5a and the interim console verified on the host below on
2026-10-30. Logs and screenshots are generated under `build/` (ignored by
Git); rerun the commands to reproduce them. The M5a section (version 0.7.0)
is the current state.

**The 0.3.0, M2 and M1 sections were recorded before the rename and quote the
output exactly as it was then**, so they show `ORIGINS_OS` markers,
`origins-os` image names and the `origins>` prompt. Paths containing
`\OriginsOS\` refer to the local checkout folder, which kept its name.

## Host and versions

| Item | Version |
| --- | --- |
| Host OS | Windows 11 Home 10.0.26200 (x86-64), PowerShell + Git Bash |
| Toolchain | `nightly-2026-10-01` → rustc 1.101.0-nightly (21b707e3f 2026-09-30), cargo 1.101.0-nightly (f3865b2a4 2026-09-29), host `x86_64-pc-windows-msvc` |
| Components | rust-src, llvm-tools, rustfmt, clippy; target `x86_64-unknown-none` |
| Linker (host) | MSVC `link.exe` from the existing Visual Studio Build Tools install |
| Crates (host) | bootloader 0.11.17 (UEFI feature only), sha2 0.11.1 |
| Crates (kernel) | bootloader_api 0.11.17, x86_64 0.15.5, pic8259 0.11.0, linked_list_allocator 0.10.6, spin 0.12.3, noto-sans-mono-bitmap 0.3.2, pc-keyboard 0.9.0 (full set in `Cargo.lock`) |
| Crates (host, since console) | noto-sans-mono-bitmap 0.3.2 (re-renders text to check screenshots) |
| Workspace crates (since M3) | `zoo-abi` (shared ABI, ELF and initrd parsers, no dependencies), `zoo-user` (runtime + programs) |
| User programs (M4) | adds peacock-handler 66,144 B (plus an 8.8 MB zero-filled back buffer in `.bss`), owl-handler 16,336 B, chimp-handler 15,184 B; initrd 151,552 B |
| User programs (M3) | zookeeper 18,496 B; monkey-handler 14,392 B; lion-handler 10,616 B; hamster-handler 3,192 B (static ELF at `0x200000`); initrd 51,200 B |
| QEMU | 11.1.0, firmware as recorded in `tools/firmware/README.md` |
| Kernel ELF | M1: 11,504 bytes. M2: normal 60,512 bytes, fault modes 40–46 KB (release); crate `zoo-os-kernel` since 0.4.0 |

## Exact QEMU invocation (verify cases)

    "C:\Program Files\qemu\qemu-system-x86_64.exe" -machine q35 -accel tcg -cpu qemu64 -smp 1 -m 512M
      -drive if=pflash,format=raw,unit=0,readonly=on,file=<OVMF_CODE>
      -drive if=pflash,format=raw,unit=1,file=build\runs\<case>\OVMF_VARS.fd
      -drive format=raw,media=disk,file=build\<mode>\zoo-os-<mode>-uefi.img
      -nic none -display none -monitor none -no-reboot
      -chardev file,id=com1,path=build\runs\<case>\serial.log -serial chardev:com1

`cargo xtask run` uses the same machine with `-serial stdio` instead of the
chardev. Commas in paths are escaped as `,,`. The exact command for each run
is saved in `build/runs/<case>/qemu-command.txt`.

## M5a results (version 0.7.0)

Design: docs/m5a-design.md. User programs now use SSE (custom target
`user/x86_64-zoo.json`, built with `-Zbuild-std`), have a growing heap
(`mem_alloc`) and can map initrd files read-only (`fs_map`). The kernel saves
and restores each user task's x87/SSE state on switches. Guest RAM: 2 GiB.

`cargo xtask verify` from a clean copy of the tracked files (path with spaces
and a comma, empty `target/` and `build/`, so `core`/`alloc` were rebuilt for
the new user target): exit 0 in 141 s, all 11 cases as expected (normal
4.8 s, desktop 13.5 s), `ZOO_OS BOOT version=0.7.0`. An earlier run in the
main checkout gave the same outcomes. The normal case now also requires
`PROC start pid=6/7 name=beaver-handler` and both exits (in any order); the
desktop case's process ids moved up by 2.

From the normal case's serial log:

    [1:zookeeper] mapped bin/zookeeper: 33480 bytes, ELF magic ok
    ZOO_OS PROC start pid=6 name=beaver-handler
    ZOO_OS PROC start pid=7 name=beaver-handler
    ZOO_OS PROC exit pid=6 name=beaver-handler code=0
    ZOO_OS PROC exit pid=7 name=beaver-handler code=0
    [1:zookeeper] both BeaverHandlers matched the reference bits (0x40e4abd67fe8c90c, 0x410b85587192e697)

- **FPU state is per process:** the two BeaverHandlers ran the same 3-million-
  step floating-point series at the same time (preempted many times) with
  different parameters, and each result equals the reference Zookeeper
  computed alone, bit for bit.
- **Heap:** each BeaverHandler fills a 4 MiB `Vec`, which needs the
  allocator to grow past its first 1 MiB with `mem_alloc`.
- **File mapping:** `fs_map` returns exactly the file's bytes (length equals
  `fs_stat`, ELF magic present).
- **No leaks:** `TEST process-cleanup ok` still holds with heap pages and
  ramdisk mappings in play.
- **SSE really used:** `llvm-objdump` of `beaver-handler` shows 22
  `mulsd`/`divsd`/`addsd`/`cvtsi2sd` instructions and no soft-float helper
  calls.

**Negative control (FPU switching).** With `fpu::save` and `fpu::restore` turned into no-ops, the normal image was booted headless: both BeaverHandlers exited 0, but Zookeeper logged `BeaverHandler 0 result 0x40e4abd67fe8c960, expected 0x40e4abd67fe8c90c` and exited with code 24 instead of 0, so the verifier's `PROC exit pid=1 name=zookeeper code=0` marker cannot match. Reverted before the runs above.

**WHPX:** the same normal image boots to READY with `-accel whpx` on the
development host; the timer self-test spun about 13× more than under TCG.
Verify does not use it.

## M4 results (version 0.6.0)

Design: docs/m4-design.md. A user-space window server (PeacockHandler), two
applications (OwlHandler: Files; ChimpHandler: crash test) and Zookeeper's
session mode, started with the shell command `desktop`.

Clean copy of the tracked files at `…\OriginsOS\build\clean checkout, spaces\`, empty
`target/` and `build/`, at version 0.6.0:

    cargo xtask doctor     -> ready, exit 0
    cargo xtask verify     -> exit 0 (70 s including builds)

| Case | Expected | Got | Time |
| --- | --- | --- | --- |
| normal | pass | pass | 3.1 s |
| panic | pass | pass | 3.1 s |
| fault-write-protect | pass | pass | 2.9 s |
| fault-heap-guard | pass | pass | 2.9 s |
| fault-stack-overflow | pass | pass | 2.8 s |
| keyboard | pass | pass | 5.0 s |
| scroll | pass | pass | 5.6 s |
| desktop (new) | pass | pass | 11.9 s |
| negative-wrong-marker | unexpected-marker | unexpected-marker | 1.9 s |
| negative-timeout | timeout | timeout | 5.0 s |
| negative-triple-fault | early-exit | early-exit | 2.0 s |

Afterwards: tracked files unchanged, firmware templates matched their
recorded hashes, no QEMU running.

The `desktop` case types into the guest through QMP in five phases, checks
the kernel markers between them (including two unordered groups where a
window server's apps exit while its replacement starts), and checks five
screenshots. Each screenshot check finds exact text by colour and also
requires a deliberately wrong string to be absent:

    1-desktop:        menu `ZooOS` at (8, 4), focused title `Files` at (32, 52), selected
                      `bin/zookeeper` at (32, 80), Chimp window shown; wrong text rejected
    2-details:        details of the 3rd file: `Name: bin/monkey-handler` at (32, 80),
                      `Size:` at (32, 100); wrong file rejected
    3-app-restart:    status `ChimpHandler crashed; restarted (1 of 3)` at (8, 780),
                      focused `Chimp` at (660, 52), Files details unchanged
    4-server-restart: fresh desktop after the window-server crash: `ZooOS` at (8, 4),
                      `Files` focused at (32, 52), list reset at (32, 80)
    5-console:        kernel console back: `Desktop session ended.` row 5,
                      `zoo> echo back` row 6, `back` row 7; no desktop left

Against the M4 acceptance criteria:

- **Keyboard navigation:** `Down`, `Enter`, `Tab` and `Esc` drive the
  desktop; screenshots 1–3 show the selection, the details view and the
  focus change.
- **Basic file browsing:** Files lists all seven programs in the initial
  filesystem with sizes, and opens the third one's details.
- **Crash recovery:** ChimpHandler's fault ends only ChimpHandler; the window
  server restarts it and Files keeps its state (screenshot 3). The window
  server's own fault (test key F12) releases the display; Zookeeper restarts
  it and a fresh desktop appears (screenshot 4).
- **Stable redraw:** frames are drawn into a back buffer and copied whole.
  All five screenshots were taken mid-session and every checked string was
  complete. This shows complete frames at those moments; it is not a frame-
  timing measurement.
- **Not a static screenshot:** the case is interactive end to end, and leaving
  the desktop returns a working kernel shell.

**Negative control (app restart).** With PeacockHandler's restart limit set to
0, the `desktop` case failed: `timeout: no ZOO_OS PROC start pid=10
name=chimp-handler within 90s`. Reverted before the runs above.

**Bug found by the first run.** The first full run failed only on screenshot
5: the shell printed its prompt before Zookeeper's last lines, so `echo back`
landed on a line of its own. Fixed (docs/m4-design.md §10); the next run
passed.

### M4 additional checks

| Check | Result |
| --- | --- |
| `cargo test -p xtask --locked` | 21 passed (adds unordered marker groups and coloured text search) |
| `cargo test -p zoo-abi` | 8 passed |
| `cargo clippy`: kernel in all 6 modes, `zoo-user`, `zoo-abi`, xtask | 0 warnings |
| `cargo fmt --all --check` | clean |

Not covered by automated checks: typing printable characters into an app
(no app uses them yet), the session's restart limit for the window server
itself (only one restart is exercised), and the `uptime` clock text.

## M3 results (version 0.5.0)

Design: docs/m3-design.md. User processes in separate address spaces,
`int 0x80` system calls, channels with handles, an ustar initrd, and the
Zookeeper scenario run at boot.

Clean copy of the tracked files at `…\OriginsOS\build\clean checkout, spaces\`, empty
`target/` and `build/` (so the user programs and initrd were built from
scratch):

    cargo xtask doctor     -> ready, exit 0
    cargo xtask verify     -> exit 0 (67 s including builds)

| Case | Expected | Got | Time |
| --- | --- | --- | --- |
| normal | pass | pass | 3.8 s |
| panic | pass | pass | 3.4 s |
| fault-write-protect | pass | pass | 3.4 s |
| fault-heap-guard | pass | pass | 3.4 s |
| fault-stack-overflow | pass | pass | 3.4 s |
| keyboard | pass | pass | 4.2 s |
| scroll | pass | pass | 5.0 s |
| negative-wrong-marker | unexpected-marker | unexpected-marker | 2.5 s |
| negative-timeout | timeout | timeout | 5.0 s |
| negative-triple-fault | early-exit | early-exit | 2.4 s |

Afterwards: tracked files unchanged, firmware templates matched their
recorded hashes, no QEMU running. That run was made just before the version
bump (its kernel printed `version=0.4.0`); `cargo xtask verify` was then
rerun in the main checkout on the 0.5.0 build (`ZOO_OS BOOT version=0.5.0`)
with the same ten outcomes.

The normal case now requires, in order, the six self-tests (including the
new `TEST address-spaces ok`), then these kernel markers, then READY:

    ZOO_OS PROC start pid=1 name=zookeeper
    ZOO_OS PROC start pid=2 name=lion-handler
    ZOO_OS PROC start pid=3 name=monkey-handler
    ZOO_OS PROC crash pid=3 name=monkey-handler fault=page-fault access=read cause=protection addr=0xffffc00000001000
    ZOO_OS PROC start pid=4 name=monkey-handler
    ZOO_OS PROC crash pid=4 name=monkey-handler fault=page-fault access=read cause=protection addr=0xffffc00000001000
    ZOO_OS PROC start pid=5 name=hamster-handler
    ZOO_OS PROC killed pid=5 name=hamster-handler by=1
    ZOO_OS PROC exit pid=2 name=lion-handler code=0
    ZOO_OS PROC exit pid=1 name=zookeeper code=0
    ZOO_OS TEST process-cleanup ok

User-process log lines from the same run (not markers; printed as
`[pid:name]`):

    [1:zookeeper] sent `ping 1`, got `pong: ping 1`
    [3:monkey-handler] ZOO-OS PANIC forged marker from user space
    [1:zookeeper] MonkeyHandler crashed (run 1 of 2)
    [1:zookeeper] restart policy allows it: restarting MonkeyHandler
    [1:zookeeper] restart limit reached: giving up on MonkeyHandler
    [5:hamster-handler] HamsterHandler: running on the wheel, never yielding
    [1:zookeeper] sent `ping 2`, got `pong: ping 2`
    [1:zookeeper] HamsterHandler stopped by the supervisor
    [1:zookeeper] Zookeeper: all checks passed

What these show, against the M3 acceptance criteria:

- **Two isolated processes communicate:** Zookeeper and LionHandler exchange
  `ping`/`pong` over a channel. `TEST address-spaces ok` shows two spaces map
  the same user address to different frames and share only the
  supervisor-only kernel half.
- **Invalid handles and malformed requests fail safely:** MonkeyHandler makes
  14 bad requests (unknown syscall, bogus/stale handles, kernel and null
  pointers, oversized length, unknown program, bad exit code, closed peer).
  Each must return the exact error or it exits with a code instead of
  crashing, which would change the markers. Its forged `ZOO_OS PANIC` text
  came out as `ZOO-OS PANIC`, so user text cannot create markers.
- **A crashing process does not affect others:** MonkeyHandler's read of
  kernel memory (`error=0x5`: present page, user mode) ends only that
  process, twice; LionHandler answers `ping 2` afterwards.
- **Supervisor with explicit failure behaviour:** one restart, then give up;
  `kill` for a process that never yields.
- **Preemption:** `ping 2` completes while HamsterHandler spins.
- **No leaks:** the free-frame count after all five processes were reaped
  equals the count before Zookeeper started.

**Negative control (preemption):** with the timer handler changed to never
switch away from a user task, the normal image was booted headless for 45 s.
Output stopped right after
`[5:hamster-handler] HamsterHandler: running on the wheel, never yielding`;
no `ping 2`, no further markers, QEMU killed by the timeout. The verifier's
30 s timeout would reject it. The change was reverted before the runs above.

### M3 additional checks

| Check | Result |
| --- | --- |
| `cargo test -p zoo-abi` | 8 passed: ELF and tar parsers on valid input, every malformed-header and bad-segment case, and every truncation of a valid file (no panics) |
| `cargo test -p xtask --locked` | 19 passed |
| `cargo clippy`: kernel in all 6 modes, `zoo-user`, `zoo-abi`, xtask | 0 warnings |
| `cargo fmt --all --check` | clean |
| Regression after the trap-path and layout changes alone, before any process code | all 10 cases passed; physical window at `0xffff818000000000`, boot stack at `0xffff808000001000` |

Not covered by automated checks: the shell's `ps`, `ls` and `run` commands.

## Rename results (version 0.4.0)

The rename changed names only: crates (`zoo-os-kernel`), images
(`zoo-os-<mode>-uefi.img`), serial markers (`ZOO_OS …`), the shell prompt
(`zoo> `) and docs. No kernel logic changed.

Clean copy of the tracked files at `…\OriginsOS\build\clean checkout, spaces\`,
empty `target/` and `build/`:

    cargo xtask doctor     -> "ZooOS doctor", ready, exit 0
    cargo xtask verify     -> exit 0 (52 s including builds)

| Case | Expected | Got | Time |
| --- | --- | --- | --- |
| normal | pass | pass | 3.1 s |
| panic | pass | pass | 3.0 s |
| fault-write-protect | pass | pass | 2.9 s |
| fault-heap-guard | pass | pass | 2.9 s |
| fault-stack-overflow | pass | pass | 2.9 s |
| keyboard | pass | pass | 3.5 s |
| scroll | pass | pass | 4.2 s |
| negative-wrong-marker | unexpected-marker | unexpected-marker | 1.8 s |
| negative-timeout | timeout | timeout | 5.0 s |
| negative-triple-fault | early-exit | early-exit | 1.9 s |

Afterwards: tracked files unchanged, firmware templates matched their
recorded hashes, no QEMU running. A run in the main checkout gave the same
outcomes, with these screen checks:

    keyboard: 1280x800, 19241 lit pixels; row 0 = BOOT line, row 15 = `zoo> echo hi`,
              row 16 = `hi`; wrong version rejected
    scroll:   43 rows; rows 40-42 = `zoo> echo end`, `end`, prompt; BOOT scrolled off

Kernel output in the keyboard case begins `ZOO_OS BOOT version=0.4.0`.
`cargo test -p xtask --locked`: 19 passed. `cargo clippy`: 0 warnings for
the kernel in all six modes and for xtask. `cargo fmt --all --check`: clean.

## Interim console results (version 0.3.0)

Design: docs/console-design.md. Normal boots now start a debug shell on the
framebuffer and PS/2 keyboard after `READY`, and print `ORIGINS_OS SHELL
ready`. Verify cases that type keys add `-qmp tcp:127.0.0.1:<free
port>,server=on,wait=off` to the QEMU command line and use QMP `send-key`
and `screendump`.

Clean copy of the tracked and new files at
`…\OriginsOS\build\clean checkout, spaces\`, empty `target/` and `build/`:

    cargo xtask doctor     -> ready, exit 0
    cargo xtask verify     -> exit 0 (55 s including builds)

| Case | Expected | Got | Time |
| --- | --- | --- | --- |
| normal | pass | pass | 3.1 s |
| panic | pass | pass | 3.1 s |
| fault-write-protect | pass | pass | 3.0 s |
| fault-heap-guard | pass | pass | 3.0 s |
| fault-stack-overflow | pass | pass | 3.0 s |
| keyboard | pass | pass | 3.7 s |
| scroll | pass | pass | 4.3 s |
| negative-wrong-marker | unexpected-marker | unexpected-marker | 2.0 s |
| negative-timeout | timeout | timeout | 5.0 s |
| negative-triple-fault | early-exit | early-exit | 2.1 s |

Afterwards: tracked files unchanged, firmware templates matched their
recorded hashes, no QEMU running. Two earlier runs in the main checkout gave
the same outcomes (the first before the `scroll` case existed).

Screen checks reported by the run in the main checkout:

    keyboard: 1280x800, 21362 lit pixels; row 0 = BOOT line, row 15 = `origins> echo hi`,
              row 16 = `hi`; wrong version rejected
    scroll:   43 rows; rows 40-42 = `origins> echo end`, `end`, prompt; BOOT scrolled off

"Row N = text" means every pixel of those glyph cells equals the text
re-rendered on the host with the same font and layout. The deliberately
wrong string `ORIGINS_OS BOOT version=9.9.9` is checked against row 0 in
the same run and does not match.

Serial log of the keyboard case after the shell starts (`^H` is the
backspace echo):

    ORIGINS_OS SHELL ready
    Type 'help' for commands.
    origins> echx^H ^Ho hi
    ORIGINS_OS SHELL line=echo hi
    hi
    origins> mem
    ORIGINS_OS SHELL line=mem
    frames: 127911 free of 128172 (511644 KiB free)
    heap:   16 bytes used of 1048576
    origins>

The 16 heap bytes are the shell's line buffer, which keeps its capacity
after `clear()`.

### Console additional checks

| Check | Result |
| --- | --- |
| `cargo test -p xtask --locked` | 19 passed (adds screenshot render/compare and PPM parsing tests) |
| `cargo clippy` on the kernel in all 6 modes, and on xtask | 0 warnings |
| `cargo fmt --all --check` | clean |
| `cargo xtask run` | not re-run with the window in this record; it now opens a QEMU window instead of running headless |

Not covered by automated checks: what the panic and fault kernels draw on
screen (their serial output is checked), `clear`, `uptime`, `keys`,
`panic` and `reboot` typed at the prompt.

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
- Every line containing `ZOO_OS ` is a marker (except `INFO` lines without
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
- The console scrolls by copying video memory, which is fine in QEMU but slow
  on real hardware. Only the PS/2 keyboard is supported; most real PCs need
  USB input. QMP uses a free localhost port picked just before QEMU starts,
  which leaves a small race if another program takes it first.
