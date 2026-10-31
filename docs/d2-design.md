# Desktop step 2 (D2): redraw only what changed

Status: implemented 2026-10-31 (version 0.22.0); §5 records the
implementation and the measurements. Follows D1 (the mouse).
Towards the owner's goal of a fast, macOS-like desktop.

## 1. Problem

PeacockHandler redraws the whole screen for every change, then copies the
whole back buffer (1280×800×4 bytes = 4 MB) to the framebuffer. Moving the
pointer by one pixel costs as much as a full repaint. The clock redraws
everything once a second.

## 2. Change

- **Damage rectangles.** Each change records the screen area it affects:
  the pointer's old and new boxes; a window's old and new rectangles when
  dragged; one window when its application sends new lines; the menu bar
  for the clock and focus; the status line. Up to 8 rectangles per frame;
  more, or a total larger than half the screen, becomes one full-screen
  rectangle.
- **Clipped drawing.** `render` takes a clip rectangle; fills and glyphs
  outside it are skipped. Each damaged rectangle is redrawn back to front
  (desktop, bars, windows in order, pointer), clipped to that rectangle.
- **Partial present.** Only the damaged rectangles are copied to the
  framebuffer.
- Full repaints remain for start-up and anything not tracked.

## 3. Measurement

Time is measured in CPU cycles with `rdtsc` (allowed in user mode), around
`render` + `present`, per frame, and logged as `PeacockHandler: frame
<cycles> cycles <pixels> px` when a test key asks for it, so the normal log
stays quiet. Reported numbers come from a WHPX run (TCG cycle counts are
not meaningful), as the median of the frames during a scripted pointer
movement, before and after the change, on the same machine. No claim is
made without both numbers.

## 4. Verification

- All existing desktop, assistant and mouse screen checks must still pass
  (the picture is unchanged).
- A new check: after a pointer move, the screenshot equals a full repaint
  (a test key forces a full repaint; the two screenshots must be identical
  pixel for pixel).
- Negative control: forget the pointer's old box in the damage list; the
  old arrow stays on screen and the comparison must fail.

## 5. As implemented

- `Screen` has a clip rectangle; `fill`, glyphs and the pointer draw only
  inside it (glyphs wholly outside are skipped). `present` copies one
  rectangle.
- Damage sources: the pointer's old and new 12×17 boxes; a window and its
  border when its application shows new content, when it is dragged (old
  and new position), when it ends, and when focus changes (the old and new
  focused windows and the menu bar); the menu bar once a second (clock);
  the status line. More than 8 rectangles, or more than half the screen in
  total, becomes one full repaint.
- Test keys: F9 toggles a full repaint every frame (the behaviour before
  D2, for measuring); F10 forces one full repaint; F11 logs `frames <n>
  mode <full|damage> median <cycles> cycles <pixels> px` for the frames
  since the last F11.
- Cases `redraw` (pixel comparison, menu bar excluded because its clock
  may tick between the two screenshots) and `frames` (timing).
- The comparison only covers partial redraws after the last full repaint.
  The first version of `redraw` moved the pointer before a focus change,
  whose full repaint hid the negative control's stale arrows; the pointer
  now moves last.

**Measured** (`cargo xtask verify --skip-model --accel whpx --case frames`,
three runs on the development PC under QEMU/WHPX, 1280×800; median over
about 42 pointer-move frames each):

| Run | Full repaint | Partial redraw |
| --- | --- | --- |
| 1 | 4,342,631 cycles, 1,024,000 px | 31,610 cycles, 408 px |
| 2 | 4,039,118 cycles, 1,024,000 px | 31,158 cycles, 408 px |
| 3 | 4,176,248 cycles, 1,024,000 px | 31,718 cycles, 408 px |

A pointer-move frame costs about 130 times fewer cycles. This covers
pointer movement only; focus changes and dragging large windows still
repaint most of the screen. Cycles are `rdtsc` counts inside a virtual
machine; they compare the two modes on the same run, not real hardware.
