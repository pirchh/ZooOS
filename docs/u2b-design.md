# UI step 2b (U2b): the monitor's own resolution, and 2x for 4K

Status: implemented 2026-11-01 (version 0.58.0). The owner runs QEMU
maximised on their main monitor, where the 1920×1080 desktop looked "low
resolution when it's blown up".

## 1. Why it looked soft

**Measured on the owner's PC:**
- The main monitor runs at **2560×1440, 59 Hz** (`EnumDisplaySettings`,
  `DISPLAY1`). The owner believed it was 4K. The other four
  monitors are 1920×1080.
- QEMU on Windows is system-DPI-aware (`GetProcessDpiAwareness` = 1), and
  its window reported 96 DPI, so Windows does not stretch it.
- So the softness was QEMU scaling a 1920×1080 picture by 1.33 to fill a
  2560-pixel-wide window.

**The fix:** a guest screen of the monitor's own size, shown one-to-one.
- A 2560×1440 monitor is drawn at 1x, using all of it.
- From 3200×1800 up (4K), the same layout is drawn with twice the pixels
  in each direction, as on a Retina display.

## 2. Getting the monitor's size in QEMU

QEMU's GTK window reports its own size as the GPU's preferred mode
(docs/gpu1-design.md). So from inside the guest a large monitor looks no
different from a small window.
- **The host says:** `xtask run` reads the primary monitor's size
  (`SetProcessDPIAware`, then `GetSystemMetrics`). On a monitor larger
  than 1920×1080 it passes that size to the guest:
  `-fw_cfg name=opt/zooos/display,string=WxH`. It also passes
  `-display gtk,zoom-to-fit=on`, so a maximised window fits, and full
  screen (Ctrl+Alt+F) maps one-to-one.
- **The kernel reads it:** `kernel/src/fw_cfg.rs` reads QEMU's
  configuration files through I/O ports 0x510 and 0x511. The virtio GPU
  driver uses the size, between 1920×1080 and 3840×2400 (`ZOO_OS INFO gpu
  host size WxH`). Without it, the old rule applies: the preferred mode,
  held between 1920×1080 and 1920×1200.
- **Safety:** fw_cfg is only read after a QEMU-only device (the virtio
  GPU) was found, so real hardware never sees those port accesses.

## 3. Drawing at 2x

The window server picks a scale of 2 when the screen is at least
3200×1800, and 1 otherwise. That covers a 4K firmware framebuffer on real
hardware too.
- **Logical layout:** the logical size is the device size ÷ the scale,
  at most 2560×1600 (raised from 1920×1200, so a 2560×1440 or 2560×1600
  screen is used whole; the `big-screen` case now checks that). All layout, window rectangles, damage requests, the
  pointer position and the tablet mapping stay in logical pixels.
- **Primitives:** every `Screen` drawing call takes logical coordinates
  and draws in device pixels.
  - Rounded corners, discs and shadows are computed at device
    resolution, so edges are as smooth as the 4K screen allows.
  - Outlines are one device pixel: hairlines at 2x.
  - Text uses new 26 px and 22 px Inter faces in the atlas
    (`FaceId::scaled`). Text widths used for layout are divided back to
    logical pixels.
- **Layers, damage, the back buffer and frames are device pixels.** The
  back buffer now comes from the heap, sized to the screen, instead of a
  fixed 1920×1200 static.
- **The pointer:** `pointer_image(2)` rasterises the arrow at 2x, for
  both the hardware cursor (hot spot ×2) and the software pointer.

**Results at 1x are unchanged.** The 1x paths do the same arithmetic as
before, and every existing case passes.

## 4. Not done

- **Following the window's size:** a resolution that tracks the QEMU
  window as it is resized (virtio-gpu display-change events) is not
  handled. The size is fixed at boot.
- **Other scales:** only 1 and 2. A 3000×2000 screen, for example, is
  drawn at 1x using its top-left 2560×1600.
- **The dock's sparkle icon** is drawn from one-pixel lines, which become
  2×2 blocks at 2x.

## 5. Measurements

Medians at `--accel whpx`, three runs, in cycles (`gpu-hidpi-frames`,
3840×2160 at 2x, against `gpu-frames` at 1x):

| Frame | 1x | 2x |
| --- | --- | --- |
| Window drag | 2,464,719–2,535,777 | 8,611,118–8,841,905 for 2,522,880 px |
| Full repaint | 5,064,211–5,283,560 | 22,756,000–23,098,433 for 8,294,400 px |
| One-line content update | 384,130–506,243 | 1,238,831–1,342,114 |

The 2x cost grows with the pixel count, about 3.5–4×. A 2x drag frame
(about 3 ms) still fits within the 10 ms pacing interval.
