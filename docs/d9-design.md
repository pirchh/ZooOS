# Desktop step 9 (D9): double-click a title bar to zoom

Status: implemented 2026-10-31 (version 0.37.0).

## 1. Change

- Two presses on a window's title bar within 50 ticks (0.5 s) and 4 pixels
  of each other zoom the window: it fills the space between the menu bar
  and the status line (with the usual margin). Doing it again restores the
  size and place it had before. Logs `zoomed <app>` / `restored <app>`.
- A double-click does not start a drag. Each window remembers its own
  size before zooming.

## 2. Verification

`zoom` case: double-click the Assistant's title (900, 424) → `zoomed
Assistant`; screenshot: its title at (32, 52), Files' and Crash Test's
titles covered, the close button at (1242, 60). Double-click its title at
the top → `restored Assistant`; screenshot: title back at (660, 416),
Files visible again.

Negative control: double-clicks never detected → `FAIL zoom` (timeout, no
`zoomed` line).

## 3. Not done

- The double-click time is fixed; no animation; no full-screen mode
  without the bars.
