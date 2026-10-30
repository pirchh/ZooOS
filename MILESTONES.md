# Milestones and acceptance gates

Only M0 and M1 are authorized by the kickoff prompt. Subsequent stages are a
roadmap, not an instruction to keep implementing indefinitely. No time estimates
or application compatibility guarantees are implied.

## M0 — reproducible development foundation

Deliver a Cargo workspace, verified compiler/dependency pins, documented QEMU and
OVMF prerequisites, firmware configuration, ignore rules and doctor command.
Acceptance: doctor correctly distinguishes ready and missing-prerequisite states;
record tested host versions. No global installs or host configuration changes
without required permission. If prerequisites are blocked, report partial status.

## M1 — actual boot and diagnostics

Deliver a UEFI-bootable freestanding Rust kernel, serial output, panic handler,
and build/run/verify commands specified in BUILD.md.
Acceptance: normal QEMU run produces ordered BOOT and READY markers; deliberate
panic run produces ordered BOOT and PANIC markers; verifier rejects a missing
marker/timeout case; all subprocesses are cleaned up; independent logs and clean
checkout instructions exist. Review unsafe boundaries and actual boot evidence.
Exclude graphics, allocator, scheduler, filesystem, Windows apps, AI and phone.
Stop and report after this milestone.

## M2 — kernel foundations

Specify memory ownership, page mappings, allocator, interrupts, timer and exception
handling before implementing them. Acceptance includes allocation/free correctness,
protection faults, guard behavior and diagnostic handling without silent reboot.

## M3 — isolated native programs and services

Specify handles, syscall ABI, IPC, process lifecycle and executable loader.
Acceptance: two isolated processes communicate; invalid handles and malformed
requests fail safely; a crashing process does not terminate unrelated processes.
Begin an initial filesystem and supervisor with explicit failure behavior.

## M4 — native graphical desktop

Framebuffer first; input, focus, windows and a small native UI. Acceptance:
keyboard navigation, basic file browsing, stable redraw and crash recovery.
A static screenshot is not an interactive desktop acceptance test.

## Early feasibility tracks — begin after required infrastructure exists

Study Wine host dependencies and real GPU driver portability before UI polish.
Use COMPATIBILITY.md for explicit outcomes. These studies may invalidate the
Opera/WoW plan and must be allowed to report that honestly.

## M5 — local assistant prototype

Port a small runtime, benchmark a selected local model and implement permission-
checked file search/folder creation. Acceptance: real results, scoped operations,
path escape rejection, cancellation, bounded resources, useful failure reporting,
and responsive desktop while inference runs. Text first; speech later.

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

