# M4 design: the first native graphical desktop

Status: design written before implementation, then implemented and verified
(2026-10-30). Results are in docs/verification.md; §10 lists what changed
during implementation.

M4 acceptance (MILESTONES.md): framebuffer first; input, focus, windows and a
small native UI. Keyboard navigation, basic file browsing, stable redraw and
crash recovery. A static screenshot is not an interactive desktop test.

This is the first step of ARCHITECTURE.md's desktop: a user-space window
server and two native applications, supervised by Zookeeper. Mouse, a scene
graph, translucency, animation, a widget toolkit and accessibility are
deferred (§9).

## 1. Shape

```
shell `desktop` ──▶ kernel spawns Zookeeper (session mode, control channel)
                         │ spawns, waits, restarts on crash (max 3)
                         ▼
                 PeacockHandler  (window server: owns display + keyboard)
                   │ channel           │ channel
                   ▼                   ▼
             OwlHandler (Files)   ChimpHandler (crash-test app)
```

All three are user processes. The kernel only hands the framebuffer and
the keyboard to one process at a time and reclaims them when it ends.

## 2. Kernel additions (ABI version 2)

New system calls (existing numbers unchanged; ABI version bumped to 2 because
calls were added):

| # | Call | Arguments | Result |
| --- | --- | --- | --- |
| 11 | `ticks` | – | timer ticks since boot (100 Hz) |
| 12 | `sleep` | ticks | 0 after at least that many ticks |
| 13 | `try_recv` | handle, ptr, capacity | like `recv`, but `AGAIN` instead of blocking |
| 14 | `display_acquire` | ptr to `DisplayInfo` | 0; maps the framebuffer into the caller |
| 15 | `input_read` | ptr, capacity | number of keyboard scancode bytes copied (0 if none) |
| 16 | `fs_stat` | index, ptr to `FsEntry` | 0, or `NOTFOUND` past the last file |

New errors: 13 `BUSY` (display owned by another live process), 14 `AGAIN`
(nothing to receive yet). `input_read` from a process that does not own the
display returns `RIGHTS`.

- **Display ownership.** One owner at a time. `display_acquire` maps the
  framebuffer's physical pages into the caller at `0x0000_4000_0000_0000`
  (user, writable, no-execute) and fills `DisplayInfo` (address, width,
  height, stride in pixels, bytes per pixel, format). The kernel console
  stops drawing (serial output continues) and the debug shell stops reading
  the keyboard. Marker: `ZOO_OS DISPLAY acquired pid=… name=…`.
- **Release.** When the owner exits, crashes or is killed:
  `ZOO_OS DISPLAY released pid=…` (printed right after its `PROC` marker);
  the console clears and resumes; the shell reads the keyboard again. The
  framebuffer pages are unmapped from the dead process's space before it is
  freed, and are never returned to the frame allocator.
- **Input.** Keyboard bytes still arrive through the IRQ1 queue. While a
  process owns the display, the queue is drained only by its `input_read`.
- **Sleep.** A sleeping task is blocked with a deadline; the timer marks it
  ready when the deadline passes. The timer also wakes the display owner when
  keyboard bytes are waiting.
- **Files.** `fs_stat(i)` describes the `i`-th file of the initial filesystem
  (name and size), enough to browse it. No file contents yet.
- **Session control channel.** The shell command `desktop` makes the kernel
  create a channel, queue the message `session` on Zookeeper's end and spawn
  Zookeeper with it. Zookeeper started without a handle (at boot) runs the
  M3 scenario as before.

## 3. Zookeeper session mode

Spawns `peacock-handler` and waits. Exit code 0 ends the session (Zookeeper
exits 0). A crash, kill or non-zero exit restarts it, up to 3 times, then
Zookeeper gives up and exits 1. Each restart is logged.

## 4. PeacockHandler: window server

- Acquires the display and keyboard; decodes scancodes with `pc-keyboard`
  (US layout), as the kernel shell does.
- Draws into a back buffer and copies whole frames to the framebuffer, so
  the screen never shows a half-drawn frame. Redraws only after an event, or
  once a second for the clock.
- **Layout:** menu bar on top (`ZooOS`, the focused window's title, uptime in
  seconds), two windows side by side, and a status line at the bottom. The
  focused window's title bar is highlighted. Text uses the same 16 px font as
  the kernel console.
