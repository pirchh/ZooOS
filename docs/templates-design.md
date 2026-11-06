# Templates and forgetting (2026-11-05)

Status: implemented (version 0.123.0). This step of the roadmap was
"learning: examples promoted to templates, and a 'forget this phrasing'
control". The owner wants Bella to learn from use, keeping only what the
user confirmed (docs/one-bella-design.md).

## Templates

Bella's memory holds confirmed pairs: a request, and the command that
answered it. Recall used to refuse a pair whose command names an app the
new request doesn't (`names_agree`), so "put network away" never used
"put files away" → `minimize files`.

**`zoo_infer::memory::by_pattern`:** when the new request is the
remembered one word for word, with one app swapped for another, the
remembered command is used with that app swapped too.

| Remembered (confirmed) | New request | Command |
| --- | --- | --- |
| "put files away" → `minimize files` | "put network away" | `minimize Network` |
| "make notes big" → `maximize Notes` | "make my files big" | `maximize Files` |
| "make notes big" → `maximize Notes` | "make files small" | none: other words differ |
| "notes mode" → `steps …` (a routine) | "files mode" | none: routines are names, not patterns |

- The request and the remembered one must each name exactly one app, and
  the apps must differ.
- The command must name the remembered app.
- Filler ("my", "the", "please", "now", "window") is ignored.
- OtterHandler scores a pattern match at 99, just below an exact one, so
  an exact phrasing always wins.
- It draws only on confirmed pairs, and it learns nothing new: a pattern
  answer is not remembered again.

## Forgetting

"Forget <phrase>": when memory holds those very words (likeness 100), she
asks "Forget that "<phrase>" means `<command>`?", like any delete. A yes
sends `forget-phrase <phrase>` to OtterHandler, which removes every pair
with the same words. This covers routines too ("forget notes mode").
Anything else still goes to the facts: "Forget what you told me about …?"
(docs/facts-design.md).

## Verified

| Check | Result |
| --- | --- |
| `bella-pattern` (no model): "put files away" not answered → "minimize files" → learned; "put network away" → `by the pattern of \`put files away\`` → `recalled \`minimize Network\` (99% alike)`; "forget put files away" → asked → y → `OtterHandler: forgot \`put files away\` (1 remembered)`; "put files away" again is not recalled | `OK` |
| `by_pattern` always `None` (**negative control**) | `FAIL`: no "by the pattern" line |
| `forget-phrase` that keeps the lines (**negative control**) | `FAIL`: `Bella: recalled \`minimize files\`` appeared |
| `bella-facts`, `bella-memory`, `bella-routine` | `OK` |
| `memory::swapped_app` unit test | pass |
| `bella-regress` | 43 as recorded, 0 changed |

## Not done

- Templates over other things: paths, people, numbers.
- "Forget that" for the answer just given, and a spoken list of what she
  has learned.
