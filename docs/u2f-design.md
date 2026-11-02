# UI step 2f (U2f): the ZooOS mark

Status: implemented 2026-11-01 (version 0.62.0). The owner liked the paw
but wanted it polished. Four directions were drawn for them (a soft paw,
a badge, geometric circles, a line paw), and they chose **A, the soft
paw**.

## 1. The shape

In a 100-unit design box:
- **The main pad:** a rounded shape of six cubic curves. It is wider at
  the top, with two slight lobes at the bottom.
- **Four oval toe beans:** at (23, 47), (38, 30), (62, 30) and (77, 47),
  with radii 7 × 9.5 or 7.5 × 10.5, tilted −25°, −8°, 8° and 25°. The
  outer toes tilt outward.
- **The drawn area:** the mark is drawn from the square (14, 18)–(86, 90),
  which holds it with a small margin.

## 2. Drawing it

- **The mask:** at start-up the window server makes the mark's coverage
  mask at the screen's scale (`paw_mask`): 18 logical pixels square, so 18
  or 36 device pixels.
  - Each pixel is sampled 4 × 4 times.
  - The main pad's curves are flattened to a 72-point polygon and tested
    even-odd.
  - The toes are tested as rotated ellipses.
- **The menu bar** blends the mask in its text colour at (9, 4), in both
  appearances.
- **No vector engine:** the mask is computed once; drawing it is one
  blend per pixel.

## 3. Checks

- **The `logo` case:** the middle of the main pad (design (50, 66),
  screen (18, 16)) must be in the menu-bar text colour, and the gap
  between the two upper toes (design (50, 30), screen (18, 7)) in the bar
  colour.
- **`gpu-hidpi`** checks the same two points doubled at 2x.
- **Negative control:** the old mark (discs) → `FAIL logo`, because it had
  a toe in that gap.
- **By eye:** a crop of the 2x menu bar.

## 4. Not done

- **The boot screen and an "About ZooOS" panel** (badge B was suggested
  there): no such screens exist yet.
