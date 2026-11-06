# Model refinement: fewer wrong actions (2026-11-06)

Status: implemented (version 0.125.0). The owner asked for "some model
refinement" overnight. There was no download and the model is unchanged.
Two changes to how Bella uses it, each measured before and after.

## Measured before

`cargo xtask assist-check` (Qwen3 1.7B Q4_0, AVX2) and `bella-regress`:

| | Before |
| --- | --- |
| Requests with a command, answered right | 44 of 46 |
| Requests with no command, acted on anyway | 1 of 8 ("What time is it?" → `close "Network"`, margin 0.67) |
| `bella-regress` known gaps | 2: "Abele maximize my notes", "move Bella to the left and files on the right, please" → `move` |

The two `assist-check` failures that remain are not wrong choices:

- "Open my notes file" → `open "notes"`: ZooOS finds `Documents/notes.txt`
  by that name (OtterHandler: `` `notes` is `Documents/notes.txt` (found
  by name) ``).
- "Delete my old playlist" → `trash "old playlist"`: the argument keeps
  "old".

## 1. A "none" example in the prompt

The prompt's examples got one more line, `Set an alarm for seven -> none`.
It is about time, but worded unlike every test sentence, so the examples
don't hand the tests their answers.

| Variant | Right | Acted on anyway | `bella-regress` |
| --- | --- | --- | --- |
| Before | 44 of 46 | 1 of 8 | — |
| **A: the alarm example** (kept) | **44 of 46** | **0 of 8** | 2 changed, both better: "What time is it?" and "set up my workspace" (was `split ""`) now no command |
| B: A, and the split line extended ("one app on the left and another on the right") | 44 of 46 | 0 of 8 | also broke "let's slip the screen with files and Bella" |
| A, and the split line rewritten ("put apps side by side…") | 43 of 46 | 0 of 8 | broke "Tile my windows" too |

The split line stayed as it was.

## 2. Left and right without the model

The prompt couldn't fix "move Bella to the left and files on the right,
please": the model chose `move` with the whole sentence as its argument.
That doesn't need a model. **`zoo_infer::route::sides`:** a request with
both "left" and "right" that names exactly two apps becomes
`split <left> and <right>`, the apps taken in the order of the words
"left" and "right". It is the last check before the model, after
the registry's words, and it's logged as `left and right`, its own route.

| Sentence | Before | Now |
| --- | --- | --- |
| "…move Bella to the left and files on the right, please?" | `move "Bella to left and files on the right please."` | `split Bella and Files` |
| "Put notes on the left and files on the right" | model: `split "Notes and Files"` | the same, without the model |
| "put files on the right and notes on the left" | — | `split Notes and Files` (unit test) |

## After

| | Before | After |
| --- | --- | --- |
| `assist-check`: right / acted on anyway | 44 of 46 / 1 of 8 | 44 of 46 / 0 of 8 |
| `bella-regress` known gaps | 2 | 1 ("Abele", a hearing gap) |

`bella-unsure` (docs/unsure-design.md) used "what time is it" to get a
low-margin `close`. Now that this is no command, it uses "lose the network
window" (`close Network`, margin 3.83 on the host).

Also found: "put that somewhere else" → `move "current file to somewhere
else"` with margin 16.41, sure of itself and wrong. OtterHandler finds no
"current file", so nothing moves, but it is noted for the next refinement.

## Verified

See docs/verification.md.