- **Keys:** `Tab` moves focus to the next window; `Up`, `Down`, `Enter`,
  `Backspace` and printable keys go to the focused application; `Escape`
  ends the session (exit 0); `F12` is a **test key** that makes the window
  server itself fault, to exercise Zookeeper's restart. It is documented and
  intended for testing only.
- **Applications** are started by the server, one channel each. The server
  loop polls keyboard input and every application channel (`try_recv`),
  sleeps one tick when idle, and redraws when something changed.
- **App crash recovery:** a closed channel means the application ended. The
  server shows `<App> crashed; restarted (n of 3)` in the status line and
  restarts it, up to 3 times per application.

## 5. Application protocol

Text messages, at most 256 bytes, over the application's channel:

| Direction | Message | Meaning |
| --- | --- | --- |
| app → server | `title:<text>` | window title |
| app → server | `lines:<n>` | the window has `n` content lines (0–24); clears the rest |
| app → server | `line:<i>:<text>` | content line `i` |
| app → server | `sel:<i>` | highlighted line, or `sel:-` for none |
| app → server | `show` | content complete; redraw |
| server → app | `key:up`, `key:down`, `key:enter`, `key:back`, `key:char:<c>` | a key for the focused app |

Unknown or malformed messages are ignored. When the server's end closes, an
application exits 0.

## 6. Applications

- **OwlHandler (Files):** lists the initial filesystem with `fs_stat`.
  `Up`/`Down` move the selection; `Enter` opens a details view (name, size,
  entry number); `Backspace` goes back.
- **ChimpHandler (crash test):** a window that says to press Enter; `Enter`
  makes it read kernel memory, which must kill only ChimpHandler.

## 7. Drawing and colours

All text is drawn by blending the glyph intensity `i` between the background
and foreground colours, per channel: `(bg·(255−i) + fg·i + 127) / 255`. The
verifier uses the same formula to find exact text anywhere in a screenshot.

| Element | Background | Text |
| --- | --- | --- |
| Desktop | `#1E3A4C` | – |
| Menu bar, status line | `#E8E8E8` | `#202020` |
| Focused title bar | `#3D7EBF` | `#FFFFFF` |
| Unfocused title bar | `#9AA5AE` | `#202020` |
| Window content | `#FFFFFF` | `#202020` |
| Selected line | `#CFE3F7` | `#202020` |

## 8. Verification

A new `desktop` case drives the real guest through QMP, with several
screenshots and the kernel markers between them:

1. Type `desktop` + Enter in the shell. Markers: Zookeeper (session),
   PeacockHandler start, `DISPLAY acquired`, OwlHandler and ChimpHandler
   start. Screenshot: menu bar `ZooOS`, focused title `Files`, a
   `bin/zookeeper` line.
2. `Down`, `Down`, `Enter`. Screenshot: Files shows the details view
   (`Size:` line).
3. `Tab`, `Enter`. Markers: ChimpHandler crash and its restart. Screenshot:
   status line `ChimpHandler crashed; restarted (1 of 3)`.
4. `F12`. Markers: PeacockHandler crash, `DISPLAY released`, then (in any
   order) the old apps' exits and the new PeacockHandler, `DISPLAY
   acquired` and new apps. Screenshot: the desktop is back (`ZooOS`, `Files`).
5. `Escape`. Markers: PeacockHandler exit 0, `DISPLAY released`, then (any
   order) app exits and Zookeeper exit 0. Then type `echo back` + Enter in
   the restored kernel shell. Screenshot: console row `zoo> echo back`.

To support this, the verifier gains multi-step interactions (keys, wait for
quiet serial output, screenshot + check) and **unordered marker groups**, for
events that happen concurrently in separate processes. Each screenshot check
also asserts a deliberately wrong string is absent.

## 9. Deferred

Mouse and pointer input, window moving/resizing, overlapping windows and a
scene graph, translucency and animation, a widget toolkit, fonts beyond the
16 px bitmap, accessibility tree, display scaling, Unicode, file contents,
writable files, launching arbitrary apps from the desktop, USB input.

## 10. Changes made during implementation

- **The shell waits for the session to settle.** The first verify run
  showed the kernel shell printing its prompt the moment the window server
  released the display, while Zookeeper was still printing its last lines,
  so typed text landed after them. Now, when the display is released, the
  shell waits (at most 5 s) until either a process takes the display back
  (a restart, so no "ended" message) or the session's Zookeeper has exited,
  then prints `Desktop session ended.` and the prompt.
- **Details view** shows the entry number rather than the archive offset
  (`fs_stat` does not expose offsets).
