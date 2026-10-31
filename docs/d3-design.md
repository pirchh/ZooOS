# Desktop step 3 (D3): a dock and close buttons

Status: implemented 2026-10-31 (version 0.23.0); §4 records the
implementation. Follows D2. Towards the
owner's macOS-like desktop: applications are opened and closed by the
user, not fixed on screen.

## 1. Close button

- Each window's title bar gets a round red close button at its right end
  (12 px, centred vertically, 8 px from the edge). macOS puts it at the
  left; the right end keeps every title where the existing screen checks
  expect it.
- Clicking it closes the application: PeacockHandler closes the app's
  channel, the app sees its channel end and exits normally, and
  PeacockHandler waits for it (bounded, as in `app_ended`). A closed
  application is not restarted (unlike a crash) and its window is hidden.
- Focus moves to the topmost remaining window; with none, the menu bar
  shows only `ZooOS`.

## 2. Dock

- A bar centred above the status line with one tile per application
  (Files, Crash Test, Assistant): a rounded square with the app's initial,
  and its name under it. A small dot under a tile marks a running app.
- Clicking a tile of a closed app starts it (fresh restart count) and
  shows its window at its last position, focused. Clicking a running app's
  tile focuses and raises its window.
- **Shown on demand**, like macOS's auto-hiding dock: it appears while
  the pointer is in the bottom 80 px of the screen (above the status line)
  and hides when the pointer leaves. It is drawn above windows, below the
  pointer. A permanent dock would cover the bottom of the Assistant window
  in the start layout; hiding it keeps the full screen for windows.

## 3. Verification

Case `dock` (mouse via QMP):

1. Desktop starts: no dock. Move the pointer to the bottom: the dock
   shows three tiles with running dots (screenshot).
2. Click Crash Test's close button: `ZOO_OS PROC exit ... name=crash-test
   code=0`, log `closed Crash Test`, no restart; the `Crash Test` title is
   gone, its dock dot is gone (screenshot).
3. Click the Crash Test tile: a new `crash-test` process starts; its window
   is back and focused (screenshot).

Existing desktop/mouse/redraw checks keep passing: the window layout is
unchanged and the dock is hidden while the pointer is elsewhere.

Negative control: closing restarts the app as a crash does (no `closed`
state); step 2's "title gone" check must fail.

## 4. As implemented

- Close button: a red disc at the right end of each title bar. Closing
  closes the app's channel and waits up to 100 ticks for it to end (then
  kills it), so a stuck application cannot hang the window server. Logs
  `closed <app>`. Focus moves to the topmost open window; Tab skips
  closed ones; the menu bar shows no window name when the focused one is
  closed.
- Dock: 3 slots of 96 px, 48 px coloured tiles with the app's initial,
  the name below, a 4 px dot for running apps. Shown while the pointer is
  within 8 px of the panel's top or below it. Clicking a closed app's tile
  starts it (logs `opened <app>`) and focuses it; a running app's tile
  focuses and raises its window. The dock is a D2 damage area.
- Case `dock` as in §3. The negative control was rejected by a timeout
  rather than by the step 2 screen check: with closing restarting the app,
  the dock click finds a running app, so `opened Crash Test` never appears
  and the case cannot reach its screenshots' checks.
