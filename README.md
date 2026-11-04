# ZooOS

An independent x86-64 operating system written in Rust (minimal assembly),
with a macOS-style desktop and **Bella**, a local assistant that runs open
models on the CPU. You can type to her or talk to her. Version 0.113.0.

**Where it runs today: QEMU.** A USB stick image exists for real PCs, but
it has not been booted on real hardware yet. Real-hardware tests happen
only on the owner's other machines, never on the development PC
(AGENTS.md).

## What works

Each item below is implemented and checked in QEMU by `cargo xtask verify`.
docs/verification.md records the results, and MILESTONES.md lists every
step with its design document.

- **Kernel:**
  - boots under UEFI through the `bootloader` crate;
  - paging, a preemptive scheduler on every processor (SMP), isolated user
    programs, channels and shared memory;
  - drivers: virtio GPU (several monitors), block, network and sound
    (playback and recording); USB keyboard and mouse (xHCI, hubs); PS/2;
    local APIC timer.
- **Services:** Zookeeper supervises animal-named handlers, restarting
  them by policy:
  - BadgerHandler: the filesystem;
  - BeaverHandler: logs;
  - PeacockHandler: the window server;
  - Bella's helpers: ElephantHandler (the model), MagpieHandler (the file
    index), OtterHandler (the file broker), BatHandler (hearing).
- **Desktop:** windows with animations, a dock and menu bar, light and dark
  appearances, several windows per app, and tiling. The apps are Files
  (Finder-style, with Trash), Notes (an editor that saves to disk), Network
  and Bella.
- **Storage:** ZooFS 2, the home folder on disk, crash-safe across reboots.
- **Linux system calls:** unmodified musl programs, static and dynamic.
  Wine 11.0 (built from source) runs Windows' `cmd.exe`.
- **Networking:** IPv4, DHCP, DNS, TCP and plain HTTP. There is no TLS
  (HTTPS) yet.
- **Bella:**
  - **Models:** Qwen3-1.7B (4-bit) by default, and Qwen3-Embedding-0.6B
    for searching your files by meaning. Weights are open (Apache-2.0 or
    MIT), downloaded rather than committed.
  - **Commands:** opens, maximizes, minimizes, closes and tiles windows,
    across monitors; runs requests of several steps ("open notes and
    files, then put them side by side").
  - **Inside apps:** writes notes and fetches pages.
  - **Files:** reads, moves and summarises them, and answers from them.
  - **Memory:** remembers facts you tell her; the audit trail ("what did
    you do?") lists her changes.
  - **Speed:** a command takes about 0.5–0.8 s on 8 processors.
- **Voice:**
  - Moonshine Tiny turns speech to text in about 0.1 s for a spoken
    command on 8 processors.
  - A voice detector and a "hey Bella" gate pick out requests: "Hey Bella,
    open notes and maximize it" works through the microphone.
  - Tested with recorded speech only (docs/listening-design.md).

## Not done yet

- **Voice:** Bella can't answer aloud yet (Kokoro-82M is approved and
  next).
- **Real hardware:** no booted real PC; no Intel HD Audio sound driver; no
  real-GPU driver or GPU compute.
- **Networking:** HTTPS.
- **Windows apps:** beyond `cmd.exe` (Opera, Battle.net and WoW are later
  milestones).

See MILESTONES.md for the full list.

## Quick start

Prerequisites (QEMU, firmware, the pinned Rust nightly) are in
tools/firmware/README.md and BUILD.md.

    cargo xtask doctor                  # check the tools; installs nothing
    cargo xtask verify --skip-model     # the whole test suite in QEMU
    cargo xtask run                     # the desktop in a QEMU window

For faster runs, add `--accel whpx --cpus 8` on Windows, or `--accel kvm`
on Linux.

Bella's models are downloaded separately. Without them she answers typed
commands only. tools/models/README.md gives each file with its source,
pinned commit and SHA-256. With them:

    cargo xtask run --accel whpx --cpus 8         # Bella with her models
    cargo xtask run --mic --accel whpx --cpus 8   # ...and listening on this PC's microphone

`--mic` connects QEMU's microphone to this PC's default recording device
while QEMU runs. Say "Hey Bella, open notes and maximize it". Other
options: `--monitors N` (several monitors), `--mode normal` (the boot
checks, then a debug shell), and `cargo xtask usb-image` (a stick image
for real PCs).

## Repository

| Path | What it is |
| --- | --- |
| `kernel/` | the kernel |
| `user/` | the runtime (`zoo_rt`) and every program: Zookeeper, handlers, apps, tests |
| `abi/` | the system call ABI and formats shared by kernel and programs |
| `infer/` | the inference engine: GGUF, Qwen, Moonshine, voice detection |
| `c/` | C test programs (Zig toolchain) |
| `xtask/` | the host tool: build, run, verify, model checks, images |
| `seed/` | files packed into the boot image (home folder, `etc/`) |
| `tools/` | toolchain, firmware and model provenance; the NumPy reference implementations |
| `docs/` | a design document per step; `verification.md` is the record of results |

Project documents:
- AGENTS.md: rules for contributors and agents.
- ARCHITECTURE.md: system diagrams and direction; parts are proposals.
- DECISIONS.md: decisions and their reasons.
- BUILD.md: build and command contract.
- MILESTONES.md: the steps and their status.
- COMPATIBILITY.md: Windows application feasibility.

## Name

The project was renamed from the placeholder OriginsOS to ZooOS on
2026-10-30. Technical identifiers use `zoo-os`. A web, GitHub and
crates.io search found no other OS called ZooOS; no trademark search has
been done (DECISIONS.md).
