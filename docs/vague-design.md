# Commands that name nothing (2026-11-06)

Status: implemented (version 0.128.0). Found during the model refinement
(docs/refine-design.md): "put that somewhere else" made the model choose
`move "current file to somewhere else"` with margin 16.41. That is sure of
itself, so the unsure guard (docs/unsure-design.md) didn't ask. In QEMU
the same request chose `move "Downloads to somewhere else"`, and Bella
sent it to OtterHandler, which found no folder called "somewhere else".
**Had one existed, she would have moved Downloads into it.**

## The rule

`zoo_infer::route::vague(word, arg)`: a model's choice that moves, closes
or removes something (`move`, `close`, `trash`, `delete`, `remove`,
`rename`, `copy`) names nothing when its argument:

- is only a pronoun ("that", "it", "this", "them", "those"). "It" after
  another step is resolved before the model (docs/multi-step-design.md), so
  this "it" refers to nothing; or
- has a word that names no thing and no place: "somewhere", "elsewhere",
  "anywhere", "someplace", "something", or "current" (the model's
  invented "current file").

Bella then treats it as "no command fits" and answers in words, as for
`none`. She doesn't ask "Do you want me to …?", because there is nothing
to say yes to. Commands typed by hand ("move it somewhere") run as typed,
as before. `bella-regress` reports the rule as `names nothing (answered in
words)`.

## Measured

| | Before | After |
| --- | --- | --- |
| "put that somewhere else" (host, `bella-regress`) | `move "current file to somewhere else"`, run | the same choice, answered in words |
| The same request in QEMU (`bella-unsure`) | `move Downloads to somewhere else` sent to OtterHandler (negative control) | `` `move` names nothing to act on ``; nothing moved, nothing asked |
| `bella-regress`, the 43 sentences before | — | 43 as recorded, 0 changed |

## Verified

See docs/verification.md.
