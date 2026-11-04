# More than one monitor (2026-11-04)

Status: implemented for virtio-gpu (QEMU). The third item of the owner's
voice-control order: "Hey Bella, put Notes maximized on monitor 3".
**On real hardware ZooOS has one monitor**: UEFI gives the kernel one
framebuffer, and driving more needs a GPU driver (the GPU stage, tested
on the owner's GTX 1650 Super machine, never this PC). This step builds
everything above the driver, so a GPU driver only has to report its
monitors.

## 1. One wide screen

The monitors are side by side, as wide and tall as each other, and the
window server sees **one wide screen**: two 1920x1080 monitors are a
3840x1080 desktop.

- **Kernel** (`virtio_gpu.rs`): the screen resource is `monitors` times
  as wide; scanout k shows the rectangle (k * width, 0, width, height)
  of it (virtio-gpu's `SET_SCANOUT` takes a rectangle). The hardware
  cursor goes to the scanout under it. `DisplayInfo::monitors` says how
  many (1 for a firmware framebuffer). ABI version 18.
- **How many:** `opt/zooos/monitors` from the host (fw_cfg), else the
  scanouts the device reports enabled. A QEMU without a window enables
  only the first, so `verify` and `run --monitors N` pass the count.
- **QEMU:** `virtio-vga,max_outputs=N,id=vga0`; the verifier saves
  `<shot>-m2.png` and on with `screendump` of each head.

## 2. The window server

- The menu bar, the dock and banners are on monitor 1 (`mw`, a
  monitor's width), as on a Mac with one menu bar; windows start there.
- Maximize (the green light and Bella's) fills the window's own monitor;
  split works on one monitor; `split-front` on the first window's.
- **Window actions take a monitor:** `window:<action>@<n>:<program>`
  (1-based) or `@next` (the one after the window's own). New action
  `move`: the window keeps its size and place on the other monitor (a
  filled window stays filled). A monitor that is not there is refused:
  "There is no monitor 3; this screen has 2."

## 3. Bella

The model's prompt is unchanged. **When a request names a monitor and an
app** (`assist::monitor_named`: "monitor 3", "screen two", "the second
display", "the 2nd monitor", "the other monitor"), the choice becomes:

| The request | Command |
| --- | --- |
| says maximize, maximized, full screen or fill | `maximize Notes on monitor 3` |
| the model chose `split` | `split Notes and Files on monitor 2` |
| anything else ("move", "put", "show") | `show Files on monitor 2` (move) |

This is decided after the model's choice and before anything else uses
it, so "move files to the second screen" (the model's `move`, a file
command) never moves a file. Typed: `maximize notes on monitor 2`,
`split notes and files on monitor 2`, `show notes on monitor 2`. Bella's
own window may be placed, never closed or minimised this way.

## 4. Verified

| Case | Result |
| --- | --- |
| `gpu-monitors` (TCG, 2 monitors, typed) | `OK`: `gpu scanout 3840x1080 ... monitors=2`; `maximize notes on monitor 2` → `placed Notes at 1944x38 1872x948`; `split notes and files on monitor 2` → `placed Files at 2886x38 930x948`; `show files on monitor 3` → "There is no monitor 3; this screen has 2."; screenshots of both monitors checked by eye |
| `gpu-monitors` with the window server ignoring `@n` (negative control) | `FAIL`: no `placed Notes at 1944x38 1872x948` within 120 s |
| `gpu-monitors-model` (3 monitors, `--accel whpx --cpus 8`) | `OK`: "hey bella put notes maximized on monitor 3 please" → ``chose maximize `Notes on monitor 3` ``, `placed Notes at 3864x38 1872x948`, 581 ms; "can you move files to the second screen" → ``chose show `Files on monitor 2` ``, 481 ms |
| `assist-check` (Qwen3) | `PASS`: 32 of 34, 0 of 8 (unchanged) |

## Not done

- **Real monitors** need a GPU driver (later stage).
- Monitors of different sizes, or above and below each other.
- A menu bar or dock on the other monitors; dragging a window across is
  allowed, but nothing snaps.
