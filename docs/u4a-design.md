# UI step 4a (U4a): menus that open from the menu bar

Status: implemented 2026-11-02 (version 0.70.0). Until now the menu bar's
titles (File, Edit, View, Window, Help) were only words. They now open
menus, as on a Mac.

## 1. The menus

| Title | Items |
| --- | --- |
| The ZooOS mark | About ZooOS; Dark (or Light) Appearance |
| The focused app's name | Quit *app* |
| File | New Window (Ctrl+N), Close Window (Ctrl+W) |
| Edit | Undo, Cut, Copy, Paste: shown dim (not yet implemented) |
| View | Dark (or Light) Appearance |
| Window | Minimize (Ctrl+M), Zoom; then every open window, a ✓ on the focused one, "(minimized)" after minimised ones; choosing one brings it forward (restoring it) |
| Help | Keyboard Shortcuts |

- **About** and **Keyboard Shortcuts** show a notification banner.
- **Without a window** (only the desktop), items that need one are dim.

## 2. Behaviour

- **Opening and closing:** a click on a title opens its menu. A second
  click on it, a click elsewhere, or Esc closes it.
- **Sliding across:** with a menu open, moving the pointer along the bar
  switches menus, as on a Mac.
- **The open title** has a soft rounded highlight in the bar.
- **Shortcut hints** sit right-aligned and dim; selected, they turn
  white.
- **Check marks** are drawn in their own column.
- **Ctrl+W and Ctrl+M** are window-server shortcuts now: they close and
  minimise the focused window (Ctrl+N already opened one). Other
  Ctrl+letters still go to the app.
- **The menus are the same `Menu` as the dock's and apps' context menus**,
  with up to 16 items (was 6), a `bar` field for the title they hang
  from, and dim (`Disabled`) items that cannot be chosen.

## 3. Checks

- **`menu-bar`:**
  1. Click Window → ``menu bar Window: Minimize, Zoom, Files, Crash Test,
     Assistant``. The screenshot shows `Minimize` with `Ctrl+M` on its
     line, `Crash Test`, and `Assistant` highlighted under the pointer.
  2. Choose Assistant → `brought Assistant forward`.
  3. Click File, then slide to View → the View menu opens.
  4. Choose Dark Appearance → `appearance dark`.
  5. Ctrl+M → the Assistant minimises.
  6. The screenshot shows the dark appearance, the Assistant gone and
     Files focused.
- **Negative control:** titles that ignore clicks → `FAIL menu-bar`
  (timeout).

**Two first-run lessons:**
- **The serial log is ASCII,** so the ✓ printed as `???`. The log leaves
  it out.
- **Labels indented with spaces** land at fractional positions (Inter's
  space is not a whole pixel), which the screenshot check cannot match.
  The ✓ got its own column, as macOS draws it.

## 4. Not done

- **Edit's items:** there is no clipboard yet.
- **Keyboard navigation of menus** (arrows, Enter).
- **Apps' own menu-bar menus** (each app adding its own).
