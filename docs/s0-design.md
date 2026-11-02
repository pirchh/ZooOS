# Stage 0: fix what the audit found

Status: in progress (2026-11-02). B1, B2, B6 and B7 done in version
0.74.0.

Part of the Bella roadmap (docs/bella-roadmap.md). The problem numbers
B1 to B15 come from the code audit (docs/bella-audit.md). Stage 0 removes
data loss and isolation holes before any new feature.

| # | Problem | Status |
| --- | --- | --- |
| B1 | Notes cuts files over 8 KiB on save | fixed, version 0.74.0 |
| B2 | A removed folder's handle reaches the next node in its slot | fixed, version 0.74.0 |
| B3 | The memory learns wrong answers | open |
| B4 | Planted memory lines run commands | open |
| B5 | The desktop keeps a dead home handle after a BadgerHandler restart; restart limits never decay | open |
| B6 | Bulk lengths are trusted up to 64 KiB, not checked against the mapping | fixed, version 0.74.0 |
| B7 | One process can take every filesystem session | fixed, version 0.74.0 |
| B8 | Memory matching on shared words picks the wrong folder | open |
| B15 | docs/m5c1-design.md describes the filesystem as it was | open |

## B1: Notes keeps files whole

**The bug** (`user/src/bin/notes.rs`):
- Notes read at most `CAPACITY` (8,192) bytes into a fixed array.
- Saving truncates the file to zero and writes back what was loaded.
- So any file over 8 KiB lost its tail at the first save, which also
  happens one second after a keystroke and when the window closes.

**The fix:**
- **The text is a growable buffer.** Files up to `CAPACITY`, now 1 MiB,
  load and save whole.
- **Reading asks for one byte past `CAPACITY`.** A file larger than that
  opens **read-only**: the header reads
  `<path> (read-only: over 1 MiB, too large to edit)`. Typing, Backspace
  and Ctrl+S do nothing, and `save` refuses with
  `Notes: not saving <path>: …`.
- **A read error part-way** makes the file read-only as well (`could not
  be read whole`). What was read is shown and never written back.

## B2: folder handles carry a generation

**The bug** (`user/src/bin/badger-handler.rs`):
- A folder channel (session) stored a bare node number.
- Removing a folder freed its slot.
- The next folder or file created anywhere reused that slot, so an old
  channel to the removed folder then reached the new node, with the old
  channel's rights.

**The fix:**
- **`Tree::gens`** holds a generation per slot, raised when its node is
  freed (`free_subtree`).
- **Each session stores its folder's generation when it opens.**
- **Every request first checks it.** On a mismatch, or with the node
  gone, the answer is `NotFound`, so nothing is reachable through a
  removed folder's channel.

## B6: a client's shared buffer is checked before use

**The bug:**
- BadgerHandler mapped any shared-memory object a client sent as its bulk
  buffer.
- It then trusted bulk lengths up to `BULK_SIZE` (64 KiB).
- A one-page buffer and a 64 KiB read made BadgerHandler write past the
  mapping and fault, which stopped every program's files.

**The fix:**
- **New system call `SHM_SIZE` (73)** returns a shared-memory object's
  size.
- **BadgerHandler only takes a buffer of at least `BULK_SIZE`.** A smaller
  one is closed unused, so bulk requests on that channel get `AGAIN`, and
  `zoo_rt` falls back to messages.

## B7: folder channels per process

**The bug:** BadgerHandler serves at most 62 folder channels, and any
holder of one can open more, so one process could take all 62 and leave
every other program unable to open its files.

**The fix:**
- **New system call `CHANNEL_PEER` (74)** returns the pid of the process
  holding a channel's other end, by a scan of the process table. It is
  called only when a folder is opened.
- **`open_session` counts the channels the asking process holds,** and
  refuses past `MAX_PER_CLIENT` (31, half the total). It logs
  `BadgerHandler: pid N holds 31 folder channels; refused another`.
- **What it does not stop:** two processes acting together can still take
  every channel; that needs per-process quotas the supervisor assigns.
- **What is not counted:** handles in transit inside a message have no
  holder.

## Checks

- **`notes-large`:**
  1. Files → Music → double-click `liner-notes.txt`, a new seed file of
     9,230 bytes → `Notes: opened Music/liner-notes.txt (9230 bytes)`.
  2. Type and press Ctrl+S → `Notes: saved 9234 bytes`.
- **ArcticFoxTest, new check 48** (in the `normal` case):
  1. Make `Old` and open a full-rights handle to it.
  2. Remove `Old` and make `New`, which takes its slot.
  3. Through the old handle, `file_create` and `dir_stat` must answer
     `NotFound`, and `New/planted.txt` must not exist.
- **ArcticFoxTest, new check 49:**
  1. A one-page shared buffer is sent on a fresh folder channel.
  2. A bulk read of `Music/liner-notes.txt` (9,230 bytes) must answer
     `AGAIN`.
  3. Before the fix, BadgerHandler faulted.
- **ArcticFoxTest, new check 50:**
  1. It opens folder channels until refused, which must happen at 20 to
     39 of them.
  2. It closes them all; opening one more must work again.
- **Negative controls:** one for each fix (docs/verification.md).
