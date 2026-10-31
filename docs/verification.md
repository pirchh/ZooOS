# Verification record

Status: M0–M5 (M5 as a prototype) and the interim console verified on the
host below on 2026-10-30. Logs and screenshots are generated under `build/`
(ignored by Git); rerun the commands to reproduce them. The L3 section
(version 0.41.0) is the current state. Sections before it
quote the program names in use at the time (for example LionHandler,
OwlHandler).

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

## Linux step 3 results: folders, working directory, uname, environment (version 0.41.0)

Design: docs/linux3-design.md.

**Full `cargo xtask verify`** (TCG): exit 0 in 15 min 10 s, every case as
expected (`assistant-live` `SKIP`). New case `linux-skua`: `uname Linux
x86_64`, `HOME=/home/user USER=user`, `/home/user: Documents Music
Photos`, `cwd /home/user; Documents/notes.txt starts "Shopping: flour,
eggs, milk."`, `all checks passed`, exit 0, no unsupported calls (a first
run showed `fcntl`, then added).

Then, under WHPX: `assistant-live` pass in 48.8 s; `linux-skua` pass in
3.8 s.

**Negative controls** (temporary edits to `kernel/src/process.rs`,
restored):

| Change | Result |
| --- | --- |
| No `.` and `..` in listings | `FAIL linux-skua`: `FAILED readdir . and ..`, exit 21 |
| `..` not applied in paths | `FAIL linux-skua`: `FAILED chdir ..`, exit 34 |
| Empty environment | `FAIL linux-skua`: `FAILED environment`, exit 11 |

The first attempt at the first control did not apply its edit (`cargo
fmt` had reformatted the line) and so passed; it was rerun with the edit
applied and failed as above.

Other checks: `cargo clippy` (kernel normal and `fast-boot`, `xtask`): 0
warnings. `cargo test -p xtask --locked`: all passed. Not run: real
hardware.

## Linux step 2 results: memory, read-only files and time (version 0.40.0)

Design: docs/linux2-design.md.

**Full `cargo xtask verify`** (TCG): exit 0 in 15 min 59 s, every case as
expected (`assistant-live` `SKIP`). New case `linux-eider` (7.2 s):
`EiderTest: memory ok`, `/etc/services 180 bytes, 5 lines`, `slept 200
ms`, `unix=…` 0 s from the host clock, `all checks passed`, exit 0; no
`unsupported syscall` line (a first run showed `nr=204`,
`sched_getaffinity`, which was then added). `linux-hello` unchanged.

Then, under WHPX: `assistant-live` pass in 58.1 s; `linux-eider` pass in
4.3 s.

**Negative controls** (temporary edits to `kernel/src/process.rs`, each
restored and checked byte for byte):

| Change | Result |
| --- | --- |
| `mmap` always fails | `FAIL linux-eider`: `FAILED small malloc (errno 12)`, exit 10 |
| `fstat` size one byte too big | `FAIL linux-eider`: `FAILED read size`, exit 24 |
| `nanosleep` returns at once | `FAIL linux-eider`: `FAILED slept too little`, exit 32 |

Other checks: `cargo clippy` (kernel normal and `fast-boot`, `xtask`): 0
warnings. `cargo test -p xtask --locked`: all passed. Not run: real
hardware.

## Linux step 1 results: a static musl program runs unmodified (version 0.39.0)

Design: docs/linux1-design.md (§5 as implemented). Owner decision: route
3 in docs/feasibility/libc-route.md.

**Full `cargo xtask verify`** (TCG): exit 0 in 12 min 38 s, every case as
expected (`assistant-live` `SKIP`). New case `linux-hello` (6.0 s):
`linux hello alpha` → `PROC start pid=26 name=hello`, `[26:hello] Hello
from Linux musl: argc=2 argv[0]=hello argv[1]=alpha`, `PROC exit pid=26
name=hello code=0`; no `unsupported syscall` line. The program is built
with `zig cc -target x86_64-linux-musl -static` from plain C with
`printf`.

Then, under WHPX: `assistant-live` pass in 49.3 s; `linux-hello` pass in
4.0 s.

**Negative controls** (temporary kernel edits, reverted):

| Change | Result |
| --- | --- |
| `writev` returns `ENOSYS` | `FAIL linux-hello` (timeout): nothing printed |
| argc written one too high in the start-up stack | `FAIL linux-hello` (timeout): musl printed `argc=3 argv[0]=hello argv[1]=alpha argv[2]=(null)` |

Other checks: `cargo clippy` (kernel normal and `fast-boot`, `xtask`): 0
warnings. `cargo test -p xtask --locked`: all passed. Not run: real
hardware.

## Desktop step 10 results: a cursor in Notes, and a start-up race fixed (version 0.38.0)

Design: docs/d10-design.md.

**A race found on the way.** The first two full runs of this step each
failed one case that the step does not touch: `persist-3` (timeout) and
then `keyboard` (screen check). Same cause: BadgerHandler loads the disk
after it starts, and Zookeeper reported `ready` before that, so the shell
prompt could appear first and the service's messages printed after it
(`zoo> [23:badger-handler] …`; the typed line then had no prompt).
Fixes, in this step:

1. Zookeeper sends `ready` only after the home-folder root arrives (at
   most 300 ticks). Its first version checked at the top of its loop and
   so waited for the next 1 s health ping: fast boot measured 1,130 /
   1,127 / 1,124 ms kernel-to-shell. Checking after each round's
   messages restored 107 / 111 / 107 ms (WHPX, three runs each).
2. The harness gained `WaitSeen` (a line anywhere in the log) for the
   load lines in `persist-2`, `persist-3` and `notes-2`, which no longer
   assume an order against `SHELL ready`. A check that it still fails
   when the line never comes (a temporary wait for "generation 9"):
   `FAIL persist-2` (timeout).

**Full `cargo xtask verify`** (TCG, with both fixes): exit 0 in 12 min 13 s, every case
as expected (`assistant-live` `SKIP`). New case
`notes-cursor`: `a b c`, Left, Left, `X`, `Y`, Backspace → `Notes: saved
70 bytes`; the screen shows `aX|bc`.

Then `--accel whpx --case assistant-live`: pass in 48.0 s.

**Negative control** (temporary edit to Notes, reverted):

| Change | Result |
| --- | --- |
| Left ignored | `FAIL notes-cursor`: `aX|bc not on screen` |

Other checks: `cargo clippy` (`zoo-user`, `xtask`): 0 warnings. `cargo
test -p xtask --locked`: all passed. Not run: real hardware.

## Desktop step 9 results: double-click to zoom (version 0.37.0)

Design: docs/d9-design.md.

**Full `cargo xtask verify`** (TCG): exit 0 in 12 min 8 s, every case as
expected (`assistant-live` `SKIP`). New case `zoom` (9.9 s): double-click
on the Assistant's title → `zoomed Assistant`, its title at (32, 52), the
other titles covered, close button at (1242, 60); double-click again →
`restored Assistant`, title back at (660, 416), Files visible.

Then `--accel whpx --case assistant-live`: pass in 49.4 s.

**Negative control** (temporary edit to PeacockHandler, reverted):

| Change | Result |
| --- | --- |
| Double-clicks never detected | `FAIL zoom` (timeout: no `zoomed` line) |

Other checks: `cargo clippy` (`zoo-user`, `xtask`): 0 warnings. `cargo
test -p xtask --locked`: all passed. Not run: real hardware.

## Desktop step 8 results: resize windows (version 0.36.0)

Design: docs/d8-design.md.

**Full `cargo xtask verify`** (TCG): exit 0 in 12 min 40 s, every case as
expected (`assistant-live` `SKIP`). New case `resize` (8.7 s): Crash
Test's corner dragged by (−200, −100) → `resized Crash Test to 404x240`;
screenshot: focused, its text shown, close button at (1042, 60) and not at
(1242, 60), desktop colour at (900, 300) below the new bottom edge.

Then `--accel whpx --case assistant-live`: pass in 49.0 s.

**Negative control** (temporary edit to PeacockHandler, reverted):

| Change | Result |
| --- | --- |
| Resize corner never grabs | `FAIL resize` (timeout: no `resized` line) |

Other checks: `cargo clippy` (`zoo-user`, `xtask`): 0 warnings. `cargo
test -p xtask --locked`: all passed. Not run: real hardware.

## Clean-copy verification of version 0.35.0 (2026-10-31)

