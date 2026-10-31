# Desktop step 7 (D7): Files browses the home folder

Status: implemented 2026-10-31 (version 0.29.0); §4 records the
implementation.

## 1. Problem

Files lists the system's own initial filesystem (`bin/zookeeper`,
`bin/beluga-test`, …), which is the operating system's inside, not a
person's files. Since S4b/S5 the home folder is a real, writable,
persistent tree served by BadgerHandler.

## 2. Change

- Files gets a home-folder handle (like the Assistant and Notes) and shows
  the home folder: first line the location (`Home`, `Home/Documents`),
  then the entries in name order, folders as `Documents/`, files as
  `notes.txt (66 bytes)`.
- Enter on a folder opens it; Enter on a file shows its first lines (CR
  before LF dropped) with `Backspace: back to the folder`; Backspace in a
  folder goes up, with the folder you came from selected.
- Up to 23 entries per folder are shown (one window line is the location).

## 3. Verification

The desktop checks that looked at the old listing change to the home
folder:

- Start: `Home`, `Documents/` selected (and `Documentsx/` rejected).
- Down, Down, Enter: `Photos/` opens: `Home/Photos`, `README.txt (34
  bytes)` selected.
- After Crash Test's crash and restart: Files still shows `Home/Photos`.
- After the window server's restart: `Home` with `Documents/` selected
  again.
- The model's busy-desktop check: `Photos/` selected.

A new case `files-browse`: open `Documents`, then `notes.txt`: the
preview shows its two lines (screenshot). Backspace twice returns to
`Home`; Down, Down, Enter opens `Photos`; Backspace returns to `Home`
with `Photos/` selected (screenshot).

Negative control: Backspace forgets which folder you came from (selects
the first entry); `files-browse` must fail (it goes up from `Photos` into
`Home` and expects `Photos/` selected).

## 4. As implemented

- `user/src/bin/files.rs` rewritten as §2; PeacockHandler starts Files
  with a home-folder copy. Like the other home-folder apps it gets full
  rights to the copy; read-only copies for apps that only read are later
  work.
- The old details view (name, size, index in the initial filesystem) is
  gone; the system's files remain visible in the kernel shell (`ls`).
- Negative control result: `FAIL files-browse`, `Photos/ not on screen`
  (selected) after going back up.
