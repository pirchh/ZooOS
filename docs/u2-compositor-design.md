# UI step 2a (U2a): a compositor in the window server

Status: implemented 2026-11-01 (version 0.57.0). After GPU1 the owner said
the desktop still did not feel native, and asked for the most efficient
approach, measured.

## 1. The problem, measured

The window server drew every window from scratch into whatever area
changed: shadows, rounded corners and anti-aliased text.
- **Dragging a window** damaged its old and new areas. Together they were
  over half the screen, so every drag frame became a full repaint:
  7.7–8.0 M cycles on standard VGA and 9.8–10.0 M on the virtio GPU
  (`frames` and `gpu-frames` cases, WHPX, three runs each).
- **Moving a software pointer** redrew whatever lay under it, about
  60,000 cycles.

## 2. The design

**Window layers.** Each window keeps a cached image of itself and its
shadow, from (x − MARGIN, y − MARGIN), with alpha.
- **Building:** the window is drawn twice with the existing drawing
  code, over black and over white. The two results give each pixel's
  premultiplied colour and its alpha: alpha = 255 − (white − black).
  Shadows, rounded corners and outlines keep exact translucency, and no
  second drawing path exists.
- **Opaque spans:** per row, the first run of fully opaque pixels is
  recorded. Compositing copies those and blends only the rest.

**Compositing.** The screen is the wallpaper, then each visible layer in
stacking order, then the menu bar, banner and dock as before.
- Blending uses premultiplied "over", two channels at once in 16-bit
  lanes.
- **No carries:** `x + 1 + (x >> 8) >> 8` is ⌊x / 255⌋ for every product
  of two bytes, and each layer channel is clamped to its alpha when the
  layer is built.

**What invalidates what:**

| Change | Work |
| --- | --- |
| A window moves | Nothing rebuilt; old and new areas composited |
| Focus, appearance, title, size, the app ends | Layer rebuilt |
| An app's lines or selection change | Only those lines redrawn, in place, in the layer (one pass; only pixels already opaque are replaced) |
| A live resize | The window drawn directly each frame; its layer rebuilt when the resize ends |
| F10 (test key) | Every layer rebuilt, so the `redraw` checks compare against drawing from scratch |

**Supporting changes:**
- **Damage merging:** overlapping damage rectangles are merged when
  their union costs no more than both. A window moving a few pixels
  becomes one area instead of two, and so no longer a full repaint.
- **Line tracking:** applications send their title and every line with
  each update. The window server now compares them and records which
  lines changed, and the selection's old and new lines. Before, any
  update counted as a title change.
- **Drawing directly into GPU memory:** on the virtio GPU, the display
  memory is ordinary cached RAM that is shown only when flushed. The
  window server draws straight into it, with no back buffer and no copy.
  The firmware framebuffer path keeps its back buffer.
- **The frame report:** it now gives the cycles spent showing each frame
  (the copy or the flush) separately.

## 3. Results

Medians from `--accel whpx`, three runs each, cycles per frame:

| Frame | Before | VGA | virtio GPU |
| --- | --- | --- | --- |
| Drag a window (30 moves) | 7,674,908–7,974,669 (VGA), 9,836,683–9,983,554 (GPU), full screen | 2,297,288–2,313,467 for 630,720 px | 2,464,719–2,535,777 |
| Pointer move | about 60,000 | 14,871–16,313 | none (hardware cursor) |
| Full repaint (2,073,600 px) | 8.5–10.0 M | 5,594,222–5,664,185 | 5,064,211–5,283,560 |
| Move Files' selection by one line | about 4 M (whole content area) | 270,184–289,975 for 35,376 px (two lines) | 384,130–506,243 |

- **Where a drag frame's time goes** (a temporary probe, not kept):
  about 0.5–0.6 M filling the wallpaper, 0.9–1.3 M compositing, and
  0.7 M showing. That is roughly one cycle per byte moved. The work is
  bound by memory bandwidth, not arithmetic.
- **Frame pacing** (drawing at most once per display refresh) was left
  out deliberately. Every frame above takes under a millisecond, so
  holding frames back for a refresh would only add delay.

## 4. Not done

- **Wallpaper overdraw:** wallpaper is still filled under opaque windows
  (about 0.5 M cycles per drag frame).
- **Animations:** window open and close, minimise, dock magnification and
  menus are the next UI step.
- **2× (Retina-style) rendering:** for the owner's 4K monitor; QEMU
  currently scales the 1920×1080 picture up.
- **Per-application surfaces:** applications send lines of text. A
  shared-memory surface per app would let apps draw their own content.
