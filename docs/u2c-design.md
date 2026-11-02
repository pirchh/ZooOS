# UI step 2c (U2c): the pointer moves with a dragged window

Status: implemented 2026-11-01 (version 0.58.1). After U2b the owner
found dragging "much better", but the window still lagged a little behind
the pointer. On their Windows desktop a dragged window stays exactly
where it was grabbed.

## 1. Why a dragged window trails the pointer under QEMU

- **The pointer** is a hardware cursor (docs/gpu1-design.md). QEMU hands
  it to Windows, which moves it at once.
- **The window takes a longer path:**
  1. the tablet event goes through QEMU into ZooOS;
  2. the window server draws a frame (under 1 ms, after up to 10 ms of
     pacing);
  3. the frame is flushed to QEMU, which copies it into its window;
  4. Windows composites that window on its next refresh.

So the window trails the cursor by about one or two refreshes. Steps 3
and 4 belong to QEMU and Windows: no guest-side speed-up removes them.
Windows itself avoids the gap by moving the window and the cursor in the
same composition.

## 2. The change

- **During a drag or resize:** the window server hides the hardware
  cursor (an empty cursor image) and draws the arrow itself in the frame,
  with the window. Both reach the screen together, so the window stays
  under the pointer.
- **When the button is released:** the arrow is erased from the frame
  and the hardware cursor is shown again at the same spot.
- **No gap without a pointer:** the switch happens right after the frame
  that adds or removes the drawn arrow.
- **The log:** `PeacockHandler: pointer drawn in the frame` and
  `PeacockHandler: pointer hardware`.

**The trade-off:** during a drag the pointer is as late as the window
instead of ahead of it. VMs usually make the same choice. At all other
times the pointer stays instant. On real hardware no QEMU stands in
between, so the gap does not exist there.

## 3. Checked by

The `gpu-drag-cursor` case drags Files with the tablet.
- A screenshot taken mid-drag must show the arrow drawn at the pointer.
- One taken after release must not; a hardware cursor is not part of a
  screenshot.

**Found along the way:** two 16 KiB cursor images built on the window
server's stack overflowed it when the first drag started. They are now
on the heap.
