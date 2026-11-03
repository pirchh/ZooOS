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
| L6 | Signal handlers and frames, fault signals, threads and futexes, lazy anonymous memory (docs/linux6-design.md) | done 2026-10-31 |
| L7 | Unix stream sockets with `SCM_RIGHTS`, `poll`/`select`/`epoll`, `eventfd`, non-blocking descriptors (docs/linux7-design.md) | done 2026-10-31 |
| L8 | Dynamically linked programs: musl's loader built from source, `PT_INTERP`, file-backed and fixed `mmap` (docs/linux8-design.md) | done 2026-10-31 |
| L9 | The Linux address-space layout Wine needs: whole lower half, image reservations, 8 MiB lazy stack, Linux `munmap`/`MAP_FIXED` carving (docs/linux9-design.md) | done 2026-10-31 |
| L10a | Wine 11.0 built for musl (WSL Alpine), packed into a `wine` image; `wine --version` runs; Wine reaches its server (docs/linux10-design.md) | done 2026-10-31 |
| L10b | tmpfs for `/tmp`, `memfd`, shared and copy-on-write `mmap`, symbolic links, GS base, devices: Wine runs `cmd.exe`, a first Windows program (docs/linux10b-design.md) | done 2026-11-01 |
| L10c | ZooFS 2: the home folder on disk block by block, crash-safe generations, 4 GiB files, multi-block disk requests (docs/linux10c-design.md) | done 2026-11-01 |
| L10d | Wine's prefix in the home folder: Linux names, home symbolic links, `*at` calls, `mmap` of home files, gathered small writes; the second start in seconds (docs/linux10d-design.md) | done 2026-11-01 |
| U1 | macOS-style desktop: Inter text from a build-time atlas, rounded windows with traffic lights and shadows, Mac menu bar, dock, notification banners, light and dark appearances, 1920x1080 (docs/u1-design.md) | done 2026-11-01 |
| U1b | Input reaches the window server at once instead of at the next 10 ms tick (docs/u1b-design.md) | done 2026-11-01 |
| U1c | USB input by MSI-X/MSI interrupt; USB tablet for `run` (absolute pointer, no grab) (docs/u1c-design.md) | done 2026-11-01 |
| U2a | Compositor: cached window layers, partial-line content updates, merged damage, direct GPU drawing; drags 3–4× cheaper (docs/u2-compositor-design.md) | done 2026-11-01 |
| U2c | The pointer drawn in the frame during drags, so a dragged window stays under it (docs/u2c-design.md) | done 2026-11-01 |
| U2d | Window animations: open, close, minimise to the dock, restore; a cycle-counter animation clock (docs/u2d-design.md) | done 2026-11-01 |
| U2e | The dock hides itself (auto-hide; shown at the bottom edge and during a genie) (docs/u2e-design.md) | done 2026-11-01 |
| U2f | The ZooOS mark: the soft paw, an anti-aliased vector mask (docs/u2f-design.md) | done 2026-11-01 |
| U2 | Dock (magnification: U2g; launch bounce: U2h; icon artwork: U3g) | done |
| U2h | The launch bounce: an opening app's dock icon hops twice (docs/u2h-design.md) | done 2026-11-02 |
| U2g | Dock magnification: icons swell along a bell curve around the pointer, the hovered one stays under it, sharp from an 84 px cache (docs/u2g-design.md) | done 2026-11-02 |
| U2b | The monitor's own resolution in QEMU (host size through fw_cfg); 2× drawing for 4K screens (docs/u2b-design.md) | done 2026-11-01 |
| U3 | Several windows per app (Ctrl+N); right-click menus on dock icons (New Window, Remove from / Keep in Dock, Quit); minimised windows after a divider; Files' Applications folder (Open, Add to Dock); dock pins saved in `.dock` (docs/u3-design.md) | done 2026-11-01 |
| U3f | Finder-style Files: sidebar, icons, size column, double-click opens, New Folder / New Text File; Notes edits any file with coloured program text, Ctrl+S; rich content lines (colours, icons, caret, columns) (docs/u3f-design.md) | done 2026-11-02 |
| N3 | Network app (connection, fetch a page by name or address, text shown) and a DNS client (docs/n3-design.md) | done 2026-11-02 |
| N1 | Network device (virtio-net, polled), raw-frame syscalls, IPv4 stack in user space: ARP, ICMP, UDP, DHCP client (docs/n1-design.md) | done 2026-11-02 |
| N2 | TCP client and an HTTP request to a host-side test server (docs/n2-design.md) | done 2026-11-02 |
| A2b | The Assistant's file tools take names in any letter case (`list music` lists `Music`) | done 2026-11-02 |
| A3 | The model built into the everyday system (`run`, `usb-image`; `--no-model` to leave out); boot cost measured (+1.85 s) (docs/a3-design.md) | done 2026-11-02 |
| A2 | The Assistant learns: request → command memory on disk, learned from answers and corrections; alike requests answered at once without the model (docs/a2-design.md) | done 2026-11-02 |
| A1 | Faster model engine: vector dot products, no unread scores, precomputed rotary frequencies; 2.0–2.7× in the guest, answers unchanged (docs/a1-design.md) | done 2026-11-02 |
| U3h | Files: Rename in place, Move to Trash, the Trash in the sidebar, Put Back (docs/u3h-design.md) | done 2026-11-02 |
| U3g | Dock and app icons with depth: lit gradient tiles, rims, soft shadows, shadowed symbols, cached per scale (docs/u3g-design.md) | done 2026-11-02 |
| U4 | Animated window moves (menus and shortcuts: U4a; saved appearance: U4b) | proposed |
| U4b | The appearance kept across restarts; Notes wraps between words (docs/u4b-design.md) | done 2026-11-02 |
| U4a | Menus that open from the menu bar (ZooOS, app, File, Edit, View, Window, Help), sliding between them, Ctrl+W / Ctrl+M (docs/u4a-design.md) | done 2026-11-02 |
| GPU1 | virtio-gpu (via `virtio-vga`) for the desktop: flushed updates and a hardware cursor the host draws; used by `run` (docs/gpu1-design.md) | done 2026-11-01 |
| U5 | Pointer feel on real hardware: acceleration curve tuned on a real mouse; cursor planes on real GPUs (per GPU family) | proposed |

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

