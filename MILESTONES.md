# Milestones and acceptance gates

Only M0 and M1 are authorized by the kickoff prompt. Subsequent stages are a
roadmap, not an instruction to keep implementing indefinitely. No time estimates
or application compatibility guarantees are implied.

## M0 — reproducible development foundation

Status: implemented and verified 2026-10-30.

Deliver a Cargo workspace, verified compiler/dependency pins, documented QEMU and
OVMF prerequisites, firmware configuration, ignore rules and doctor command.
Acceptance: doctor correctly distinguishes ready and missing-prerequisite states;
record tested host versions. No global installs or host configuration changes
without required permission. If prerequisites are blocked, report partial status.

## M1 — actual boot and diagnostics

Status: implemented and verified 2026-10-30.

Deliver a UEFI-bootable freestanding Rust kernel, serial output, panic handler,
and build/run/verify commands specified in BUILD.md.
Acceptance: normal QEMU run produces ordered BOOT and READY markers; deliberate
panic run produces ordered BOOT and PANIC markers; verifier rejects a missing
marker/timeout case; all subprocesses are cleaned up; independent logs and clean
checkout instructions exist. Review unsafe boundaries and actual boot evidence.
Exclude graphics, allocator, scheduler, filesystem, Windows apps, AI and phone.
Stop and report after this milestone.

## M2 — kernel foundations

Status: implemented and verified 2026-10-30 (docs/m2-design.md,
docs/verification.md).

Specify memory ownership, page mappings, allocator, interrupts, timer and exception
handling before implementing them. Acceptance includes allocation/free correctness,
protection faults, guard behavior and diagnostic handling without silent reboot.

## Interim — screen, keyboard and debug shell (pulled forward from M4)

Status: implemented and verified 2026-10-30 at the user's request, before
M3 (docs/console-design.md). A kernel-mode framebuffer text console, PS/2
keyboard and built-in diagnostic commands. It does not replace M4's
desktop, input routing or windowing.

## M3 — isolated native programs and services

Status: implemented and verified 2026-10-30 (docs/m3-design.md,
docs/verification.md). Zookeeper is the supervisor; the filesystem is a
read-only initrd so far.

Specify handles, syscall ABI, IPC, process lifecycle and executable loader.
Acceptance: two isolated processes communicate; invalid handles and malformed
requests fail safely; a crashing process does not terminate unrelated processes.
Begin an initial filesystem and supervisor with explicit failure behavior.

## M4 — native graphical desktop

Status: implemented and verified 2026-10-30 (docs/m4-design.md,
docs/verification.md). Keyboard-only; mouse and window management deferred.

Framebuffer first; input, focus, windows and a small native UI. Acceptance:
keyboard navigation, basic file browsing, stable redraw and crash recovery.
A static screenshot is not an interactive desktop acceptance test.

## Early feasibility tracks — begin after required infrastructure exists

Status: desk studies done 2026-10-30 (docs/feasibility/). Outcome: Wine
hosting, GPU and graphics translation are **BLOCKED**; continuing toward
Opera/WoW needs owner decisions on scope, host and reference hardware.

Study Wine host dependencies and real GPU driver portability before UI polish.
Use COMPATIBILITY.md for explicit outcomes. These studies may invalidate the
Opera/WoW plan and must be allowed to report that honestly.

## M5 — local assistant prototype

Status: prototype complete 2026-10-30. M5a (floating point, user heap, file
mapping), M5b (inference engine, docs/m5b-design.md), M5c-1 (filesystem,
directory capabilities, action broker, docs/m5c1-design.md) and M5c-2
(Assistant window, model service, guided tool choice, cancellation, budgets,
docs/m5c2-design.md). Limits: one command per request, no persistence,
model quality checked on 8 fixed requests only; speech not started.

Port a small runtime, benchmark a selected local model and implement permission-
checked file search/folder creation. Acceptance: real results, scoped operations,
path escape rejection, cancellation, bounded resources, useful failure reporting,
and responsive desktop while inference runs. Text first; speech later.

## POSIX layer (prerequisite for M6, owner decision 2026-10-30)

Strategy A in docs/feasibility/wine-hosting.md: grow ZooOS until Wine's Unix
side can be ported. Steps are taken one at a time, each with its own design
and verification:

| Step | Content | Status |
| --- | --- | --- |
| 1 | Threads, futexes, thread-local storage (docs/posix1-design.md) | done 2026-10-30 |
| 2 | Address space: reserve/commit/protect/decommit/release/query (docs/posix2-design.md) | done 2026-10-30 |
| 3 | User exception delivery to a handler, resume with edited registers (docs/posix3-design.md) | done 2026-10-30 |
| 4 | Writable files: create, write, truncate, remove, rename (docs/posix4-design.md) | done 2026-10-30 |
| 5a | C toolchain (Zig 0.16.0, approved download) and freestanding C programs (docs/posix5-design.md) | done 2026-10-30 |
| 5b-1 | Process arguments and wall-clock/monotonic time (docs/posix5b1-design.md) | done 2026-10-31 |
| 5b-2 | File descriptors: now part of the Linux personality (route 3) | superseded |
| 5b-3 | C library: route 3 chosen (Linux system calls; docs/feasibility/libc-route.md) | in progress |
| L1 | Linux system calls, step 1: a static musl "hello" runs unmodified (docs/linux1-design.md) | done 2026-10-31 |
| L2 | Memory (`brk`, `mmap`), read-only files, time (docs/linux2-design.md) | done 2026-10-31 |
| L3 | Folders, working directory, uname, environment (docs/linux3-design.md) | done 2026-10-31 |
| L4 | Writable files in the home folder, on the F1 fast path (docs/linux4-design.md) | done 2026-10-31 |
| L5 | Processes (`fork`/`vfork`/`execve`/`wait4`), pipes, `dup`, signals with default actions (docs/linux5-design.md) | done 2026-10-31 |
| F2 | Kernel output and memory reuse off the hot path: buffered serial, batched console drawing, warm frame reuse (docs/f2-design.md) | done 2026-10-31 |
| L6 | Signal handlers (delivery, `rt_sigreturn`, blocked signals) and threads (`clone` with `CLONE_THREAD`) | proposed |

