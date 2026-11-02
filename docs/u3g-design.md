# UI step 3g (U3g): icons with depth

Status: implemented 2026-11-02 (version 0.64.0). The owner asked for
"better looking almost 3d looking icons for the taskbar and some of the
apps", with an Apple feel.

## 1. The look

Each app icon (`app_icon`) is built in layers, as macOS icons are:
- **The tile:** a rounded square (radius 22.5 % of its size) filled with
  a vertical gradient, lighter at the top, as if lit from above:

  | App | Top | Bottom | Symbol |
  | --- | --- | --- | --- |
  | Files | `#6CC4FF` | `#1F7AE8` | a folder, its front lighter than its back |
  | Crash Test | `#B9B9C0` | `#6C6C73` | a yellow warning sign with a dark `!` |
  | Assistant | `#B497FF` | `#5A36E0` | two four-pointed sparkles |
  | Notes | white | `#ECECEF` | a yellow band at the top, ruled lines |

- **Rims:** a bright line along the top edge (white, 35 %) and a darker
  one along the bottom (black, 20 %), which give the tile its thickness.
- **A soft shadow** under the tile: three faint, widening layers, a little
  lower than the tile.
- **The symbol has its own shadow:** the same shape in black at 27 %,
  moved down by a pixel at dock size.

## 2. Drawing it

- **Shapes** are tests of whether a point is inside (`rr`, `circle`,
  `sparkle`, half-planes). `Screen::shade` fills them with a gradient,
  sampling 4 × 4 per device pixel, so edges are anti-aliased at 1x and
  2x.
- **Cached:** the shapes are costly, and the dock is drawn in every frame
  of its slide. So each dock icon is drawn once per screen scale:
  - over black and over white, kept premultiplied, as window layers are
    (`build_icons`);
  - then composited with `over` (`draw_icon`);
  - logged as `dock icons drawn at 1x`.
- **The same artwork at 16 px** is the app's icon in Files' Applications
  folder.
- **Minimised-window badges** keep their flat colours.

## 3. Checks

- `apps-folder` and `dock-menu` find Notes by its yellow band (a row 15 %
  down the icon). No other icon is yellow on that row. Crash Test's sign
  starts lower.
- `finder` checks the Files list's folder icon.
- Screenshots of the dock at 1x were looked at by eye.

## 4. Not done

- Magnification on hover, and the launch bounce (U2, proposed).
