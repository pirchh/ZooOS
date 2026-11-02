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

- ~~A true genie warp~~: done in 0.60.0, §6.
- **Animated zoom and focus fades:** zooming and changes of focus are
  not animated yet.
- **Dock magnification and the launch bounce:** next in the polish pass.
- **The layer rebuild on refocus:** see §3.

## 6. The genie (0.60.0)

**The owner's feedback:** the animations "feel good but they don't look
as much like mac". The Mac minimise pinches the window toward the dock
and then pulls it in. U2d's minimise was a plain scale-and-move.

**`composite_genie` draws every row of the window body between a left
and a right edge:**
- **The edges:** S-curves (smoothstep) running from the window's width
  at its top to the dock icon's width at the icon's bottom.
- **First 40 %:** the curves bend in. The window's top stays put while
  its bottom pinches toward the icon.
- **The rest:** the body slides down the funnel, its bottom ahead of its
  top (compressing), and fades in the last 15 %.
- **Sampling:** rows are mapped linearly into the moving top and bottom,
  then sampled bilinearly along the row. The shadow is not drawn, only
  the body.

**Lengths and frames:**
- Minimise now takes 520 ms and restore 460 ms; restore is the same warp
  run backwards.
- Each frame damages the whole funnel's bounding box.

**Slow motion:** with Shift held, a new animation runs ten times slower,
as on a Mac. That is also how the `genie` case catches a frame early in
the first phase.

**Measured** (`animations`, `--accel whpx`, three runs):

| | Result |
| --- | --- |
| Minimise | 53 frames, 523 ms |
| Restore | 47–48 frames, 460–469 ms |
| Genie frames | median 6.7–7.8 M cycles for 799,112 px, about 2.5 ms; the longest 16.5–23.7 M, the refocus rebuild of §3 |

**Checked:**
- **By eye:** frames at 30 %, 60 % and 90 % of a slowed minimise show the
  pinch, then the slide down the funnel, then the narrow tail entering
  the dock icon.
- **The `genie` case:** early in a slow-motion minimise, the title bar
  must still span the window's top at both ends, and the bottom-left
  corner must show wallpaper.
- **Its negative control:** minimise drawn as the old shrink → `FAIL
  genie: the top edge moved`.
