# UI step 3f (U3f): Finder-style Files, and Notes as a code editor

Status: implemented 2026-11-02 (version 0.64.0). The owner asked for:
- a folder browser like Finder, with good icons;
- a text editor with coloured text;
- saving to disk, and making folders.

## 1. Rich content lines

Apps still send lines of text. A few control characters in a line now
mean something to the window server (`zoo_abi::ui::mark`):

| Character | Meaning |
| --- | --- |
| `\u{1}` + role | the text after it in that role's colour |
| `\u{2}` + icon | at the start of a line: a 16-px icon |
| `\u{3}` | the text cursor: a 2-px accent bar, with 4 px of room |
| `\t` | the rest of the line right-aligned and dim (a column) |

- **Roles:** `0` text, `k` keyword, `s` string, `c` comment, `n` number,
  `t` type, `f` function, `m` macro, `d` dim.
  - The colours are Xcode's default light and dark themes
    (`Palette::syntax`).
  - A selected line draws all of its text in the selection colour.
- **Icons:**
  - `F` folder, `D` document, `C` code document;
  - sidebar symbols `H` home, `O` documents, `M` music, `P` photos,
    `A` applications;
  - `0`–`3` an app's icon.

  Each is drawn from a small shape description (`Screen::shape`): a test
  of whether a point is inside, sampled 4 × 4 per device pixel. So icons
  are anti-aliased and sharp at 1x and 2x, with no image files.
- **A content line holds up to 240 bytes** (it was 80), to leave room for
  the markup.

## 2. Sidebars and clicks

- **A sidebar:** `side:<k>:<text>`, `sides:<n>` and `side-sel:<k>` give a
  window a sidebar (`SIDEBAR_W` = 180), drawn with the window's frame.
  - Its items use the same markup.
  - The selected item has a grey rounded highlight, as in Finder.
  - Content lines move right of it.
- **Clicks now reach apps:**

  | Message | When |
  | --- | --- |
  | `click:<k>` | a click on content line `k` |
  | `dclick:<k>` | the second click of a double-click on the same line |
  | `sclick:<k>` | a click on sidebar item `k` |
  | `rclick:none` | a right-click in the content below the lines |

  Apps that ignore them (Crash Test, the Assistant) are unchanged.
- **Ctrl+letter** is now sent to the app as `key:ctrl:<c>` (a capital
  letter with Shift). The window server keeps Ctrl+N.
- **`open:<program>:<path>`** from an app opens the file in a window of
  that app:
  - its first window if the app is not running, else a new one;
  - the window then gets `open:<path>`.

## 3. Files

- **The sidebar:** Favourites, then Home, Documents, Music, Photos and
  Applications, each with its symbol. The current place is highlighted.
- **Each entry has an icon and a right column:**
  - folders: a blue folder, `Folder`;
  - files: a page (coloured lines for program files), and the size in
    Finder's style (`34 bytes`, `2 KB`, `1.2 MB`);
  - apps: their icon, `Application`.
- **Folders no longer end in `/`.**
- **Opening:**
  - a double-click (or Ctrl+O) opens a folder, an app, or a file in
    Notes;
  - Enter on a file still shows its first lines (a quick look);
  - Backspace goes up.
- **Right-click menus:**

  | On | Menu |
  | --- | --- |
  | a file | Open in Notes, Quick Look |
  | a folder | Open, New Folder |
  | an app | Open, Add to Dock |
  | empty space | New Folder, New Text File |

  - **New Folder** (also Ctrl+Shift+N) makes `untitled folder`
    (`untitled folder 2`, …) on disk and selects it.
  - **New Text File** makes `untitled.txt` and opens it in Notes.
- **The title stays `Files`.** The first line still shows the location
  (`Home/Documents`).

## 4. Notes, the editor

- **Opening files:** Notes opens any file it is sent (`open:<path>`). It
  saves the file it had first, if that file was changed. The title is the
  file's name; it is `Notes` for the default `Documents/notes.txt`.
- **Colours, by file extension:**
  - Rust, C, Python, shell, JavaScript, TOML and JSON;
  - Markdown: headings, code spans and bullets.
- **What is coloured:**
  - keywords;
  - strings;
  - line and block comments;
  - numbers;
  - types: capitalised names, and Rust's built-in types;
  - functions: a name before `(`;
  - Rust macros (`name!`) and attributes, and C's `#` lines.
- **Moving the cursor:** Up and Down move by a shown line, keeping the
  column. The view scrolls to keep the cursor in sight.
- **Saving:**
  - Ctrl+S saves at once;
  - otherwise Notes still saves a second after typing stops, and when the
    window closes.
- **The cursor is drawn as a bar** instead of `|` or `_` in the text.
- **Files up to 8 KiB** (was 4 KiB).

The text is drawn in Inter, not a monospaced font: a monospaced font would
need a new download, which needs the owner's approval.

## 5. Checks

- **New case `finder`:**
  - the sidebar, with Home selected, a folder icon, and the `Folder`
    column;
  - a sidebar click goes to Documents;
  - a double-click opens `Documents/zoo.rs` (a new sample file in the
    seed) in Notes;
  - keywords, strings, comments, types, macros and functions are each
    checked in their colour, and `struct` is checked not to be in the
    text colour;
  - Ctrl+S logs `Notes: saved 363 bytes`;
  - a right-click → New Folder makes and selects `untitled folder`.
- **Updated checks:**
  - folder names without `/`;
  - `README.txt` and its `34 bytes` checked separately;
  - `notes-cursor` finds the accent bar between `aX` and `bc`.

## 6. Not done

- **Renaming, deleting and moving files**, and drag and drop.
- **Column, icon and gallery views.**
- **A monospaced font**, needing a download.
- **Selecting text, copy and paste.**
