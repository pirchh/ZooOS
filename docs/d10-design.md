# Desktop step 10 (D10): a cursor in Notes

Status: implemented 2026-10-31 (version 0.38.0). Follows D4.

## 1. Change

- The app protocol gains two keys, `key:left` and `key:right`; the window
  server sends them for the arrow keys (apps that do not use them ignore
  them, as Files and the Assistant do).
- Notes keeps a cursor (a byte offset, starting at the end). Left and
  Right move it; typing and Enter insert at it; Backspace deletes before
  it. It is drawn as `_` at the very end (as before) and `|` inside the
  text.

## 2. Verification

`notes-cursor` case (fresh disk): open Notes, type `a b c`, Left twice,
`X`, `Y`, Backspace → `Notes: saved 70 bytes` (66 + 4) and the screen
shows `aX|bc` (no `aXY`, no `abcXY`), `(saved)`. `notes-1`/`notes-2`
unchanged.

Negative control: Left ignored → `FAIL notes-cursor`, `aX|bc not on
screen`.

## 3. Not done

- No Up/Down between lines, Home/End, selection, copy or paste.