Every step's record so far said "Not run: a clean-copy verify". This run
closes that for the current state: a fresh `git clone` of `main` at
`d6d93d3` (merge of hardware step 5) into an empty directory, with only
the two Git-ignored downloads copied in from the development tree (the
pinned Zig 0.16.0 toolchain, `tools/zig/zig-x86_64-windows-0.16.0`, and
the checksum-pinned model file in `tools/models/`), then:

| Command | Result |
| --- | --- |
| `cargo xtask doctor` | `ready: all prerequisites found` |
| `cargo xtask verify` (TCG, everything built from nothing) | exit 0 in 15 min 56 s; 35 cases as expected, `assistant-live` `SKIP`; `PASS` |
| `cargo xtask verify --accel whpx --case assistant-live` | pass in 50.6 s |

The clone's working tree stayed clean (no tracked file changed by the
build). So the merged state builds and verifies from Git plus the
documented downloads alone.

## Hardware step 5 results: key repeat for USB keyboards (version 0.35.0)

Design: docs/h5-design.md.

**Full `cargo xtask verify`** (TCG): exit 0 in 14 min 35 s, every case as
expected (`assistant-live` `SKIP`). New case `usb-repeat` (9.3 s): `x`
held for 1.5 s through QMP → `SHELL line=` with 34 x's (at least ten
required; the quiet wait after release proves the repeat stopped).

Then `--accel whpx --case assistant-live`: pass in 51.2 s.

**Negative controls** (temporary edits to `kernel/src/xhci.rs`, reverted):

| Change | Result |
| --- | --- |
| No repeat | `FAIL usb-repeat`: `got ZOO_OS SHELL line=x while expecting ZOO_OS SHELL line=xxxxxxxxxx…` |
| Repeat not stopped on release | `FAIL usb-repeat`: Enter kept repeating, `got ZOO_OS SHELL line= after the final expected marker` |

Other checks: `cargo clippy` (kernel, `xtask`): 0 warnings. `cargo test
-p xtask --locked`: all passed. Not run: real hardware.

## Hardware step 4 results: USB hubs (version 0.34.0)

Design: docs/h4-design.md.

