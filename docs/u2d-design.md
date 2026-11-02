# UI step 2d (U2d): window animations

Status: implemented 2026-11-01 (version 0.59.0). This is the first step of
the owner's polish pass ("the massive piece is animations"). Windows now
open, close, minimise to the dock and come back with motion instead of
appearing and vanishing.

## 1. The clock

- **Timing:** animations are timed with the time-stamp counter, not the
  10 ms timer tick. At start the window server measures the counter over
  three ticks (30 ms) from a tick boundary (`calibrate_ms`), which gives
  cycles per millisecond.
- **Frames:** while an animation runs, the main loop treats the screen as
  changed. The frame pacing from U2a (at most one frame a tick) then gives
  about 100 frames a second. Each frame samples every animation at the
  same instant (`frame_now`).

## 2. The animations

Each animation moves the window's rectangle and its opacity from a start
to an end along an easing curve:

| Kind | From → to | Opacity | Length | Curve |
| --- | --- | --- | --- | --- |
| Open (dock, and the first windows at desktop start) | 92 % size, same centre → the window | 0 → 1 | 220 ms | ease-out (cubic) |
| Close | the window → 92 % | 1 → 0 | 180 ms | ease-in (cubic) |
| Minimise | the window → its dock icon, with the window's proportions | 1 → 0.25 | 320 ms | ease-in-out (cubic) |
| Restore | the dock icon → the window | 0.25 → 1 | 280 ms | ease-in-out (cubic) |

- **What is drawn:**
  - Closing and minimising windows are drawn from a snapshot of their
    layer, taken as the animation starts. The application may already
    have ended, and the window is no longer "visible" to anything else.
  - Opening and restoring windows use their live layer.
- **Drawing:** a layer is composited scaled and faded
  (`composite_scaled`).
  - Bilinear sampling in 16.16 fixed point.
  - Premultiplied pixels, two channels per 16-bit lane.
  - Outside the layer counts as transparent, so edges stay smooth as the
    window shrinks.
- **Damage:** each frame damages the area the animation covered last
  frame and the one it covers now.
- **The end:** a finished animation hands the window back to normal
  drawing, or to none, and logs `PeacockHandler: animation <kind>
  <window>: frames N time M ms`.

## 3. Measurements

From the `animations` case under `--accel whpx` (1920×1080):

| | Result |
| --- | --- |
| Close Crash Test | 19 frames, 180 ms |
| Minimise Files | 34 frames, 328 ms |
| Restore Files | 30 frames, 287 ms |
| Open Crash Test | 24 frames, 229 ms |
| Frames during minimise and restore (three runs) | median 0.95–1.41 M cycles, longest 17.5–23.3 M cycles |

**The longest frame is a one-off, not the animation itself:**
- **What it is:** restoring a window gives it focus, which rebuilds its
  layer (drawn twice, docs/u2-compositor-design.md).
- **Evidence:** a fast path that removed per-sample bounds tests from
  the resampling did not change it, so it was reverted.
- **Cost:** one hitch of about 6 ms per restore. The animation frames
  themselves have a median of about 0.4 ms.

A frame from the middle of a slowed-down minimise was checked by eye: the
window is scaled down and on its way to the dock, slightly translucent,
smoothly filtered, with its shadow.

## 4. Checks

- **The `animations` case:**
  - closes Crash Test, minimises Files, restores it from the dock and
    opens Crash Test from the dock;
  - requires each animation to report at least 3 frames, lasting about
    its length;
  - checks the final desktop: Crash Test focused, Files back in place.
- **Other screenshot cases** settle for 1.2 s before their screenshots,
  longer than any animation, and pass unchanged.
- **The negative control:** zero-length animations make `close Crash
  Test` report 1 frame, and the case fails.

## 5. Not done

- **A true genie warp:** minimise is a scale-and-move toward the dock,
  not a curved warp.
- **Animated zoom and focus fades:** zooming and changes of focus are
  not animated yet.
- **Dock magnification and the launch bounce:** next in the polish pass.
- **The layer rebuild on refocus:** see §3.
