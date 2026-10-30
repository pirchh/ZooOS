# Compatibility and feasibility register

All entries start UNTESTED. This document is a research/test plan, not evidence
of support. Native Windows applications must execute on the independent guest;
running them on the host, inside Windows, or on a Linux VM is not a pass.

## Required result format

For each experiment record date, source references, exact component/app version,
commit, hardware/virtual devices, prerequisites, procedure, logs, expected versus
actual behavior, blockers and next decision. Status: UNTESTED, BLOCKED, PARTIAL,
PASS for the specified workflow, or FAIL. A PASS is scoped to that version and
workflow and does not mean vendor support or future compatibility.

## Infrastructure gates

| Gate | Questions and evidence |
| --- | --- |
| Wine hosting | Required host APIs, threading, signals/exceptions, memory mappings, executable loading, synchronization, sockets, files, fonts, GUI and build dependencies; compile and run a minimal console executable |
| Windows behavior | Required PE imports, process environment, registry, COM/services, installers and 32-bit helpers; do not infer coverage from 64-bit kernel support |
| GPU | Selected device, firmware/license, command submission, memory management, display, synchronization, reset and recovery; runnable accelerated sample |
| Graphics translation | Exact DirectX needs and translation component host/API requirements; demonstrate rendered frames and correctness on the guest |
| Local inference | Runtime host assumptions, threads, atomics, allocator, file mappings and CPU features; real local inference with measured memory/latency |
| Phone | Current official iOS APIs and companion-app constraints; paired, authenticated transfer and revocation on real devices |

Compatibility code reuse needs a license/dependency inventory. A Vulkan library
or translation layer alone is not a GPU driver. Treat GPU and Wine portability
as independent feasibility investigations with explicit stop/rethink outcomes.

## Application acceptance matrix

| Target | Required workflow | Status |
| --- | --- | --- |
| Minimal Windows console app | Load executable, invoke required APIs, correct output and exit status | UNTESTED |
| Simple Windows GUI app | Window, text, keyboard/mouse, filesystem persistence | UNTESTED |
| Installer/multiprocess app | Install, subprocesses, restart, uninstall and state | UNTESTED |
| Opera (edition TBD) | Official installer, startup, tabs, HTTPS, persistent profile, downloads, media, acceleration, sandbox and update | UNTESTED |
| Battle.net | Official installer, login including required authentication, agent lifecycle, download, verify/repair, patch, restart and launch | UNTESTED |
| WoW (edition TBD) | Installation through Battle.net, login, realm/character selection, sustained gameplay, input/audio/network, addons where required, restart and update | UNTESTED |

Select exact release/build numbers when experiments start. Keep credentials and
session tokens out of logs and source control. Respect software access controls;
never patch around authentication or integrity checks to label a test passed.

## Performance evidence

Use the same physical reference machine for Windows comparison. Record resolution,
settings, drivers, app versions, power mode and workload. Repeat measurements;
report launch latency, frame-time distributions, average FPS, 1% lows, memory and
stability. Include assistant/indexer contention tests. QEMU software rendering
cannot establish final gaming performance.

## Product inputs still open

Reference hardware, Opera or Opera GX, WoW edition, essential browser features,
required addons/peripherals and acceptable performance budgets. These do not
block M0/M1. They must be resolved before application support is committed.

## Starting primary sources

- https://www.winehq.org/about/
- https://github.com/ValveSoftware/Proton
- https://www.opera.com/download/requirements
- https://partner.steamgames.com/doc/steamdeck/proton

Check current official application and iOS documentation when relevant stages
begin. Reports about Linux compatibility are leads, not proof of our OS support.

