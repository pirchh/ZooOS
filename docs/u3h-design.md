# UI step 3h (U3h): Rename and the Trash in Files

Status: implemented 2026-11-02 (version 0.72.0). Files could make
folders and text files; now it can rename them and throw them away, as
Finder does, without losing anything by mistake.

## 1. Rename

- **Starting:** right-click a file or folder, then **Rename**. Its line
  becomes the name being typed: a folder icon, the name and a cursor,
  starting from the current name.
- **Typing:** Backspace deletes, and Enter renames on disk
  (`zoo_rt::rename`) and selects the result.
- **Refused:** `/` and an empty name are not accepted, and neither is a
  name starting with `.` (it would be hidden).
- **Logs:** `Files: renaming <name>`, then `Files: renamed <from> to <to>`.

## 2. The Trash

- **Move to Trash** (right-click) moves the item into `.Trash` in the
  home folder. That is a rename, so nothing is deleted. A clash gets a
  free name (`name 2`, …).
- **The sidebar's new Trash place** (with a bin symbol) lists it. The
  location line reads `Trash`, not `Home/.Trash`.
- **In the Trash,** right-click gives **Put Back**, which moves the item
  to Home.
- **Emptying the Trash** (deleting for good) is not done yet. Nothing
  Files does can lose data.
- **The Applications folder's own entry** offers only Open.

## 3. Checks

- **`files-manage`:**
  1. Documents → right-click `notes.txt` → Rename;
  2. wait for `renaming notes.txt`, then 9 × Backspace and `shopping.txt`
     + Enter → `renamed Documents/notes.txt to Documents/shopping.txt`;
  3. right-click `recipes` → Move to Trash → `moved Documents/recipes to
     the Trash`;
  4. the sidebar's Trash: the screenshot shows the location `Trash` and
     `recipes` selected;
  5. Put Back → `put recipes back in Home`.
- **Negative control:** Move to Trash that does nothing → `FAIL
  files-manage`.
- **A test race found and fixed in the test.** The first run's keys
  reached Files before the menu's answer, because the window server
  handles the keyboard before the mouse in each pass. The first Backspace
  went up a folder, and the rename hit `Documents`. A person cannot type
  that fast after a click, so the test now waits for `renaming …` before
  typing.
