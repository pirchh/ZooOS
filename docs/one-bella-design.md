# One Bella (2026-11-04)

Status: implemented (version 0.116.0). Step 1 of the plan revised after an
outside design review (docs/registry-design.md §6; DECISIONS.md "One
Bella").

## 1. What the owner's demo showed

The owner said "Oh Bella, like, switch instead of doing it, split two
ways, split it three ways with Bella's the third window." The request was
split into steps, and its last step became `split Bella and Bella and
Bella`.
- The window server can show two windows of one app, so it opened two
  more Bella windows, each a new Bella *process*.
- Each new Bella started its own model service, file index and hearing.
  Each model is 1 GB, so they ran out of memory ("no memory for the
  model"), and their hearing found the microphone taken.
- She also *learned* that request, and two other wrong ones, silently:
  "split three ways, two on top, one on the bottom" → `split Files`, which
  maximized Files.

## 2. What changed

| Where | Change |
| --- | --- |
| Kernel | **`CLAIM` (82), ABI 21:** a name held by one live process; `BUSY` for any other; released at exit. Bella claims "bella" before anything else, and a second Bella logs "another Bella is running; this one leaves" and exits before loading a model |
| Window server | **Bella is single-window** (`SINGLE`): a split, Ctrl+N, the dock's New Window or a second `open` of Bella shows her one window (restored if minimized) instead of starting another process |
| Split | **"three ways" counts columns** (`split_ways`), not copies of the next app named; Bella is never repeated; up to four columns |
| Split | **Layouts that aren't columns** ("on top", "below", "quarters", "corners", "grid", "rows") get `say I can only put windows side by side so far; rows and quarters come later.` rather than a guess |
| Split | **Fewer apps named than columns asked for:** `say Which 3 apps should I put side by side? I heard Bella.` |
| Model replies | **`say` (never scored, like `show`):** words instead of an action |
| Learning | **She learns only corrections,** a typed command after a wrong answer. Her own choices are not remembered: `not learned ... (not confirmed)` |
| Memory | **What she learned before is set aside,** not deleted: `.bella-memory` becomes `.bella-memory-unconfirmed`, never read. New learning goes to `.bella-learned` |

**Two guards, not one.** The window server keeps a second Bella window
from opening. The claim keeps a second Bella process from doing anything,
however it was started.

## 3. Verified

| Check | Result |
| --- | --- |
| `claim` (new): LynxTest claims "lynx" and starts a copy of itself | `OK`: `claim name=lynx pid=26`, `claim refused name=lynx pid=27 holder=26`, then `claim released` after the holder's exit |
| `claim` with the kernel granting every claim (negative control) | `FAIL`: the copy exited 1 (it got the name) |
| `bella-one` (new, typed): Ctrl+N on Bella, then `split bella and bella and files` | `OK`: "Bella is one; showing her window"; one Bella column, then Files; no new Bella window |
| `bella-one` without the window server's check (negative control) | `FAIL`: "new window Bella (window 5)". **The second guard held:** that Bella was refused the name and left before loading anything (`claim refused name=bella pid=31 holder=30`) |
| `memory-guard` (updated) | an entry in the old `.bella-memory` is set aside and never recalled; an `undo` planted in `.bella-learned` is still ignored (B4) |
| `cargo test -p zoo-infer` | `split_ways`, `layout_unsupported`, Bella once in splits, four columns, the demo's sentence |

## 4. Not done here

- Per-request caps on processes and memory beyond these two guards.
- Layouts beyond columns, and asking "is this what you meant?": the next
  step (docs/layouts-design.md).
