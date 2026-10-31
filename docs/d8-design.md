# Desktop step 8 (D8): resize windows

Status: implemented 2026-10-31 (version 0.36.0).

## 1. Change

- Pressing in a window's bottom-right 12×12 corner and dragging resizes
  it, as on macOS; the top-left stays put. Windows stay at least 200×120
  and inside the screen (above the status line). Logs `resized <app> to
  WxH` on release.
- Content already clips to the window (lines that do not fit are not
  drawn; text is cut at the window's width), and the close button is
  placed from the right edge, so both follow the new size. Each step of
  the drag damages the old and new rectangles (D2).

## 2. Verification

`resize` case: Crash Test (604×340 at (652, 48)) dragged by its corner by
(−200, −100) → `resized Crash Test to 404x240`; screenshot: Crash Test
focused with its text, the close button at the new right edge (1042, 60)
and not at the old one (1242, 60), the desktop colour below the new
bottom edge.

Negative control: the corner never grabs → `FAIL resize` (timeout, no
`resized` line).

## 3. Not done

- Only the bottom-right corner (no edges or other corners), no visible
  grip, no minimise or full-screen.
- Apps are not told their window size; they keep sending the same lines.
