# UI step 2e (U2e): the dock hides itself

Status: implemented 2026-11-01 (version 0.61.0). The owner noticed that
the dock "does not go away" and chose auto-hide, as in macOS's
"Automatically hide and show the Dock".

## 1. Behaviour

- **At start:** the dock is hidden below the screen's bottom edge.
- **Revealing it:** the pointer near the bottom edge (the last six
  logical rows, `DOCK_EDGE`) slides it in over 250 ms, slowing as it
  arrives.
  - It was the last two rows until 0.61.1. The owner found the dock would
    not come back in a maximised QEMU window, whose scaling stops the
    pointer a few rows short of the last one.
- **While it is in use:** it stays while the pointer is over it, or over
  the label above its icons.
- **Leaving it:** 400 ms after the pointer leaves, it slides out over
  300 ms, speeding away.
- **During a genie:** it also shows while a window genies into it or out
  of it, since the animation ends at its icon.
- **Icons:** they take clicks only while the dock is fully shown.
- **The log:** `PeacockHandler: dock shown` / `dock hidden` when a slide
  ends.

## 2. Implementation

- **State in the window server:**
  - the offset below its place (`dock_off`, 0 to `DOCK_HIDDEN`, which is
    the dock's height and bottom gap plus room for its outline);
  - whether it is meant to be shown;
  - the slide (from, to, start, length);
  - the time it hides.
- **`update_dock`:** runs every frame, from the frame clock of
  docs/u2d-design.md. While a slide or a pending hide exists, the main
  loop keeps producing frames.
- **Damage:** a slide damages the dock's column from its label down to
  the bottom edge.
- **Drawing:** the dock, its icons, running dots and labels are drawn
  shifted down by the offset.
- **Zoomed windows** keep their size. They still leave room for the dock.

## 3. Checks

- **`dock-autohide`:**
  - **At start:** wallpaper where the dock would be.
  - **Pointer at the bottom edge:** `dock shown`, with the running dots
    under the first three apps.
  - **Pointer moved away:** `dock hidden`, and wallpaper again.
- **Every case that clicks a dock icon** first moves the pointer to the
  bottom edge and waits 500 ms for the slide (`DOCK_SLIDE`).
- **The `dock` case:**
  - it checks the running dot under the reopened Crash Test while the
    dock is still up (`4-dock-reopened`);
  - after the pointer leaves, it checks that the dock has hidden
    (`5-reopened`).
- **Negative control:** a dock that never hides → `FAIL dock-autohide`,
  never seeing `dock hidden`.

## 4. Not done

- **No setting:** auto-hide is always on; there is no settings app yet.
- **No magnification or launch bounce:** next in the polish pass.
