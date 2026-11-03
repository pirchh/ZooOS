# Bella saves a summary

Status: implemented in version 0.91.0. This finishes the Stage 5 workflow
"find, summarise, then create a note" (docs/bella-roadmap.md), on top of
docs/summary-design.md. The summary is shown first, and it is written only
when the user asks.

## The flow

1. **Summarise.** "Summarise Documents/notes.txt" shows the summary
   (docs/summary-design.md). Bella keeps the file's path and the summary
   as `last_summary`.
2. **Ask to save.** The user says "save that", "save it", "save the
   summary", "keep that as a note" or "make a note of that", optionally
   after "please". These match `zoo_abi::bella::wants_saved` (host-tested),
   and only count while a summary waits; otherwise the words go to the
   usual routing.
3. **Write.** Bella sends the broker `write <stem>-summary.txt<TAB>Summary
   of <file>:\n<summary>\n`. The file lands beside the original:
   `Documents/notes.txt` gives `Documents/notes-summary.txt`
   (`summary_stem`, host-tested).
4. **The broker's `write` tool** makes a new text file of at most 900
   bytes. It never replaces an existing file (`already exists`), and the
   file's folder must exist. The kernel's `FILE_CREATE` refuses an
   existing file too, so the broker's check is a second guard: with it
   removed, `bella-save` still saw `already exists`. It is a change, so it gets an audit line
   (docs/audit-design.md). The audit keeps the path, not the text.
5. Bella says "Saved. Open it with: open Documents/notes-summary.txt".

## Why never overwrite

The owner's access rule is "broad, confirm deletes" (docs/access-design.md).
Replacing a file deletes its old contents. Rather than add a
confirmation, `write` only makes new files. The case that needs a
replacement, saving a second summary of the same file, gets a clear
`already exists`.

## Checks

`bella-save` (WHPX):

1. Summarise `Documents/notes.txt`, then "save that" → `OtterHandler: wrote
   Documents/notes-summary.txt (…)`.
2. Summarise it again, then "save it" → `audit: write
   Documents/notes-summary.txt: already exists`.
3. `open Documents/notes-summary.txt` → `Notes: opened
   Documents/notes-summary.txt (94 bytes)`.

**Negative control:** with "save that" no longer routed to the save,
`FAIL` (no `wrote Documents/notes-summary.txt` within 300 s).

## Not done

- The summary is saved beside the path the user typed. A bare name the
  broker found elsewhere ("summarise notes.txt") saves `notes-summary.txt`
  in the home folder.
- There is no "save it as X": the name is fixed.