## Services (owner direction 2026-10-30)

Re-balance toward system services before further POSIX work: a supervised,
lean set of daemons, as ARCHITECTURE.md intends.

| Step | Content | Status |
| --- | --- | --- |
| S1 | Resident supervisor (Zookeeper): service list, health pings, restart policy; BeaverHandler (logs); shell `services`/`logs` (docs/s1-design.md) | done 2026-10-30 |
| S2 | Every program's log output routed through BeaverHandler; `logs [text]` (docs/s2-design.md) | done 2026-10-30 |
| S3 | Handles passed in channel messages; services find each other by name through Zookeeper (docs/s3-design.md) | done 2026-10-30 |
| S4a | `wait_any`; services stop polling; scheduler prefers ready programs over idling (docs/s4a-design.md) | done 2026-10-30 |
| S4b | Filesystem moved out of the kernel into a service, BadgerHandler (docs/s4-design.md) | done 2026-10-30 |
| S5 | Disk driver (virtio) and crash-safe A/B snapshots: files survive a reboot (docs/s5-design.md) | done 2026-10-31 |

## Desktop steps (towards the owner's macOS-like goal)

| Step | Content | Status |
| --- | --- | --- |
| D1 | PS/2 mouse; pointer; click to focus; drag windows by the title bar (docs/d1-design.md) | done 2026-10-31 |
| D2 | Redraw only what changed; measured frame cost (docs/d2-design.md) | done 2026-10-31 |
| D3 | Close buttons; auto-hiding dock that reopens apps (docs/d3-design.md) | done 2026-10-31 |
| D4 | Notes: a text editor whose typing survives a reboot (docs/d4-design.md) | done 2026-10-31 |
| D5 | Date and time (UTC) in the menu bar (docs/d5-design.md) | done 2026-10-31 |
| D6 | The everyday (fast-boot) build starts in the desktop (docs/d6-design.md) | done 2026-10-31 |
| D7 | Files browses the home folder, previews text files (docs/d7-design.md) | done 2026-10-31 |
| D8 | Resize windows from the bottom-right corner (docs/d8-design.md) | done 2026-10-31 |
| D9 | Double-click a title bar to zoom and restore (docs/d9-design.md) | done 2026-10-31 |
| D10 | A cursor in Notes (arrow keys to apps) (docs/d10-design.md) | done 2026-10-31 |

## Real hardware (USB boot on the owner's other PCs; never the dev PC)

| Step | Content | Status |
| --- | --- | --- |
| H1a | USB host controller (xHCI): find, start, enumerate devices (docs/h1-design.md) | done 2026-10-31 |
| H1b | USB keyboard and mouse input (HID boot protocol) (docs/h1b-design.md) | done 2026-10-31 |
| H2a | USB stick image (`cargo xtask usb-image`), large screens, readiness notes and risks (docs/usb-boot.md) | done 2026-10-31 |
| H3 | Local APIC timer calibrated against the ACPI PM timer; PIT as fallback (docs/h3-design.md) | done 2026-10-31 |
| H4 | USB hubs (one tier) (docs/h4-design.md) | done 2026-10-31 |
| H5 | Key repeat for USB keyboards (docs/h5-design.md) | done 2026-10-31 |
| H2 | Boot a USB stick on one of the owner's other PCs (needs the owner: hardware details, a named USB device) | proposed |

## Speed (owner direction 2026-10-31: speed first)

| Step | Content | Status |
| --- | --- | --- |
| F1 | Shared-memory bulk file transfers; scheduler stall fixed (docs/f1-design.md) | done 2026-10-31 |
| F2 | Faster first-time writes (file growth); interrupt-driven input | proposed |

## Boot speed

| Step | Content | Status |
| --- | --- | --- |
| B1 | Fast boot: check programs only in test builds; boot timing in the harness (docs/b1-design.md) | done 2026-10-31 |

## M6 — Windows application stages

Minimal console, simple GUI, installer/multiprocess program, then selected Opera
workflow. Acceptance records exact versions, sandbox behavior and persistent state.
Do not call running a simpler browser equivalent to running Opera.

## M7 — Battle.net and WoW

Prerequisite: accelerated graphics on selected hardware. Verify installation,
authentication, download, patch, game launch, audio, input and online gameplay.
Record FPS/frame times, reference Windows baseline and stability across restarts.
No graphics acceleration or online-service bypasses count as success.

## M8 — reliable personal-use release

Installation and recovery, update failure tests, credential protection, diagnostics,
accessibility, hardware coverage and measured performance under combined loads.

## Later — iPhone link

Research current iOS capabilities, build explicit pairing and authorized transfer
first, then test lifecycle/revocation. Do not assume calls, messages, notification
mirroring or Apple Continuity parity are available.

