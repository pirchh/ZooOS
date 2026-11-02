# Stage 0: fix what the audit found

Status: in progress (2026-11-02). B1, B2, B6 and B7 done in version
0.74.0; B3, B4 and B8 in 0.75.0.

Part of the Bella roadmap (docs/bella-roadmap.md). The problem numbers
B1 to B15 come from the code audit (docs/bella-audit.md). Stage 0 removes
data loss and isolation holes before any new feature.

| # | Problem | Status |
| --- | --- | --- |
| B1 | Notes cuts files over 8 KiB on save | fixed, version 0.74.0 |
| B2 | A removed folder's handle reaches the next node in its slot | fixed, version 0.74.0 |
| B3 | The memory learns wrong answers | fixed, version 0.75.0 |
| B4 | Planted memory lines run commands | fixed, version 0.75.0 |
| B5 | The desktop keeps a dead home handle after a BadgerHandler restart; restart limits never decay | open |
| B6 | Bulk lengths are trusted up to 64 KiB, not checked against the mapping | fixed, version 0.74.0 |
| B7 | One process can take every filesystem session | fixed, version 0.74.0 |
| B8 | Memory matching on shared words picks the wrong folder | fixed, version 0.75.0 |
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

## B3, B4, B8: the memory learns only right answers and replays only safe ones

The memory (docs/a2-design.md) pairs a request in words with the command
that answered it, in `.assistant-memory` in home.

**B3, learning wrong answers:**
- **The bug:**
  - A `search` that found nothing still counted as an answer.
  - A recalled command was learned again.
  - Because the recall used up the request, a command typed after a wrong
    recall was never learned as the correction.
- **The fix:**
  - OtterHandler finishes a search without matches with `none`, not `ok`.
    The Assistant treats it as "ran, answered nothing": not learned, and
    the request stays open for a typed command.
  - A recall runs its command without learning it, then leaves the
    request open, so a command typed next replaces the wrong pair (pairs
    for the same request replace each other).
  - Declining a `create` no longer drops an open request.

**B4, planted lines:**
- **The bug:** any program with write access to home can add lines to the
  memory file, and recalled commands ran without the command filter. So a
  planted `undo`, or any other text, ran with no model and no
  confirmation.
- **The fix:** only `search`, `list`, `inspect` and `create` are learned
  or replayed (`RECALLABLE`).
  - A remembered command that is anything else, or contains a tab or
    newline, is ignored and logged: ``Assistant: ignored remembered `…` ``.
  - The request then goes to the model as usual.
  - `create` still asks before acting.

**B8, the wrong folder:**
- **The bug:** "make a new project folder for work called Receipts" shares
  75% of its words with the same request for Invoices, so it recalled
  `create Invoices`.
- **The fix:**
  - A remembered pair is recalled only if every name its command's
    argument took from the remembered request is also in the new one.
  - Names are the words, also split at `/`.
  - Otherwise OtterHandler skips it:
    ``OtterHandler: `create Invoices` names other things than `…` ``.

**What it does not do:** the memory file still lives in home, writable by
any program with home access. The filter limits what a planted line can
do to what the user could type. Moving the file behind the broker's own
handle is Stage 2.

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
- **`memory-guard`** (normal image, no model, so requests in words stay
  open for a typed command):
  1. In the shell, `put .assistant-memory where is my music	undo`.
  2. In the Assistant, `where is my music` →
     ``Assistant: ignored remembered `undo` ``.
  3. `find my tax papers`, then `search taxes`, which finds nothing. The
     test then checks that ``learned `find my tax papers` `` never appears
     in the log.
  4. `make a new project folder for work called Invoices`, then
     `create Invoices` and `y` → learned.
  5. The same request with `Receipts` (75% alike, enough to recall) →
     OtterHandler's "names other things" line, and no
     `Assistant: recalled` anywhere.
- **New harness step `NeverSeen`:** fails a case if a line containing its
  text appeared. The shell's `put` writes `	` as a tab, so the test can
  plant a line.
- **Negative controls:** one for each fix (docs/verification.md).
