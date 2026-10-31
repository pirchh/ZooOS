# Desktop step 4 (D4): Notes, a text editor whose work survives a reboot

Status: implemented 2026-10-31 (version 0.25.0); §4 records the
implementation. Follows D3 (dock) and S5
(persistent files): the first application where a person's typing is
kept on disk.

## 1. The application

- `notes` (window title `Notes`), started from the dock, closed at first.
  It gets a home-folder handle (like the Assistant) and edits one file,
  `Documents/notes.txt`.
- Editing is deliberately simple: typing appends at the end, Enter starts
  a new line, Backspace deletes the last character. Long lines wrap at 72
  characters; the window shows the last lines, with `_` as the cursor.
  The first line shows the file and `(saved)` or `(editing)`.
- **Saving is automatic**: one second (100 ticks) after typing stops, and
  when the window is closed. The file is truncated and rewritten (it is at
  most 4 KiB). Logs `Notes: saved <n> bytes`. BadgerHandler then writes
  its snapshot to disk as for any change (docs/s5-design.md).

## 2. Window server

- A fourth application slot: dock tile `Notes`; its window at (200, 100),
  640×480, above the others when opened. Applications closed at start are
  not started with the session, so every existing process number stays the
  same.
- The dock grows to four slots; the dock case's coordinates move with it.

## 3. Verification

Two boots of one data disk:

1. `notes-1` (fresh disk): open Notes from the dock; type `Hello from
   Notes`, Enter, `second line`; wait for `Notes: saved` and
   BadgerHandler's `saved generation`; the window shows both lines after
   the original file's text (screenshot).
2. `notes-2` (same disk, reboot): open Notes; the two typed lines are
   there (screenshot), loaded from the disk.

Negative control: Notes does not save on its timer; `notes-1` must fail
(no `Notes: saved`), and `notes-2` must not find the typed lines.

## 4. As implemented

- `user/src/bin/notes.rs` as §1; it waits for keys with `wait_any` and a
  100-tick timeout while there are unsaved changes.
- Lines end in LF or CRLF: the seed files are stored byte-exact with CRLF
  (`seed/** -text`), and the first version drew the carriage return as
  `?`. Found in the first screenshot; `check_notes` now rejects
  `milk.?`, and that check fails with the CRLF handling removed. Typed
  lines are saved with LF.
- PeacockHandler: `APPS = 4`; Notes is marked closed before the session
  starts, so it is not started and existing process numbers are
  unchanged. The dock case moved to the four-tile geometry.
- Negative control results: `notes-1` timed out waiting for
  `Notes: saved`; `notes-2` timed out because the disk had no snapshot to
  load.
