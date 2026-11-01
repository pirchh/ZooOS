# UI step 1 (U1): a macOS-style desktop

Status: implemented 2026-11-01 (version 0.54.0). The first UI milestone
after the L10 Wine track, at the owner's request ("mac based UI"). The
owner chose:

- the Inter font;
- both light and dark appearances, with a switch between them;
- a much higher resolution, with the windows given room instead of being
  stacked.

The behaviour of earlier desktop steps is kept: focus, drag, resize, zoom,
close, the dock, restarts, and redrawing only what changed (D1–D10).

## 1. Screen

QEMU's display now reports 1920×1080 as its preferred mode
(`-device VGA,xres=1920,yres=1080,edid=on`), and the firmware starts in it.

Two other approaches failed:
- Asking the bootloader for at least 1920×1080 made it take the largest
  mode, 2560×1600.
- Capping video memory at 8 MiB left no usable mode at all.

On real hardware the firmware's own mode is used, as before.

## 2. Text: Inter, rendered at build time

- **The atlas:** `xtask` renders Inter 4.1 (tools/fonts/README.md) with
  `fontdue` 0.9.3 into a bitmap atlas in the initrd (`ui/fonts.zfnt`):
  - faces: Regular, Medium and SemiBold at 13 px, Regular at 11 px;
  - characters: printable ASCII and a few UI symbols;
  - coverage raised to the power 0.82, for slightly heavier strokes.
- **No font engine in the OS.** The window server only blends coverage
  bytes.
- **Shared code:** the atlas format and the text layout (`Face::draw`)
  live in `zoo-abi` (abi/src/font.rs). The window server and the
  screenshot checks place every glyph with the same code, so a check
  reproduces the screen's pixels exactly.
- **No kerning.** Inter 4 keeps its kerning in the OpenType `GPOS` table,
  which fontdue does not read. Inter's spacing is made to read well
  without it.

## 3. The look

Geometry is in `zoo_abi::ui` (abi/src/ui.rs), shared with the tests:

- **Menu bar** (26 px):
  - left: a paw mark, the focused application's name in SemiBold, then
    File, Edit, View, Window and Help (labels only; they open in U4);
  - right: the appearance switch and the UTC clock.
- **Windows:**
  - 10 px rounded, anti-aliased corners, a 28 px title bar, a one-pixel
    outline, and a soft shadow (stronger for the focused window);
  - traffic lights: red closes, yellow minimises (the dock shows the
    window again), green zooms. They are grey in windows without focus;
  - centred titles in Medium;
  - content lines 22 px apart, the selected one a rounded blue bar.
- **Dock:** always shown at the bottom centre, translucent and rounded:
  - four drawn icons (a folder, an exclamation mark, a sparkle, a page);
  - a running dot under each running application;
  - an application's name above its icon while the pointer is on it.
- **Notifications:** the bottom status line is gone. Crashes and restarts
  show as a banner at the top right for six seconds, and are still logged.
- **Starting layout** at 1920×1080:
  - Files on the left (806×653);
  - the Assistant top right (768×408);
  - Crash Test below it (768×272);
  - Notes, opened from the dock, in the middle.

  The layout is proportional to the screen.
- **Appearance:** light (blue-to-lavender wallpaper, light windows) or dark
  (deep blue-to-violet, dark windows). Both palettes are in
  `zoo_abi::ui::{LIGHT, DARK}`. F8 or the menu bar's switch changes
  appearance; it is not saved yet.
- **Pointer:** a black arrow with a white edge.

## 4. Speed

- **Shadows:** a separable falloff table rather than per-pixel distances.
- **Rounded corners:** anti-aliased from squared distances, with no
  square roots.
- **Skipping:** every drawing primitive skips shapes outside the area
  being redrawn, and handles only the rows and columns inside it.

A first version did not skip them, and made a pointer move cost 25 times
more. Measured with `--accel whpx --case frames`, three runs each:

| Build | Full repaint | Pointer move (408 px) |
| --- | --- | --- |
| D2 (1280×800, docs/verification.md) | 4.0–4.3 M cycles, 1.0 M px | 31,158–31,718 cycles |
| U1 without skipping (1920×1080) | 9.8–10.7 M cycles, 2.1 M px | 751,112–962,589 cycles |
| U1 as merged | 9.4–9.9 M cycles, 2.1 M px | 59,149–62,200 cycles |

A full repaint costs about the same per pixel as before (4.5–4.8 against
4.0–4.2 cycles). A pointer move costs about twice what it did, at about
20 µs.

## 5. Not done (later UI steps)

- **U2, the dock:** magnification, launch animation, real icon artwork.
- **U3, applications:** a Finder-style Files window (sidebar, icons,
  columns); restyled Notes and Assistant.
- **U4, behaviour:** menus that open, keyboard shortcuts with a Command
  key, animated window moves, a saved appearance setting.
- **HiDPI:** 2× rendering for high-density screens, which needs 2× atlas
  faces.
- **Not drawn:** no blur behind translucent surfaces, no subpixel text
  positioning, no kerning.
