# Feasibility study: hosting Wine on ZooOS

| Field | Value |
| --- | --- |
| Date | 2026-10-30 |
| ZooOS version | 0.6.0 (after M4) |
| Gate (COMPATIBILITY.md) | Wine hosting; Windows behaviour |
| Procedure | Desk study: Wine's current architecture and documented ports, compared with the facilities ZooOS has today. No code was run; nothing below is a test result. |
| **Status** | **BLOCKED**: ZooOS lacks nearly every host facility Wine's Unix side needs (§3). No experiment is possible yet. |

## 1. Question

Can Wine (the most likely source of Windows API compatibility code, per
ARCHITECTURE.md) run on ZooOS, and what would it take to get to the gate's
first concrete step: "compile and run a minimal console executable"?

## 2. How Wine is built (relevant facts)

- Wine is split into a **PE side** (Windows DLLs compiled as PE, largely
  host-independent) and a **Unix side** (`ntdll`'s Unix half, the
  `wineserver` process, and drivers such as the X11, Wayland and macOS
  display drivers). The Unix side is what a host OS must support.
- **Wine 11.0 (January 2026) completed the new WoW64 architecture:** 32-bit
  Windows programs run in a 64-bit Wine without 32-bit host libraries, with a
  single loader. This helps: a host needs only a 64-bit Unix side. Wine 11 can
  also use Linux's NTSync module for synchronization, but that is optional and
  Linux-specific. Sources: [AlternativeTo](https://alternativeto.net/news/2026/1/wine-11-0-debuts-full-wow64-support-ntsync-for-linux-and-graphics-updates),
  [Betanews](https://betanews.com/article/wine-11-finally-fixes-one-of-the-biggest-problems-with-running-old-windows-apps),
  [GamingOnLinux](https://www.gamingonlinux.com/2026/01/windows-compatibility-layer-wine-11-arrives-bringing-masses-of-improvements-to-linux/page=1).
- **Precedent, Haiku (2022):** a developer ported Wine to Haiku, a
  POSIX-compatible OS with years of existing infrastructure. 64-bit apps such
  as 7-Zip, Inkscape and Notepad++ ran, with gaps (screen size, focus). The
  main platform-specific piece was **a new Wine display driver for Haiku's
  native graphics**, replacing the X11 driver. Sound needed a new backend too.
  Sources: [Liliputing](https://liliputing.com/2022/01/run-some-windows-apps-on-haiku-operating-system-thanks-to-wine-port.html),
  [Phoronix](https://www.phoronix.com/news/Wine-On-Haiku-Starts-2022),
  [Haiku forum](https://discuss.haiku-os.org/t/wine4haiku/2269).

The Unix-side requirements below come from Wine's design (a POSIX program
that maps Windows calls onto POSIX equivalents, plus `wineserver` for shared
state) and must be confirmed against the Wine source when porting starts.

## 3. What Wine's Unix side needs, and what ZooOS has

| Host facility | Wine uses it for | ZooOS 0.6.0 | Gap |
| --- | --- | --- | --- |
| C toolchain and libc for the OS (ELF, `dlopen`) | building and loading the Unix side and its `.so` modules | none: only Rust `no_std` programs, static | **large**: a libc port (e.g. musl or relibc) and a cross toolchain |
| Processes: create with arguments and environment, wait, exit codes | `CreateProcess`, `wineserver`, helper processes | `spawn` from the initrd with one handle, no arguments | medium |
| Threads, thread-local storage, futex-style waits | every Windows thread is a host thread | single-threaded processes, no TLS | **large** |
| Virtual memory: map/unmap/protect at chosen addresses, reserve large ranges, file-backed and shared mappings | the Windows address space, PE image mapping, `VirtualAlloc`, sections | fixed layout set by the kernel; no user mapping calls | **large** |
| Exceptions to user handlers (fault address and registers), alternate signal stack, suspending other threads | Windows structured exception handling, debuggers, `SuspendThread` | a user fault ends the process | **large** |
| Local IPC with descriptor passing; pipes; `poll` | talking to `wineserver`, sharing handles | channels of 256-byte messages, no descriptors | medium |
| A real filesystem: create, read, write, rename, delete, directories, locks | the Windows drive (C:), registry files, prefixes | read-only initrd, list and stat only | **large** |
| Clocks and timers | `GetTickCount`, `Sleep`, timers | 100 Hz ticks, `sleep` | small |
| Network sockets (TCP, UDP, DNS) | Winsock; Battle.net and Opera need the network | no network driver or stack | **large** |
| A display driver: windows, surfaces, input | `user32`/`gdi32` on screen | one fixed window server, text protocol, no surfaces or mouse | **large** (Haiku's experience: this is the main platform piece) |
| Audio, fonts (FreeType), TLS crypto (GnuTLS for Schannel) | sound, text rendering, HTTPS | none | medium each |
| Vulkan/OpenGL | Direct3D through Wine or DXVK/vkd3d-proton | none (see gpu.md) | blocked by the GPU gate |

## 4. Strategies

| Strategy | Summary | Assessment |
| --- | --- | --- |
| A. POSIX layer, then port Wine | Grow ZooOS system calls and a libc until Wine's Unix side builds largely unmodified, as Haiku did; write a ZooOS display driver for Wine | Keeps close to upstream Wine. Requires most of a POSIX OS first. Each row marked large in §3 is milestone-sized. |
| B. Re-implement Wine's Unix side natively | Write ZooOS-specific `ntdll` Unix code and a `wineserver` replacement on ZooOS IPC | Avoids POSIX emulation, but forks Wine's most complex parts and loses upstream fixes. Not recommended. |
| C. Run a Linux VM as an application | Host Wine inside a Linux guest | Explicitly ruled out as the foundation (CLAUDE_PROMPT.md, COMPATIBILITY.md: running apps on Linux is not a pass). |

## 5. Application-level notes

- **WoW / Battle.net under Wine on Linux** work for many users today, with
  DXVK (D3D 8–11) and vkd3d-proton (D3D 12) translating to Vulkan, but
  reports of crashes and installer hangs recur. Sources:
  [Proton (Wikipedia)](https://en.wikipedia.org/wiki/Proton_(software)),
  [Blizzard forum](https://us.forums.blizzard.com/en/wow/t/wow-classic-keeps-crashing-randomly-on-linux/1724941),
  [Lutris forum](https://forums.lutris.net/t/correctly-updating-wine-versions-for-battle-net-and-wow/23093),
  [WineHQ forum](https://forum.winehq.org/viewtopic.php?p=144379).
  **Even on Linux this needs a working Vulkan GPU driver**, which ZooOS does
  not have (gpu.md).
- **Opera (Chromium) under Wine** has a long history of breaking on
  Chromium's sandbox. The historical workaround is `--no-sandbox`, and a
  WineHQ bug about it was closed WONTFIX; later Chromium sandbox regressions
  were fixed in Wine. Sources:
  [WineHQ bug 21232](https://list.winehq.org/archives/list/wine-bugs@list.winehq.org/thread/FZPBIZIZPER6PJAOWC7S7UZNDJFWRCJ4),
  [bug 56724](https://list.winehq.org/archives/list/wine-bugs@list.winehq.org/thread/CR5BK7K4AHAYSBLMTHROFGZNEEPZ6RCM).
  **ARCHITECTURE.md forbids weakening browser sandboxing to pass**, so a
  sandbox regression would block the Opera milestone, not be worked around.
- No current WineHQ AppDB ratings for Battle.net, WoW or Opera were found;
  AppDB ratings are user-submitted and have been unreliable for online games
  ([WineHQ bug 9702](https://list.winehq.org/archives/list/wine-bugs@list.winehq.org/thread/S56GHPBRAQMFXUDIKEGWVNRMPTXBIFEM)).

## 6. Outcome and next decision

- **Status: BLOCKED.** The blockers are the host facilities marked large in
  §3. No Windows executable can be loaded on ZooOS yet.
- **Smallest useful next experiment** (if this path is kept): a ZooOS-native
  PE loader that runs a *minimal console executable* calling a handful of
  `kernel32` functions implemented natively. It would show the process,
  memory-mapping and exception facilities a PE program needs, without Wine.
  That is the first row of the COMPATIBILITY.md matrix, and builds the same
  facilities strategy A needs.
- **Decision for the project owner (scope):** keep strategy A as the
  long-term plan (multi-milestone: threads, memory mapping, exceptions,
  writable filesystem, networking, libc, display driver), or narrow the
  Windows goals. Either way the WoW target also depends on the GPU gate.
