# UI step 3 (U3): windows, the dock's menus, and the Applications folder

Status: implemented 2026-11-01 (version 0.63.0). The owner asked for:
- right-click on dock icons, to remove apps from the dock;
- a place (Files) to right-click an app and put it back in the dock;
- several windows per app, as on a Mac;
- minimised windows on the right of the dock, after a divider.

## 1. Windows and applications (U3a)

- **Window slots are separate from applications.** `WINDOWS` = 8 slots.
  The first `PROGRAMS` (4) are each app's first window, in `CATALOG`
  order. Slots 4–7 hold further windows.
- **Each window is its own process.** A second Files window is a second
  `files` process, with its own state. Nothing is shared between them
  except the home folder.
- **Ctrl+N** opens another window of the focused window's app, cascaded
  32 px down and right of it, and focuses it. With all eight slots used it
  logs `no room for another window`.
- **A dock icon** raises the app's frontmost window, else restores a
  minimised one, else opens its first window.

## 2. The dock's menus (U3b)

- **A right-click on a dock icon** opens a menu above it:
  - a running app: New Window, Remove from Dock (or Keep in Dock), Quit;
  - an app that is not running: Open, Remove from Dock (or Keep in Dock).
- **The dock lists kept apps and running apps.** An app removed from the
  dock still shows while it runs, then leaves when its last window closes.
- **Menus** are drawn by the window server: rounded, in the tooltip
  colours, the item under the pointer highlighted. Esc or a click
  elsewhere closes them.

## 3. Minimised windows (U3c)

- **After a divider** on the right of the dock, each minimised window has
  its own thumbnail: the window's cached layer scaled into the slot, with
  a small badge in its app's colour.
- **The genie** goes into that thumbnail and comes back out of it.
- **A click on a thumbnail** restores that window.

## 4. Applications in Files, and app menus (U3d)

- **A small protocol** between apps and the window server, so apps can
  have their own menus:

  | Message | Direction | Meaning |
  | --- | --- | --- |
  | `rclick:<line>` | server → app | right button on content line `<line>` |
  | `menu:A\|B\|…` | app → server | show a menu of these items at the pointer |
  | `menu-choose:<n>` | server → app | item `n` of that menu was chosen |
  | `dock-add:<program>` | app → server | keep this app in the dock |
  | `launch:<program>` | app → server | open this app (as a dock click) |

  The app API is `Window::menu`, `Window::dock_add`, `Window::launch` and
  `Event::parse` (`user/src/app.rs`). A menu request is ignored unless the
  app's window has the focus.
- **The Applications folder** is the last entry of Home in Files. It is
  not on disk: Files lists `zoo_abi::ui::APPLICATIONS`.
  - Enter opens the selected app.
  - A right-click gives **Open** and **Add to Dock**.
- It sits last so that the positions of the real folders stay as before.

## 5. Saved pins (U3e)

- **The kept apps are saved** in `.dock` in the home folder, as program
  names separated by commas (`files,crash-test,assistant`), on every
  change. The window server logs `dock saved (N kept)`.
- **At start-up** it reads `.dock` if it exists (`dock loaded (N kept)`).
  With no file, every app is kept.
- Files hides dot-files, so `.dock` does not show in Home.

## 6. Checks

| Case | What it shows |
| --- | --- |
| `windows` | Ctrl+N: a second Files window 32 px down and right, focused; closing it leaves the first |
| `dock-menu` | Files' menu (New Window highlighted); New Window opens window 4; Remove from Dock takes Notes out (three-icon dock) |
| `dock-minis` | two minimised windows after the divider, each with its badge; a thumbnail click restores Files |
| `apps-folder` | Remove Notes; Files → Applications; right-click Notes → Add to Dock puts it back; remove again, restart the desktop: the dock loads without Notes |
| `animations` | restore now goes through the window's thumbnail |

Negative controls:
- **`windows`:** a Ctrl+N that does nothing → `FAIL`.
- **`apps-folder`:** a loader that ignores `.dock` → `FAIL` (it logs
  `dock loaded (4 kept)`, and the case waits for `3 kept`).
- **`dock-menu` and `dock-minis`:** see `docs/verification.md`.

## 7. Not done

- **Dragging** icons into or out of the dock, and reordering it.
- **Files** is still a plain list. A Finder-style sidebar and icons come
  next.
- **Window menus** in the menu bar (Window → list of windows).