**Full `cargo xtask verify`** (TCG): exit 0 in 14 min 58 s, every case as
expected (`assistant-live` `SKIP`). New case `usb-hub` (7.8 s): `port=5
vendor=0x0409 product=0x55aa class=9` (QEMU's hub), `port=5.1
hid=keyboard … input ready`, `port=5.2 hid=mouse … input ready`,
`devices=3`; `echo hub` typed through the keyboard behind the hub →
`SHELL line=echo hub`. `usb` and `usb-desktop` pass unchanged.

Then `--accel whpx --case assistant-live`: pass in 50.1 s.

**Negative control** (temporary edit to `kernel/src/xhci.rs`, reverted):

| Change | Result |
| --- | --- |
| Hub port left out of the route string | `FAIL usb-hub` (timeout): `port=5.1 error=command failed`, `port=5.2 error=command failed`, `devices=1` |

Other checks: `cargo clippy` (kernel normal and `fast-boot`, `xtask`): 0
warnings. `cargo test -p xtask --locked`: all passed. Not run: a
clean-copy verify; real hardware.

## Hardware step 3 results: the local APIC timer (version 0.33.0)

Design: docs/h3-design.md.

**Full `cargo xtask verify`** (TCG): exit 0 in 13 min 24 s, every case as
expected (`assistant-live` `SKIP`), all on `timer source=lapic
pm-timer=0x608`. New case `timer-rate`: `TernTest: waited` came 4,964 ms
after `waiting 500 ticks` (4–6 s required); 4,965 ms and, under WHPX,
4,958 ms in the earlier single runs. `fast-boot` under WHPX: pass with
`source=lapic`.

Then `--accel whpx --case assistant-live`: pass in 58.5 s
(`source=lapic per-tick=125071`).

PIT fallback (a temporary edit forcing it): `timer-rate` 4,958 ms and
`desktop` pass with `source=pit reason=forced-for-test`.

**Negative control** (temporary edit to `kernel/src/apic.rs`, reverted):

| Change | Result |
| --- | --- |
| Calibration doubled | `FAIL timer-rate` (`harness-error`): `waited came 10012 ms after the previous line (expected 4000-6000 ms)` |

Other checks: `cargo clippy` (kernel normal, `fast-boot`, `panic-test`;
`xtask`): 0 warnings. `cargo test -p xtask --locked`: all passed. Not
run: a clean-copy verify; real hardware.

## Hardware step 2a results: ready for a USB stick (version 0.32.0)

Plan and risks: docs/usb-boot.md.

**Full `cargo xtask verify`** (TCG): exit 0 in 14 min 31 s, every case as
expected (`assistant-live` `SKIP`). New case `big-screen` (9.0 s, test-only
mode whose boot configuration asks for a screen at least 2000 pixels
wide): QEMU gave 2560×1600; `PeacockHandler: display 2560x1600: using
1920x1200`; the desktop checks pass and the area right of x=1920 is black.

`cargo xtask usb-image`: `build/usb/zoo-os-usb.img (3211264 bytes)` with
its SHA-256 printed; no device written.

Then `--accel whpx --case assistant-live`: pass in 63.0 s.

**Negative control** (temporary edit to PeacockHandler, reverted):

| Change | Result |
| --- | --- |
| Large screens refused, as before | `FAIL big-screen`: `got ZOO_OS PROC exit pid=27 name=peacock-handler code=3 while expecting ... files` |

Other checks: `cargo clippy` (kernel normal, `fast-boot`, `big-screen`;
`zoo-user`; `xtask`): 0 warnings. A first version set the screen request
in the kernel's bootloader configuration, which is deprecated in
bootloader 0.11; it moved to the disk image's boot configuration. `cargo
test -p xtask --locked`: all passed. Not run: a clean-copy verify; real
hardware.

## Hardware step 1b results: USB keyboard and mouse input (version 0.31.0)

Design: docs/h1b-design.md.

**Full `cargo xtask verify`** (TCG): exit 0 in 17 min 24 s, every case as
expected (`assistant-live` `SKIP`).

| Case | Time | What it shows |
| --- | --- | --- |
| usb | 8.9 s | both devices `input ready`; `echo usb` typed → `usb keyboard input`, `SHELL line=echo usb` |
| usb-desktop | 11.7 s | `desktop` typed through the USB keyboard; a USB mouse move and click → `usb mouse input`, `click focuses Crash Test`; screenshot: Crash Test focused, Files not |

Then `--accel whpx --case assistant-live`: pass in 58.1 s.

**Negative controls** (temporary edits to `kernel/src/xhci.rs`, reverted):

| Change | Result |
| --- | --- |
| USB key presses not queued | `FAIL usb` (timeout): `no ZOO_OS SHELL line=echo usb within 30s`; so the keys did not reach the PS/2 keyboard either |
| USB mouse y not negated | `FAIL usb-desktop` (timeout): the click landed on the Assistant (`click focuses Assistant`) |

Other checks: `cargo clippy` (kernel normal and `fast-boot`, `xtask`): 0
warnings. `cargo test -p xtask --locked`: all passed. Not run: a
clean-copy verify; real hardware.

## Hardware step 1a results: USB host controller, enumeration (version 0.30.0)

Design and details: docs/h1-design.md.

**Full `cargo xtask verify`** (TCG): exit 0 in 15 min 18 s, every case as
expected (`assistant-live` `SKIP`); boots without a USB controller log
`ZOO_OS INFO usb none`. New case `usb` (6.2 s, with `qemu-xhci`,
`usb-kbd`, `usb-mouse`): `usb xhci ports=8 slots=32 context=32`; port 5
`hid=keyboard`, `vendor=0x0627 product=0x0001 class=3`; port 6
`hid=mouse`, same ids; `usb devices=2`; then `SHELL ready`.

Then `--accel whpx --case assistant-live`: pass in 68.0 s.

Boot time (fast-boot, WHPX, kernel's first line → `SHELL ready`, seven
runs): 110, 168, 160, 110, 117, 109, 112 ms (before: 108–110 ms). A first
version that scanned all 256 PCI buses measured 226, 216, 282 ms and was
replaced by a bridge walk.

**Negative controls** (temporary edits to `kernel/src/xhci.rs`, reverted):

| Change | Result |
| --- | --- |
| Event ring read with the wrong cycle bit | `FAIL usb` (timeout): `port=5 error=command timed out`, `port=6 error=command timed out`, `devices=0` |
| USB 2 port reset skipped | passed: QEMU enables USB 2 ports without a reset, so this path is not tested |

Other checks: `cargo clippy` (kernel normal and `fast-boot`, `xtask`): 0
warnings. `cargo test -p xtask --locked`: all passed. Not run: a
clean-copy verify; real hardware.

## Desktop step 7 results: Files browses the home folder (version 0.29.0)

Design: docs/d7-design.md (§4 as implemented).

**Full `cargo xtask verify`** (TCG): exit 0 in 14 min 49 s, every case as
expected (`assistant-live` `SKIP`). Changed checks: `desktop` (`Home` with
`Documents/` selected; Down, Down, Enter opens `Home/Photos` with
`README.txt (34 bytes)` selected; kept through Crash Test's restart; reset
after the window server's restart), `fast-boot`, and `assistant-model`'s
busy-desktop check (`Photos/` selected while the model works). New case
`files-browse`: the preview of `Home/Documents/notes.txt` shows its lines
(no stray `?`); after going into `Photos` and back up, `Photos/` is
selected.

Then `--accel whpx --case assistant-live`: pass in 57.7 s.

**Negative control** (temporary edit to Files, reverted):

| Change | Result |
| --- | --- |
| Going up forgets the folder you came from | `FAIL files-browse`: `Photos/ not on screen` (as the selection) |

Other checks: `cargo clippy` (`zoo-user`, `xtask`): 0 warnings. `cargo
test -p xtask --locked`: all passed. Not run: a clean-copy verify; real
hardware.

## Desktop step 6 results: start in the desktop (version 0.28.0)

Design: docs/d6-design.md (§3 as implemented).

**Full `cargo xtask verify`** (TCG): exit 0 in 14 min 24 s, every case as
expected (`assistant-live` `SKIP`). The `fast-boot` case (7.5 s) now
shows `ZOO_OS SHELL autostart desktop`, the session's processes (pids
5–9) and the display, a desktop screenshot passing `check_desktop_started`;
after Escape, `run tern-test fast` (pid 10) exits with code 0.

Then `--accel whpx --case assistant-live`: pass in 57.1 s.

**Negative control** (temporary edit to the kernel shell, reverted):

| Change | Result |
| --- | --- |
| No autostart | `FAIL fast-boot` (timeout): `no ZOO_OS SHELL autostart desktop within 30s` |

Other checks: `cargo clippy` (kernel normal and `fast-boot`, `xtask`): 0
warnings. `cargo test -p xtask --locked`: all passed. Not run: a
clean-copy verify; real hardware.

## Desktop step 5 results: date and time in the menu bar (version 0.27.0)

Design: docs/d5-design.md (§3 as implemented).

**Full `cargo xtask verify`** (TCG): exit 0 in 14 min 18 s, every case as
expected (`assistant-live` `SKIP`). New case `menu-clock`: the menu bar
showed `Sat 31 Oct 06:33 UTC`, the host's UTC time of the screenshot. New
unit test `menu_clock_text_known_dates` (1970-01-01 Thursday, 2000-02-29
Tuesday, 2026-10-31 Saturday): passed.

Then `--accel whpx --case assistant-live`: pass in 60.4 s.

**Negative control** (temporary edit to PeacockHandler, reverted):

| Change | Result |
| --- | --- |
| Date one day ahead | `FAIL menu-clock`: `menu bar does not show Sat 31 Oct 06:29 UTC or Sat 31 Oct 06:28 UTC` |

Other checks: `cargo clippy` (`zoo-user`, `xtask`): 0 warnings. `cargo
test -p xtask --locked`: all passed. Not run: a clean-copy verify; real
hardware.

## Boot step 1 results: fast boot (version 0.26.0)

Design and measurements: docs/b1-design.md. Kernel feature and image mode
`fast-boot` (no boot-check programs); `serial-times.log` per run.

**Full `cargo xtask verify`** (TCG): exit 0 in 12 min 31 s, every case as
expected (`assistant-live` `SKIP`); all existing cases still use the normal
build with every check. New case `fast-boot` (4.5 s): self-tests, then
`PROC start pid=1 name=zookeeper` (supervise mode) with BadgerHandler,
BeaverHandler and SealTest as pids 2–4, `READY`, `SHELL ready`, and `run
tern-test fast` → `argc=2 argv=tern-test,fast`, exit code 0.

Then `--accel whpx --case assistant-live`: pass in 62.2 s.

**Measurement** (`--skip-model --accel whpx`, three runs each), kernel's
first line → `SHELL ready`: normal 1,072 / 1,058 / 1,075 ms; fast-boot 108
/ 108 / 110 ms. QEMU start → `SHELL ready`: normal 2,090 / 1,973 / 2,099
ms; fast-boot 1,136 / 1,284 / 1,238 ms (the rest is UEFI firmware).

**Negative controls:**

| Change | Result |
| --- | --- |
| `fast-boot` condition forced false (checks run anyway) | `FAIL fast-boot`: `got ZOO_OS PROC start pid=2 name=beluga-test while expecting ... badger-handler` |
| Two mode features enabled together (`fast-boot,panic-test`) | the kernel does not compile: `at most one mode feature may be enabled` |

Other checks: `cargo clippy` (kernel normal and `fast-boot`, `xtask`): 0
warnings (one lint allowed on the mode-count assertion, which is always
true in a build that compiles). `cargo test -p xtask --locked`: all
passed. Not run: a clean-copy verify; real hardware.

## Desktop step 4 results: Notes (version 0.25.0)

Design: docs/d4-design.md (§4 as implemented).

**Full `cargo xtask verify`** (TCG): exit 0 in 13 min 35 s, every case as
expected (`assistant-live` `SKIP`), the dock case on the four-tile layout.
New cases, two boots of one data disk:

| Case | Time | What it shows |
| --- | --- | --- |
| notes-1 | 12.6 s | Notes opened from the dock (`PROC start pid=31 name=notes`); `Hello from Notes` and `second line` typed; `Notes: saved`, then `BadgerHandler: saved generation 1 to slot A`; screen: `(saved)`, the original two lines without stray `?`, then the typed lines in order |
| notes-2 | 9.8 s | reboot: `loaded generation 1 from slot A`; Notes shows the typed lines again |

Then `--accel whpx --case assistant-live`: pass in 61.5 s.

**Negative controls** (temporary edits to Notes, reverted):

| Change | Result |
| --- | --- |
| No save on the timer | `FAIL notes-1` (timeout waiting for `Notes: saved`); `FAIL notes-2` (timeout: nothing to load) |
| CRLF not handled (the first version's bug) | `FAIL notes-1`: `wrong text Shopping: flour, eggs, milk.? matched at (208, 152)` |

Other checks: `cargo clippy` (`zoo-user`, `xtask`): 0 warnings. `cargo
test -p xtask --locked`: all passed. Not run: a clean-copy verify; real
hardware.

## POSIX step 5b-1 results: process arguments and the clock (version 0.24.0)

Design: docs/posix5b1-design.md (§4 as implemented). Command lines in
`spawn`, calls 57 `args_get` and 58 `clock` (ABI version 16), CMOS RTC at
boot (`ZOO_OS INFO clock unix=…`), C program `tern-test`, harness action
`WaitClock`.

**Full `cargo xtask verify`** (TCG): exit 0 in 13 min 0 s, every case as
expected (`assistant-live` `SKIP`). New case `args-clock` (7.4 s): `run
tern-test alpha beta` → `TernTest: argc=3 argv=tern-test,alpha,beta`;
`TernTest: unix=…` 0 s from the host clock (allowed 120 s); `TernTest:
monotonic +500 ms` across a 50-tick sleep; exit code 0.

Then `--accel whpx --case assistant-live`: pass in 61.2 s.

**Negative controls** (each temporary kernel edit reverted):

| Change | Result |
| --- | --- |
| RTC values not BCD-decoded | `FAIL args-clock`: the date read as impossible, the clock stayed unset and TernTest exited with code 12 (`unexpected-marker`) |
| Year read as 1999 + YY (a plausible wrong time) | `FAIL args-clock` (`harness-error`): `guest clock 1761888157 is 31536001 s from the host's 1793424158 (allowed 120 s)` |
| Command line not stored (arguments dropped) | `FAIL args-clock` (timeout): the program printed `argc=1 argv=tern-test` |

Other checks: `cargo clippy` (kernel, `zoo-user`, `xtask`, `zoo-abi`):
0 warnings (one unnecessary cast fixed). `cargo test -p zoo-infer -p
zoo-abi -p xtask --locked`: all passed. Not run: a clean-copy verify;
real hardware (the RTC is QEMU's).

## Desktop step 3 results: a dock and close buttons (version 0.23.0)

Design: docs/d3-design.md (§4 as implemented).

**Full `cargo xtask verify`** (TCG): exit 0 in 15 min 6 s, every case as
expected (`assistant-live` `SKIP`); existing desktop, mouse, redraw and
assistant checks unchanged. New case `dock` (13.4 s):

| Screenshot | Check |
| --- | --- |
| 1-dock | pointer at the bottom: tiles `Files`, `Crash Test`, `Assistant` at y 736, three running dots |
| 2-closed | after Crash Test's close button: its window and text gone, `Files` still focused, dock hidden; serial `PROC exit pid=29 name=crash-test code=0` (no restart) |
| 3-dock-closed | no running dot under Crash Test; dots under Files and the Assistant |
| 4-reopened | after its tile: `PROC start pid=31 name=crash-test`, window back and focused, dock hidden |

Then `--accel whpx --case assistant-live`: pass in 61.9 s.

**Negative control** (temporary edit to PeacockHandler, reverted):

| Change | Result |
| --- | --- |
| Close restarts the app instead of closing it | `FAIL dock` (timeout, 60 s): the app never closed, so its dock tile raised it instead of opening it and `opened Crash Test` never appeared |

Other checks: `cargo clippy` (`zoo-user`, `xtask`): 0 warnings. `cargo
test -p xtask --locked`: all passed. Not run: a clean-copy verify; real
hardware.

## Desktop step 2 results: redraw only what changed (version 0.22.0)

Design: docs/d2-design.md (§5 as implemented, with the measurements).
PeacockHandler damage rectangles, clipped drawing, partial present; test
keys F9/F10/F11.

**Full `cargo xtask verify`** (TCG): exit 0 in 10 min 48 s, every case as
expected (`assistant-live` `SKIP`). The existing desktop, mouse and
assistant screen checks pass unchanged. New cases:

| Case | Result |
| --- | --- |
| redraw | After a click, a drag of Files to (124, 98), Tab and eight pointer moves, the screen equals a forced full repaint: 993,280 pixels compared (menu bar excluded), 0 differ |
| frames | Both timing reports logged (cycle counts under TCG are not meaningful) |

Then `--accel whpx --case assistant-live`: pass in 51.0 s.

**Measurement** (`cargo xtask verify --skip-model --accel whpx --case
frames`, three runs): median pointer-move frame 4,342,631 / 4,039,118 /
4,176,248 cycles with full repaints (1,024,000 px) versus 31,610 / 31,158 /
31,718 cycles with partial redraws (408 px). Details and limits:
docs/d2-design.md §5.

**Negative control** (temporary edit to PeacockHandler, reverted):

| Change | Result |
| --- | --- |
| Pointer's old box not damaged | `FAIL redraw`: `405 of 993280 pixels differ from the partial redraws, first at (400, 110)` |

The first version of `redraw` did not catch this control (it passed): the
focus change after the pointer moves caused a full repaint that cleaned up
the stale arrows. The case now moves the pointer last; the control was
rerun and failed as above.

Other checks: `cargo clippy` (`zoo-user`, `xtask`): 0 warnings. `cargo
test -p xtask --locked`: all passed. Not run: a clean-copy verify; real
hardware.

## Desktop step 1 results: a mouse (version 0.21.0)

Design: docs/d1-design.md (§4 as implemented). PS/2 mouse on IRQ12, call 56
`mouse_read` (ABI version 15); PeacockHandler pointer, click to focus,
title-bar drag. Every boot logs `ZOO_OS INFO mouse ps2=true`.

**Full `cargo xtask verify`** (TCG): exit 0 in 12 min 49 s, every case as
expected (`assistant-live` `SKIP`), including the new case:

| Case | Time | Screen checks |
| --- | --- | --- |
| mouse | 10.4 s | `1-pointer`: arrow at (640, 400), `Files` focused. `2-click`: after moving to (840, 200) and one click, `Crash Test` focused (title and menu bar), `Files` not. `3-drag`: the Assistant title bar dragged by (-300, -100); its title found at (360, 316) instead of (660, 416), log `moved Assistant to (352, 312)` |

Then `--accel whpx --case assistant-live`: pass in 61.5 s.

**Negative control** (temporary edit to `kernel/src/mouse.rs`, reverted):

| Change | Result |
| --- | --- |
| Packet sign bits ignored (negative movement becomes large positive) | `FAIL mouse` (timeout, 60 s): the click never landed on Crash Test, so `click focuses Crash Test` never appeared |

Other checks: `cargo clippy` (kernel, `zoo-user`, `xtask`, `zoo-abi`,
`zoo-infer`): 0 warnings. `cargo test -p zoo-infer -p zoo-abi -p xtask
--locked`: all passed. Not run: a clean-copy verify; a real mouse on real
hardware (QEMU only).

## Services step 5 results: files survive a reboot (version 0.20.0)

Design: docs/s5-design.md (§5 as implemented). Kernel virtio block driver
(`kernel/src/virtio_blk.rs`, legacy interface, polled) and calls 52–55 (ABI
version 14); a 16 MiB data disk on every QEMU run; BadgerHandler A/B
snapshots with generation and CRC-32; shell `dir`/`cat`/`put`.

**Full `cargo xtask verify`** (TCG): exit 0 in 805 s, every case as
expected (`assistant-live` `SKIP`), with the three new boots of one disk:

| Case | Disk | Time | What it shows |
| --- | --- | --- | --- |
| persist-1 | fresh | 14.2 s | `put` twice → `saved generation 1 to slot A`, `saved generation 2 to slot B`; `cat` shows `second version` |
| persist-2 | same (reboot) | 10.8 s | `loaded generation 2 from slot B (457 bytes)`; `cat` shows `second version`; `dir Documents` lists `kept.txt (15 bytes)` |
| persist-3 | same, generation 2 damaged by xtask (`flipped byte 8392724`) | 17.0 s | `slot B: generation 2 failed its check, ignored`, `loaded generation 1 from slot A`; then a save cut short (`stopping after the payload of generation 2 (simulated power loss)`, exit 41), the supervisor restarts BadgerHandler and generation 1 loads again |

Then `--accel whpx --case assistant-live`: pass in 58.3 s. Every boot
reports `ZOO_OS INFO disk virtio-blk bytes=16777216`.

**Negative controls** (each temporary edit to BadgerHandler reverted):

| Change | Result |
| --- | --- |
| CRC not checked on load | `persist-1`, `persist-2` OK; `FAIL persist-3`: the damaged generation 2 was loaded (`loaded generation 2 from slot B`, then `snapshot did not parse; using the seed`) |
| First valid slot taken instead of the newest | `FAIL persist-2`: `loaded generation 1 from slot A` |

The planned control "header before payload" was not run: with the CRC it
cannot fail (docs/s5-design.md §5 explains why).

Other checks: `cargo clippy`: 0 warnings (one `too_many_arguments` allowed
on `run_case`). `cargo test -p zoo-infer -p zoo-abi -p xtask --locked`: all
passed. Not run: a clean-copy verify.

## Services step 4b results: the filesystem as a service (version 0.19.0)

Design: docs/s4-design.md (§6 as implemented). BadgerHandler holds the file
tree; folder handles are channels to it (`user/src/fsproto.rs`); the
kernel's filesystem and calls 19–25, 41–45 are gone (ABI version 13);
handle and `wait_any` limits 64; `restart <service>`.

**Full `cargo xtask verify`** (TCG): exit 0 in 679 s, every case as expected
(`assistant-live` `SKIP`; `services` OK in 26.1 s). Then `--accel whpx
--case assistant-live`: pass in 59.6 s. Every earlier file test passed
unchanged on BadgerHandler:

    ZOO_OS PROC start pid=8 name=badger-handler
    [9:arcticfox-test] ArcticFoxTest: all file checks passed
    [1:zookeeper] OtterHandler answered 19 scripted requests exactly
    ZOO_OS PROC exit pid=8 name=badger-handler code=0
    ...
    ZOO_OS PROC start pid=23 name=badger-handler        (resident, supervised)

and the `desktop`, `assistant` and `assistant-model` cases (Files, the
Assistant's broker) passed with the desktop's home folder now coming from
the supervisor. Zookeeper also checks that the old `fs_root` call returns
`NOSYS`. From the `services` case:

    ZOO_OS PROC killed pid=23 name=badger-handler by=22
    [22:zookeeper] Zookeeper: badger-handler killed; restarting (1 of 3)
    ZOO_OS PROC start pid=30 name=badger-handler
    [22:zookeeper] Zookeeper: filesystem root from badger-handler (pid 30)
    badger-handler pid=30 running restarts=1 last=killed

**Negative controls** (normal case; each temporary edit to BadgerHandler
reverted):

| Change | Result |
| --- | --- |
| Rights not checked | `FAIL normal`: `zookeeper code=26`, `create via read-only handle: expected RIGHTS, got Ok(())` |
| `..` accepted as a path component | `FAIL normal`: `zookeeper code=26`, `dir_open ../bin: expected INVAL, got Err(NotFound)` |

Not automated: a client holding a folder handle while BadgerHandler dies.
Other checks: `cargo clippy`: 0 warnings. `cargo test -p zoo-infer -p
zoo-abi -p xtask --locked`: all passed. Not run: a clean-copy verify.

## Services step 4a results: wait_any and faster hand-offs (version 0.18.0)

Design: docs/s4a-design.md. Call 51 `wait_any` (ABI version 12); Zookeeper
and BeaverHandler wait on channels instead of sleeping; the scheduler
prefers a ready user task over the idling kernel task on voluntary
switches; harness `WaitFor` accepts lines after shell prompts; the shell
waits for a desktop session's apps before reprinting its prompt.

**Measured** (seal-test, 50 ping round trips to BeaverHandler over a
channel it got by name, `services` case, TCG, this host):

| Version | 50 round trips |
| --- | --- |
| Before (sleep polling) | 250 ticks |
| `wait_any` only | 50 ticks |
| `wait_any` + scheduler preference | 0 ticks (in three runs) |

**Full `cargo xtask verify`** (TCG): exit 0 in 634 s, every case as
expected (`assistant-live` `SKIP`). Then `--accel whpx --case
assistant-live`: pass in 53.7 s. The first full run of this step failed one
check: the `desktop` case's last screenshot had an app's exit line after
the shell prompt (`zoo> ZOO_OS PROC exit pid=33 name=assistant code=0`),
because the faster scheduling let the prompt print before the session's
apps had exited. Fixed in the shell (§4 of the design); the rerun above
passed.

Also found while doing this step: with the scheduler change the `services`
case stalled because a kernel line shared a line with the prompt and the
harness's `WaitFor` needed whole-line equality. Diagnosed with temporary
scheduler logging (removed) that showed the kernel idle and every service
blocked, i.e. no starvation.

**Negative control:** `wait_any` not treating a closed peer as ready:
`FAIL normal`, `ptarmigan-test code=14`, `closed peer is ready`.

Other checks: `cargo clippy`: 0 warnings. `cargo test -p zoo-infer -p
zoo-abi -p xtask --locked`: all passed (new `find_line` test). Not run: a
clean-copy verify.

## Services step 3 results: handles in messages (version 0.17.0)

Design: docs/s3-design.md. Calls 49 `send_handle` and 50 `recv_handle`,
error 19 `WRONGKIND` (ABI version 11), Zookeeper's `connect <name>`,
BeaverHandler serving clients, seal-test's `connect`, and the boot test
`ptarmigan-test` (pid 20; later process ids moved up by one).

**Full `cargo xtask verify`** (TCG): exit 0 in 625 s, every case as
expected (`assistant-live` `SKIP`; `services` OK in 22.3 s). Then
`--accel whpx --case assistant-live`: pass in 54.6 s.

    [20:ptarmigan-test] PtarmiganTest: all handle-passing checks passed
    ...
    zoo> poke seal-test connect
    [21:zookeeper] Zookeeper: connected seal-test to beaver-handler
    [23:seal-test] seal-test: connected to beaver-handler by name; it returned 2 lines

ptarmigan-test checks: a moved handle is gone from the sender and works in
the receiver with its tag; `WRONGKIND` in both directions with the message
kept; an end cannot be sent through itself; a channel end queued towards a
closed end is released (its peer sees `PEERCLOSED`).

**Negative controls** (normal case; each temporary edit reverted):

| Change | Result |
| --- | --- |
| `send_handle` copies instead of moving | `FAIL normal`: `ptarmigan-test code=10`, `moved handle gone from sender` |
| Queued handles not released when an end closes | `FAIL normal`: `ptarmigan-test code=13`, `queued handle released` |

Other checks: `cargo clippy`: 0 warnings (one collapsed `match` arm in
seal-test). `cargo test -p zoo-infer -p zoo-abi -p xtask --locked`: all
passed. Not run: a clean-copy verify.

## Services step 2 results: log routing (version 0.16.0)

Design: docs/s2-design.md. Kernel log buffer (`kernel/src/klog.rs`), calls
47 `log_attach` and 48 `log_read` (ABI version 10), BeaverHandler draining
the buffer, `logs [text]`.

**Full `cargo xtask verify`** (TCG): exit 0 in 561 s, every case as
expected (`assistant-live` `SKIP`; `services` OK in 20.1 s). Then
`--accel whpx --case assistant-live`: pass in 48.6 s. (In this run
`negative-timeout` again timed out waiting for `NEVER-EMITTED`; in the S1
run boot was slow enough that it timed out on an earlier marker. Either way
the timeout is rejected.)

From the `services` case:

    ZOO_OS SHELL line=logs puffin
    [    14.24] [26:puffin-test] PuffinTest: hello from C
    [    14.24] [26:puffin-test] PuffinTest: crc32 0x414fa339, sqrt(2) ok, 64 KiB heap crc32 0x85400450
    [    14.24] [26:puffin-test] PuffinTest: all C checks passed

and plain `logs` begins with `(32 earlier matching lines not shown)` followed
by boot-time lines from Zookeeper, showing output from every program.

**Negative control:** with `klog::push` removed, `FAIL services …
got=timeout`: the supervisor's events never reach BeaverHandler, so the
`logs` check before `run puffin-test` never matches.

Other checks: `cargo clippy` (kernel, `zoo-user`, `xtask`, `zoo-abi`,
`zoo-infer`): 0 warnings. `cargo test -p zoo-infer -p zoo-abi -p xtask
--locked`: all passed. Not run: a clean-copy verify.

## Services step 1 results: resident supervisor (version 0.15.0)

Design: docs/s1-design.md. Zookeeper's supervise mode, BeaverHandler (log
service), seal-test (test service), `etc/services`, shell commands
`services`/`logs`/`poke`, call 46 `process_id` (ABI version 9), and the
harness action `WaitContains`. Every boot now starts the resident
Zookeeper (pid 20), BeaverHandler (21) and seal-test (22) before `READY`;
later process ids moved up by 3.

**Full `cargo xtask verify`** (TCG): exit 0 in 813 s, every case as expected
(`assistant-live` `SKIP`; new `services` case OK in 17.7 s). Then
`--accel whpx --case assistant-live`: pass in 63.7 s. The
`negative-timeout` case now times out while waiting for
`PROC start pid=17 name=wolverine-test` (boot takes longer than its 5 s
limit), so it still demonstrates a rejected timeout, just earlier.

From the `services` case (shell output and kernel markers):

    beaver-handler pid=21 running restarts=0 last=none
    seal-test pid=22 running restarts=0 last=none
    ZOO_OS PROC crash pid=22 name=seal-test fault=page-fault access=read cause=protection addr=0xffffc00000001000
    ZOO_OS PROC start pid=23 name=seal-test
    seal-test pid=23 running restarts=1 last=crashed
    ZOO_OS PROC killed pid=23 name=seal-test by=20
    ZOO_OS PROC start pid=24 name=seal-test
    seal-test pid=24 running restarts=2 last=unresponsive
    ...
    seal-test pid=25 failed restarts=3 last=crashed
    [     7.41] seal-test unresponsive; restarting (2 of 3)
    [    10.20] seal-test crashed; restart limit reached, left stopped

The hang was poked at about 3.8 s and the kill came at 7.41 s: within one
ping interval plus the 2 s answer limit.

**Negative controls** (`services` case; each temporary edit reverted):

| Change | Result |
| --- | --- |
| Zookeeper never sends health pings | `FAIL services … got=timeout 90.0s`: no `PROC killed pid=23` (the hang is never noticed) |
| Restart limit off by one | `FAIL services`: `got ZOO_OS PROC start pid=26 name=seal-test while expecting ZOO_OS SHELL line=services` |

Other checks: `cargo clippy` (kernel, `zoo-user`, `xtask`, `zoo-abi`,
`zoo-infer`): 0 warnings. `cargo test -p zoo-infer -p zoo-abi -p xtask
--locked`: all passed. Not run: a clean-copy verify.

## Program names (version 0.14.0)

Naming convention set by the owner (DECISIONS.md, "Program names"): only
system services keep `…Handler`; apps get plain names; boot-time tests are
Arctic animals with a `-test` suffix. Behaviour is unchanged.

| Old | New |
| --- | --- |
| lion-handler | beluga-test |
| monkey-handler | lemming-test |
| hamster-handler | muskox-test |
| beaver-handler | narwhal-test |
| squirrel-handler | arcticfox-test |
| ant-handler | caribou-test |
| mole-handler | walrus-test |
| gecko-handler | wolverine-test |
| capybara-handler | puffin-test |
| owl-handler | files |
| parrot-handler | assistant |
| chimp-handler | crash-test (window title "Crash Test") |

**Full `cargo xtask verify`** (TCG): exit 0 in 717 s, every case as
expected (`assistant-live` `SKIP`), with the new names in every marker and
screen check, e.g.:

    screen 1-desktop: … selected `bin/zookeeper` at (32, 80), Crash Test window shown; wrong text rejected
    screen 2-details: details of the 3rd file: `Name: bin/lemming-test` at (32, 80), …
    screen 3-app-restart: status `Crash Test crashed; restarted (1 of 3)` at (8, 780), focused `Crash Test` at (660, 52), …

Then `--accel whpx --case assistant-live`: pass in 61.7 s.

## POSIX step 5a results: C toolchain (version 0.14.0)

Design: docs/posix5-design.md. Zig 0.16.0 downloaded into `tools/zig/`
with the owner's approval: `zig-x86_64-windows-0.16.0.zip`, 97,217,739
bytes, SHA-256 `68659eb5…a75a7e`, identical to ziglang.org's index
(tools/zig/README.md); `zig version` 0.16.0, `zig cc --version` clang
21.1.0. The initrd now holds 16 programs, one of them in C (610,816 bytes).

**Full `cargo xtask verify`** (TCG) in the main checkout: exit 0 in 812 s,
`ZOO_OS BOOT version=0.14.0`, every case as expected (`assistant-live`
`SKIP`). This run shared the CPU with an interactive demo session, so its
times are longer than usual (normal 6.7 s, desktop 16.5 s, inference
624.9 s). New case `c-program`: OK in 7.7 s. Then `--accel whpx --case
assistant-live`: pass in 53.5 s.

From the `c-program` case's serial log:

    ZOO_OS SHELL line=run capybara-handler
    ZOO_OS PROC start pid=20 name=capybara-handler
    [20:capybara-handler] CapybaraHandler: hello from C
    [20:capybara-handler] CapybaraHandler: crc32 0x414fa339, sqrt(2) ok, 64 KiB heap crc32 0x85400450
    [20:capybara-handler] CapybaraHandler: all C checks passed
    ZOO_OS PROC exit pid=20 name=capybara-handler code=0

**Negative and absence checks** (each reverted):

| Change | Result |
| --- | --- |
| CapybaraHandler expects a wrong CRC-32 value | `FAIL c-program`: `capybara-handler code=11` |
| Toolchain folder moved aside | `[build] no C toolchain in tools/zig/ …; C programs skipped`, initrd with 15 programs, `SKIP c-program not run: no C toolchain` |

**Interactive demo** (not a verify case, recorded as an observation): the
model image under `--accel whpx` in a visible window, driven by keystrokes
over QMP: boot checks, shell commands, the C program, the desktop, and the
Assistant answering "Find my pancake recipe" (`search pancake recipe`) and
"Make a folder called Projects/2026" (`create Projects/2026`, created after
`y`, then `undo`).

Other checks: `cargo clippy -p xtask --all-targets`: 0 warnings. `cargo
test -p zoo-infer -p zoo-abi -p xtask --locked`: all passed. Not run: a
clean-copy verify.

## POSIX step 4 results: writable files (version 0.13.0)

Design: docs/posix4-design.md. Calls 41–45 and the WRITE right (ABI
version 8), page-backed file contents, and the test program
SquirrelHandler, which Zookeeper runs during the directory checks (pid 8;
every later process id moved up by one).

**Full `cargo xtask verify`** (TCG) in the main checkout: exit 0 in 534 s,
`ZOO_OS BOOT version=0.13.0`; every case as expected (normal 5.0 s,
desktop 13.9 s, assistant 23.0 s, assistant-model 27.4 s, inference
398.4 s; `assistant-live` `SKIP`). Then `--accel whpx --case
assistant-live`: pass in 59.6 s.

From the normal case's serial log:

    ZOO_OS PROC start pid=8 name=squirrel-handler
    [8:squirrel-handler] SquirrelHandler: all file checks passed
    ZOO_OS PROC exit pid=8 name=squirrel-handler code=0
    ...
    ZOO_OS INFO processes done frames_free=520916 file_pages=1
    ZOO_OS TEST process-cleanup ok

`file_pages=1` is `Documents/notes.txt`, page-backed after SquirrelHandler
wrote to it and restored its contents; the OtterHandler and Assistant
searches that rely on its text still pass.

**A gap found and closed:** counting file pages as free in the leak check
would have hidden a file-removal leak. The check now also audits that the
file-page counter equals the pages files hold.

**Negative controls** (normal case; each temporary edit reverted):

| Change | Result |
| --- | --- |
| `file_remove` does not free the file's pages | `FAIL normal`: `PANIC self-test process-cleanup: files hold 1 pages but 27 are counted` |
| Shrinking does not zero the rest of the last page | `FAIL normal`: `squirrel-handler code=43`, `check 43 failed: regrown tail is zero` |

Other checks: `cargo clippy` (kernel, `zoo-user`, `xtask`, `zoo-abi`,
`zoo-infer`): 0 warnings (one method renamed for a naming lint). `cargo
test -p zoo-infer -p zoo-abi -p xtask --locked`: all passed. Not run: a
clean-copy verify.

## POSIX step 3 results: exceptions (version 0.12.0)

Design: docs/posix3-design.md. Calls 38–40 (ABI version 7),
`zoo_rt::exception`, and the test program GeckoHandler, run by Zookeeper
at boot (pid 15, children 16–18; desktop and model cases' process ids
moved up by 4).

**Full `cargo xtask verify`** (TCG) in the main checkout: exit 0 in 626 s,
`ZOO_OS BOOT version=0.12.0`; every case as expected (normal 5.5 s,
desktop 14.1 s, assistant 23.2 s, assistant-model 31.7 s, inference
473.6 s; `assistant-live` `SKIP`). Then `--accel whpx --case
assistant-live`: pass in 60.4 s.

The normal case requires, in order:

    ZOO_OS PROC start pid=15 name=gecko-handler
    ZOO_OS PROC start pid=16 name=gecko-handler
    ZOO_OS PROC crash pid=16 name=gecko-handler fault=page-fault access=write cause=not-present addr=0x0000000000000010
    ZOO_OS PROC start pid=17 name=gecko-handler
    ZOO_OS PROC crash pid=17 name=gecko-handler fault=page-fault access=read cause=not-present addr=0x0000000000000020
    ZOO_OS PROC start pid=18 name=gecko-handler
    ZOO_OS PROC exit pid=18 name=gecko-handler code=103
    ZOO_OS PROC exit pid=15 name=gecko-handler code=0

Pid 16's fault was declared unhandled; pid 17's crash is the fault raised
inside its handler (address 0x20, not the original 0x10); pid 18's handler
returned a kernel-half `rip`, which the kernel refused. The kernel logged 33
deliveries to pid 15: 16 demand-commit faults on the main thread, one
divide error, and 16 on the second thread. GeckoHandler logs
`all exception checks passed`; `TEST process-cleanup ok` holds.

**Negative controls** (normal case; each temporary edit reverted):

| Change | Result |
| --- | --- |
| `exception_return` does not reload the FPU/SSE state | `FAIL normal`: `gecko-handler code=22`, `check 22 failed: SSE register restored` |
| No guard against faults inside a handler | first try: **passed** (the re-delivered contexts stacked downwards until the thread's stack ran out, which also ended in a crash). The child now uses a fixed exception stack; rerun: `FAIL normal … got=timeout 30.0s`, 10,583 deliveries to pid 17 |

Other checks: `cargo clippy` (kernel, `zoo-user`, `xtask`, `zoo-abi`,
`zoo-infer`): 0 warnings. `cargo test -p zoo-infer -p zoo-abi -p xtask
--locked`: all passed. Not run: a clean-copy verify.

## POSIX step 2 results: address space (version 0.11.0)

Design: docs/posix2-design.md. Calls 32–37 (ABI version 6), `zoo_rt::vm`,
and the test program MoleHandler, which Zookeeper runs at boot (pid 10,
with four child processes 11–14 that must crash; desktop and model cases'
process ids moved up by 5).

**Full `cargo xtask verify`** (TCG) in the main checkout: exit 0 in 588 s,
`ZOO_OS BOOT version=0.11.0`; every case as expected (normal 5.4 s,
desktop 13.9 s, assistant 23.5 s, assistant-model 31.3 s, inference
440.5 s; `assistant-live` `SKIP`). Then `--accel whpx --case
assistant-live`: pass in 61.3 s.

The normal case requires these kernel lines in order (the crash lines are
matched exactly, including access type and cause):

    ZOO_OS PROC start pid=10 name=mole-handler
    ZOO_OS PROC start pid=11 name=mole-handler
    ZOO_OS PROC crash pid=11 name=mole-handler fault=page-fault access=read cause=not-present addr=0x0000000040000000
    ZOO_OS PROC start pid=12 name=mole-handler
    ZOO_OS PROC crash pid=12 name=mole-handler fault=page-fault access=write cause=protection addr=0x0000000040000000
    ZOO_OS PROC start pid=13 name=mole-handler
    ZOO_OS PROC crash pid=13 name=mole-handler fault=page-fault access=execute cause=protection addr=0x0000000040000000
    ZOO_OS PROC start pid=14 name=mole-handler
    ZOO_OS PROC crash pid=14 name=mole-handler fault=page-fault access=write cause=not-present addr=0x0000000040000000
    ZOO_OS PROC exit pid=10 name=mole-handler code=0

and MoleHandler logs `all address-space checks passed` (its in-process
checks are listed in the design, §5). `TEST process-cleanup ok` holds.

**Negative controls** (normal case; each temporary edit reverted):

| Change | Result |
| --- | --- |
| `vm_commit`/`vm_protect` never set the execute permission | `FAIL normal`: `PROC crash pid=10 name=mole-handler fault=page-fault access=execute cause=protection addr=0x0000000040200000` (the generated-code check) |
| `vm_decommit`/`vm_release` free no pages | `FAIL normal`: `mole-handler code=16`, `check 16 failed: decommitted pages reserved again` |

Other checks: `cargo clippy` (kernel, `zoo-user`, `xtask`, `zoo-abi`,
`zoo-infer`): 0 warnings after replacing six `% PAGE != 0` with
`is_multiple_of`. `cargo test -p zoo-infer -p zoo-abi -p xtask --locked`:
all passed. Not run: a clean-copy verify.

## POSIX step 1 results: threads (version 0.10.0)

Design: docs/posix1-design.md. Process/thread split in the kernel, calls
27–31 (ABI version 5), `zoo_rt::thread`, and the test program AntHandler,
which Zookeeper runs at every boot (pid 9; desktop and model cases' process
ids moved up by one).

**Full `cargo xtask verify`** (TCG) in the main checkout: exit 0 in 501 s,
`ZOO_OS BOOT version=0.10.0`; every case as expected (normal 5.3 s,
desktop 14.0 s, assistant 23.2 s, assistant-model 27.5 s, inference
368.8 s; `assistant-live` reported `SKIP` without an accelerator). Then
`--accel whpx --case assistant-live`: pass in 49.1 s.

From the normal case's serial log:

    ZOO_OS PROC start pid=9 name=ant-handler
    ZOO_OS INFO thread start pid=9 tid=65536
    ...
    [9:ant-handler] AntHandler: 4 threads x 20000 rounds: counter 80000, lock waited 3 times, 4 handovers, TLS errors 0, main TLS 100, 34 ticks
    [9:ant-handler] AntHandler: all thread checks passed; exiting with a thread still running
    ZOO_OS PROC exit pid=9 name=ant-handler code=0
    ZOO_OS TEST process-cleanup ok

The last two lines show that a process exiting with a thread still running
ends cleanly with no leaked frames.

**Negative controls** (normal case; each temporary edit reverted):

| Change | Result |
| --- | --- |
| Kernel does not load the thread's FS base on a switch | `FAIL normal … ant-handler code=6`; `TLS errors 123, main TLS 3` |
| `futex_wake` wakes nobody | `FAIL normal … ant-handler code=10`; `futex_wake woke 0 (expected 1)` |

The first version of the futex check **passed** with `futex_wake` disabled:
the kernel's general wake-ups restart blocked calls, and the waiters then
saw the lock word changed. The check was replaced by one where only
`futex_wake` can release the waiter (value never changes, exactly one waiter
reported), and the kernel was fixed to count waiters that a general wake-up
had already made runnable.

Two findings while building the test:

- With a busy loop inside the locked region and no sleeping, the lock was
  never found taken under TCG (0 waits in two runs, although threads did
  interleave). The test now sleeps while holding the lock every 5,000
  rounds, so contention is deterministic; why timer preemption never landed
  inside the busy loop was not investigated further.
- Thread ids first came from the process-id counter, which shifted every
  later process id by the number of threads (the desktop and model cases
  failed). Extra threads now use ids from 65536.

Other checks: `cargo clippy` (kernel, `zoo-user`, `xtask`, `zoo-abi`,
`zoo-infer`): 0 warnings. `cargo test -p zoo-infer -p zoo-abi -p xtask
--locked`: all passed. Not run: a clean-copy verify.

## M5c-2 results (version 0.9.0)

Design: docs/m5c2-design.md. New: ParrotHandler (the Assistant window),
ElephantHandler's service mode, guided tool choice (`infer/src/assist.rs`),
three verify cases and `cargo xtask assist-check`. The initrd holds 11
programs and 4 seed files (473,088 bytes).

**Full `cargo xtask verify`** (TCG) in the main checkout: exit 0 in 485 s,
`ZOO_OS BOOT version=0.9.0`.

| Case | Result | Time | Notes |
| --- | --- | --- | --- |
| normal, panic, 3 fault modes, keyboard, scroll | OK | 2.7–6.7 s | unchanged |
| desktop | OK | 12.9 s | three windows now; process ids for the restarts moved (Parrot is one more app) |
| assistant | OK | 22.2 s | typed commands, plan + `y`, undo, not found, escape refused, "model is not installed" |
| assistant-model | OK | 25.3 s | see below |
| assistant-live | **SKIP** | — | `not run: needs a hardware accelerator (--accel whpx or kvm)` |
| inference | OK | 356.7 s | unchanged |
| 3 negative cases | rejected as intended | 1.8–5.0 s | unchanged |

Screen checks (exact text and colours, with a wrong string that must not
match):

    screen 1-plan: `Assistant` focused at (660, 416); search result at (660, 504); plan shown and confirmation asked at (660, 444); nothing created yet
    screen 2-transcript: created at (660, 544), undone at (660, 584), then not found; escape refused at (660, 684); missing model reported at (660, 724); wrong text rejected
    screen 1-busy: while the model works (status `Thinking:` at (660, 444)), Files took focus at (32, 52) and its selection moved to the 3rd entry at (32, 120)
    screen 2-cancelled: `Cancelled.` at (660, 504), ready again at (660, 444), no longer thinking

**Cancellation under TCG** (assistant-model serial log). The request was
cancelled while the system prompt was being computed; the service noticed
at its next token, 756 ticks (7.6 s) after Parrot sent `cancel`:

    [13:parrot-handler] ParrotHandler: cancel sent at tick 1038
    [14:elephant-handler] ElephantHandler: cancel seen at tick 1794, 1024 ticks into the request
    [14:elephant-handler] ElephantHandler: request cancelled

An earlier run of the same case gave 818 ticks. These are observations, not
performance claims.

**`--accel whpx --case assistant-live`** on the final code: exit 0 in 65 s
(case 47.0 s):

    [14:elephant-handler] ElephantHandler: system prompt 55 tokens in 1908 ticks
    [14:elephant-handler] ElephantHandler: `Find my pancake recipe`: margin 3, 10 request + 3 argument tokens, 2402 ticks
    [14:elephant-handler] ElephantHandler: chose search `pancake recipe`
    [14:elephant-handler] ElephantHandler: `Make a folder called Projects/2026`: margin 140, 15 request + 6 argument tokens, 762 ticks
    [14:elephant-handler] ElephantHandler: chose create `Projects/2026`
    screen 1-live: model chose `search pancake recipe` at (660, 504) (result shown) and `create Projects/2026` at (660, 564), created after `y` at (660, 644)

**`cargo xtask assist-check`** (host engine, same code): PASS, 8 of 8, in
39 s. The first request's tool margin is small (0.04 on the host, 3/100 in
the guest), so that choice is the least certain one.

| Request | Choice | Margin |
| --- | --- | --- |
| Find my pancake recipe | search `pancake recipe` | 0.04 |
| Search for files that mention flour | search `flour` | 2.72 |
| What is in my Documents folder? | list `Documents` | 1.53 |
| Show me what's inside Music | list `Music` | 0.29 |
| How big is Documents/notes.txt? | inspect `Documents/notes.txt` | 0.32 |
| Make a folder called Projects/2026 | create `Projects/2026` | 1.40 |
| Create a new folder named Invoices | create `Invoices` | 4.14 |
| Please create the folder Photos/Holidays | create `Photos/Holidays` | 5.37 |

**Negative controls:**

| Change | Result |
| --- | --- |
| ElephantHandler ignores `cancel` (temporary edit, reverted) | `FAIL assistant-model … got=timeout 300.0s`; the log shows `cancel sent` and no `request cancelled` |
| `assist-check` expecting `search "pancake"` (its first version) | `FAIL "Find my pancake recipe" -> search "pancake recipe" … expected search "pancake"`; exit 1. The expectation now accepts both, and OtterHandler's search matches all words against paths, so both find the recipe |

Other checks: `cargo clippy` (kernel, `zoo-user`, `xtask`, `zoo-abi`,
`zoo-infer`): 0 warnings. `cargo test -p zoo-infer -p zoo-abi -p xtask
--locked`: all passed (2 new tests for `assist`). Not run: a clean-copy
verify.

## M5c-1 results (version 0.8.0)

Design: docs/m5c1-design.md. In-memory filesystem (`kernel/src/fs.rs`),
directory capabilities and system calls 19–26 (ABI version 4), and the
action broker OtterHandler. The initrd now holds 10 programs and 4 seed
files (411,136 bytes); the kernel logs `ZOO_OS INFO fs seeded files=14
nodes=22`.

**Full `cargo xtask verify`** (TCG) in the main checkout: exit 0 in 442 s, all
12 cases as expected (normal 4.6 s, desktop 13.5 s, inference 356.0 s),
`ZOO_OS BOOT version=0.8.0`. The normal case now also requires
`PROC start pid=8 name=otter-handler` and its exit with code 0; the desktop
and inference cases' process ids moved up by 1. From the normal case's
serial log:

    [1:zookeeper] directory capabilities enforced (paths stay inside their handle)
    ZOO_OS PROC start pid=8 name=otter-handler
    [1:zookeeper] broker `search pancake` -> ok
    [1:zookeeper] broker `search FLOUR` -> ok
    [1:zookeeper] broker `list` -> ok
    [1:zookeeper] broker `inspect Documents` -> ok
    [1:zookeeper] broker `plan Projects/2026` -> ok
    [1:zookeeper] broker `inspect Projects` -> err not found
    [1:zookeeper] broker `create Projects/2026` -> ok
    [1:zookeeper] broker `list Projects` -> ok
    [1:zookeeper] broker `undo` -> ok
    [1:zookeeper] broker `inspect Projects` -> err not found
    [1:zookeeper] broker `undo` -> err nothing to undo
    [1:zookeeper] broker `create Documents` -> err already exists
    [1:zookeeper] broker `list ../bin` -> err path escapes home
    [1:zookeeper] broker `inspect /bin` -> err path escapes home
    [1:zookeeper] broker `delete Documents` -> err unknown tool
    [1:zookeeper] broker `create Projects/2027` -> ok
    [1:zookeeper] broker `undo` -> ok
    ZOO_OS PROC exit pid=8 name=otter-handler code=0
    [1:zookeeper] OtterHandler answered 17 scripted requests exactly

Each `->` line is logged only after every reply line of that request matched
the expected text exactly (`broker_step` in `user/src/bin/zookeeper.rs`).
`TEST process-cleanup ok` still holds.

**Negative controls** (normal case only, each reverted before the run above):

| Change | Result |
| --- | --- |
| Kernel path check accepts `..` (`valid_component` in `kernel/src/fs.rs`) | `FAIL normal … got ZOO_OS PROC exit pid=1 name=zookeeper code=26`; log: `dir_open ../bin: expected INVAL, got Err(NotFound)` |
| OtterHandler `undo` removes in creation order (parents first) | `FAIL normal … code=27`; log: ``broker `undo` reply 0: got `| not empty, left in place: Projects`, expected `| removed Projects/2026` `` |

Other checks: `cargo clippy` on the kernel, `zoo-user` (user target),
`xtask`, `zoo-abi` and `zoo-infer`: 0 warnings. `cargo test -p zoo-abi -p
xtask -p zoo-infer --locked`: all passed. Not run this step: a clean-copy
verify (the model image needs the 428 MB model copied in).

## M5b results (version 0.7.0)

Design: docs/m5b-design.md. New crate `zoo-infer` (GGUF reader, Q4_0/Q8_0,
Qwen2 forward pass, byte-level BPE tokenizer, ChatML); new program
ElephantHandler; new verify case `inference` using the model image. The
version number was not bumped: the kernel and the boot path are unchanged
from M5a.

Model: `tools/models/qwen2.5-0.5b-instruct-q4_0.gguf`, 428,730,208 bytes,
SHA-256 `7671c0c3…daf6ed` (full value and source in tools/models/README.md;
checked before every use).

**Independent reference** — `cargo xtask infer-check` (Rust engine vs
`tools/reference/qwen_ref.py`, NumPy 2.3.4, Python 3.12), exit 0 in 76 s:

| Prompt | Prompt tokens | Output tokens | Top-5 logit max diff |
| --- | --- | --- | --- |
| What is the capital of France? | 26 (same) | 7 (same) | 0.00006 |
| List three animals found in a zoo, one per line. | 31 (same) | 7 (same) | 0.00004 |
| Compute 12 + 30 and explain in one sentence. | 33 (same) | 11 (same) | 0.00007 |
| Translate 'café' to English; don't explain. | 31 (same) | 1 (same) | 0.00004 |
| Create a folder named Projects/2026 in my home directory. | 34 (same) | 24 (same) | 0.00005 |

"Same" means the token id sequences are identical; the threshold is 0.02.

**Full `cargo xtask verify`** on commit `0a96884` (TCG): exit 0 in 431 s, all
12 cases as expected (11 earlier cases unchanged, plus `inference` 350.5 s).
The host engine's answer for the guest prompt was
`The capital of France is Paris.` (ids 785, 6722, 315, 9625, 374, 12095, 13).
From the inference case's serial log:

    [8:elephant-handler] ElephantHandler: answer `The capital of France is Paris.`
    [8:elephant-handler] ElephantHandler: prefill 26 tokens in 26390 ticks, generated 7 tokens in 7632 ticks (100 ticks = 1 s)
    [8:elephant-handler] ElephantHandler: tokens match the host engine exactly
    ZOO_OS PROC exit pid=8 name=elephant-handler code=0

Timings are observations on this host, not performance claims:

| Where | Prompt (26 tokens) | Generation (7 tokens) | Case |
| --- | --- | --- | --- |
| Guest, TCG (run above) | 263.9 s | 76.3 s | 350.5 s |
| Guest, TCG (earlier run, same code) | 394.4 s | 104.2 s | 510.8 s |
| Guest, `--accel whpx --case inference` | 9.0 s | 2.4 s | 18.3 s |
| Host engine (release, one thread) | ≈ 5.9 tokens/s overall | | |

**Negative checks of the engine:** every truncation of a synthetic GGUF file
is rejected without a panic (unit test). ElephantHandler compares its ids with
`test/elephant.txt` and exits non-zero on a difference, and only exit 0
satisfies the marker; a deliberately wrong expectation was **not** run as a
separate case in this step. `--skip-model` reports the case as `SKIP`, never
`OK`.

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
