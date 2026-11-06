# Routines (2026-11-05)

Status: implemented (version 0.122.0). The owner asked for confirmed
sequences saved under a name, such as "set up my workspace". That is how
Bella "adds functionality" from use: a routine maps new words onto
things she can already do. It never adds a capability.

## Saving one

There are two ways, both asked about first. The steps are read back,
and only a yes saves them:

| Said | Bella |
| --- | --- |
| "When I say notes mode, open notes and minimize it" | `Save "notes mode" as: open notes \| minimize it?` |
| "Save that as notes mode" (after doing something) | the request just done, its steps as they ran: `Save "notes mode" as: open notes \| minimize Notes?` |

- `zoo_abi::bella::routine_def` reads "when I say", "whenever I say" and
  "if I say". The name ends at the first comma.
- `save_last_as` reads "save that as", "save it as", "call that" and
  "remember that as".
- `zoo_infer::route::plan` checks both before splitting steps, since a
  routine's body has steps of its own. They appear in `bella-regress`.

## How it is kept

A saved routine is a confirmed phrasing in Bella's memory
(docs/a2-design.md), kept by OtterHandler, with the command
`steps <a> | <b> | …`. Nothing new is stored: the memory file, its size
limit and its matching are the ones she already has.

- **Running:** saying the name (or something at least 75% alike) recalls
  `steps …`. Bella runs the steps one by one, as a multi-step request,
  without the model. "it" still means the app the step before named.
- **Safety:** memory is in the home folder, which other programs can
  write. Bella replays only the commands on her list (`RECALLABLE`), and
  `steps` is now one of them. Each step goes through the normal path:
  a delete inside a routine still asks.
- A routine's own steps are not saved again by "save that as".

## Verified

| Check | Result |
| --- | --- |
| `bella-routine` (no model): define → asked → y → `OtterHandler: remembered \`notes mode\` -> \`steps open notes \| minimize it\``; "notes mode" → recalled → 2 steps → Notes opened and minimized; "save that as big notes" → asked → n → "Not saved." and nothing remembered | `OK` |
| The same with `steps` not replayable (**negative control**) | `FAIL`: `Bella: ignored remembered \`steps open notes \| minimize it\`` |
| Unit tests: `routine_def`, `save_last_as`, `plan` routines, `routine_command` refusing a `\|` inside a step | pass |
| `bella-regress` | three new sentences recorded. On the PC there is no memory, so "set up my workspace" goes to the model there |

## Not done

- Listing, renaming and forgetting routines by voice. Forgetting comes
  with the "forget this phrasing" control.
- Routines with values ("open notes with today's date").
- A step that fails doesn't stop the routine.
