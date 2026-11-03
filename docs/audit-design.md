# Bella's audit trail

Status: implemented in version 0.89.0. Stage 2 of docs/bella-roadmap.md
("one audit line per action"). It goes with the owner's broad-access
decision (docs/access-design.md): Bella may change things without asking,
apart from deleting, so there must be a record of what she changed.

## What is recorded

OtterHandler, the broker that does Bella's file work, records:

- **Every request, in the log.** Format: `OtterHandler: audit: <tool>
  <argument>: <outcome>`. The outcome is `done`, `nothing found`, or the
  reason it failed. Reads (`search`, `list`, `read`, `inspect`, `info`)
  are recorded too.
- **Every change that worked, in `.bella-audit`** in the home folder. One
  line per change, `YYYY-MM-DD HH:MM <tool> <argument>`, in UTC, newest
  last. The changes are `create`, `move`, `trash`, `undo`, `fact-add` and
  `fact-forget`.

Reads stay out of the file so it shows what changed, not everything Bella
looked at. A failed change is not written to the file either; its log
line says why it failed.

The file is capped at 16 KiB; the oldest lines go first. It is a plain
text file the user can open in Notes.

## Asking

"What did you do?", "what have you changed?" or "show your history" make
Bella ask the broker for `audit-list`. It sends the last 8 lines, and Bella
shows them under "What I changed, newest last:". With no changes she says
"nothing yet."

These phrases are matched by `zoo_abi::bella::asks_what_bella_did`, which
is host-tested. "What did I just open?" is about the user, so it still goes
to the screen snapshot (docs/context-design.md).

## Checks

`bella-audit` (TCG):

1. "what did you do?" on a fresh disk → `listed 0 of 0 changes`.
2. `list Documents` → `audit: list Documents: done` in the log.
3. "remember that Zara the zebra likes carrots" → `audit: fact-add Zara the
   zebra likes carrots: done`.
4. A new desktop session, then "what have you changed?" → `listed 1 of 1
   changes`, and the line shown is dated (`shows change: 20…`). So the
   file survived the restart and kept the change, not the read.

## Not done

- The file is Bella's own record, written by the same broker that makes
  the changes. It is not tamper-proof against that broker, and the user
  can edit or delete it.
- Changes made by apps (Files, Notes) are not recorded here; only Bella's.