## Bella roadmap (owner direction 2026-11-02)

The assistant is named Bella. The plan is docs/bella-roadmap.md, built on
the code audit in docs/bella-audit.md. Stages run in this order; each
lands as small, separately verified steps.

| Stage | Content | Status |
| --- | --- | --- |
| 0 | Fix what the audit found: B1 Notes truncation, B2 stale folder handles, B6 bulk buffer size, B7 sessions per process (docs/s0-design.md) | B1, B2, B6, B7 done 2026-11-02 |
| 0 | B3, B4, B8: the memory learns only right answers and replays only safe ones (docs/s0-design.md) | done 2026-11-02 |
| — | The assistant renamed Bella everywhere in the OS (app, program, transcript, memory file, tests) | done 2026-11-02 |
| 0 | B5: restart counts reset after 30 s; the desktop reloads with a fresh home folder after a filesystem restart; B15: m5c1 current state (docs/s0-design.md) | done 2026-11-02 |
| 1 | Measure: "none of these" (B9) and a 22-request assist-check with a bar; per-request timing lines; peak heap; WHPX baselines (docs/stage1-design.md) | done 2026-11-02 |
| 1 | Responsiveness while the model runs | open |
| 2 | B12: background programs; input preempts them; the model runs in the background (input wait during inference 4.7 → 0.25 ms) (docs/stage2-design.md) | done 2026-11-02 |
| 2 | The model warms up in the background as Bella opens: first request 6.1 → 1.0 s (docs/stage2-design.md §2) | done 2026-11-02 |
| 2 | The model on its own read-only disk: desktop up 2.93 → 1.21 s; B10 measured (52 µs per request) (docs/stage2-design.md §3) | done 2026-11-02 |
| 2 | Bella's helpers start again after a back-off; broker deadline (docs/stage2-design.md §4) | done 2026-11-02 |
| 2 | B13: the kernel heap under pressure: a 1 MiB reserve programs cannot use (NOMEM, not a panic); no channel-end cycles (docs/heap-design.md) | done 2026-11-03 |
| 2 | Bella's audit trail: a log line per broker request; changes kept in `.bella-audit`; "what did you do?" (docs/audit-design.md) | done 2026-11-03 |
| 5 | Bella summarises a file in a model call of its own: "summarise Documents/notes.txt" → "a list of items to buy for a shopping trip" in 2.4 s (docs/summary-design.md) | done 2026-11-03 |
| 5 | Bella saves a summary beside its file ("save that"); the broker's `write` never replaces a file (docs/save-design.md) | done 2026-11-03 |
| — | Bella answers as she writes (first words 0.3–2.2 s with 0.5B), with an instant line before slow answers; Qwen3-1.7B runs for evaluation (docs/stream-design.md, docs/qwen3-design.md) | done 2026-11-03 |
| 5 | Qwen3-1.7B in 4 bits (`cargo xtask requant`): about 1.5B's speed, better summaries, assist-check 17/23 and 0/8 (docs/qwen3-design.md) | done 2026-11-03 |
| 2 | B14 keys during broker calls: measured 2–10 ms per call, not changed (docs/stage2-design.md §4) | done 2026-11-03 |
| — | Bella answers from your files: by meaning (Qwen3-Embedding-0.6B, MagpieHandler at idle priority, `.bella-index`), else by words; model disk v2; prompts read in batches (docs/vectors-design.md, docs/lookup-design.md, docs/batch-design.md) | done 2026-11-03 |
| — | 8-bit activations: about 1.3× faster; 0.5B small talk 0.62 s whole (docs/int8-design.md) | done 2026-11-03 |
| — | More than one processor, step 1: the other processors start (MADT, SIPI, trampoline) and park (docs/smp-design.md) | done 2026-11-03 |
| — | More than one processor, steps 2–3: tasks on every processor; the model's work split across them; 0.5B under a second for every case with 8 processors (docs/smp-design.md) | done 2026-11-03 |
| 2 | Still open: narrowing the broker's rights | open |
| — | Bella talks (replies in words; small talk; sentence routing) and `--bella 1.5b` (Q6_K, 4 GiB heap) (docs/talk-design.md) | done 2026-11-02 |
| — | Bella keeps the conversation: a running chat in the model's cache, noted command results, follow-ups with context (docs/talk-design.md §6) | done 2026-11-03 |
| 3 | XSAVE, CPU features, an AVX2 backend: inference 2–2.5× faster in the guest (0.5B request 1.08 → 0.52 s) (docs/stage3-design.md) | done 2026-11-03 |
| — | Broad access: read, open, move/undo, trash (asks), launch, info; names resolved against real files (docs/access-design.md) | done 2026-11-03 |
| — | Bella remembers facts the user tells her (on disk), answers from them, forgets on request with confirmation (docs/facts-design.md) | done 2026-11-03 |
| 4 | Bella sees the screen: "what am I working on?" answered from the window server's snapshot (docs/context-design.md) | done 2026-11-03 |
| 4 | The action and event interface: app action manifests, `do:` invocations, event subscribers | open |
| 5 | Bella v1: workflow templates, safe summarising, "none of these", model bake-off, memory provenance | open |
| 6 | SMP | open |
| 7 | Hardware base and real disks: ECAM, MSI-X, IOMMU, flushes (B11), USB storage | open |
| 8 | GPU compute: RTX 3050, then RTX 5060 Ti, each milestone a stop point | open |
| 9 | Voice | open |
| 10 | Model tiers | open |

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
| F2 | Faster first-time writes (file growth) (input wakes at once since U1b, USB by interrupt since U1c) | proposed |

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

