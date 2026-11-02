# UI step 4b (U4b): a kept appearance, and word wrap in Notes

Status: implemented 2026-11-02 (version 0.71.0). Two small things that
make the desktop feel settled.

## 1. The appearance is kept

- **When the user switches the appearance,** it is written to
  `.appearance` in the home folder (`dark` or `light`). This applies to
  the switch in the menu bar, View or the ZooOS menu, and F8. The window
  server logs `appearance saved`.
- **At start-up,** after the dock's pins, the window server reads it and
  starts dark if it says so.
- **Only a user's switch writes the file;** start-up does not.
- Files hides dot-files, so `.appearance` does not show in Home.

## 2. Notes wraps between words

- A shown line still holds at most 72 characters, but it now breaks after
  the last space in them when there is one. A word longer than the whole
  line is still cut.
- The space stays at the end of the first piece, so cursor positions and
  Up/Down work as before.

## 3. Checks

- **`appearance-saved`:**
  1. F8 → `appearance saved`;
  2. Esc, then `desktop` again → the new window server logs
     `appearance dark`;
  3. the screenshot shows Files' title in dark colours.

  Negative control: the file never read → `FAIL appearance-saved`
  (timeout).
- **`finder`'s code screenshot** now also checks `"Badger"`. It is on the
  long line of `zoo.rs`, which used to break inside the word (`"Badg` |
  `er"`) and now breaks before it. Negative control: the old wrapping →
  ``FAIL finder: … string after a word wrap: `"Badger"` not on screen``.
