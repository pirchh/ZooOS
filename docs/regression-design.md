# Bella's regression sentences (2026-11-04)

Status: implemented (version 0.119.0). From the outside design review the
owner shared: keep the demos' transcripts as a regression set with
baselines, so a change to the gate, the routing or the model shows up as a
change in what Bella does with a known sentence.

## 1. What runs

`cargo xtask bella-regress` reads `tests/bella/requests.txt` and puts each
sentence through Bella's own path on the PC:

| Stage | Code | The same code in ZooOS |
| --- | --- | --- |
| BatHandler's gate: for her, her name alone, or not for her | `zoo_abi::bella::{addressed, name_only}` | `user/src/bin/bat-handler.rs` |
| The plan: asked not to, several steps, or one | `zoo_infer::route::plan` | `Bella::submit` |
| Each step's path: typed command, layout from the words, facts, small talk… | `zoo_infer::route::route` | `Bella::submit_one` |
| What reaches the model: its choice | `zoo_infer::assist::Bella::choose` | ElephantHandler |

**`route` and `plan` are new, and Bella now uses them:** before this
change the routing was a chain of `if`s inside `user/src/bin/bella.rs`,
which a host check could only copy. The chain moved unchanged into
`infer/src/route.rs`, with one change of form: whether a remembered fact
answers the request is asked of the facts service (OtterHandler), so it is
checked by Bella when `route` says `Model`, in the same order as before.

## 2. What the first run found, and the fix

The first run (Qwen3 1.7B) found that the model chooses nothing for "Add
milk to Notes", one of the review's sentences, and for "add milk to my
notes". It chose `write` only for the longer "add flowers, egg and milk to
my notes". The registry already holds the words for this: "add" is a verb
of Notes' `write`, and "to my notes" one of its places.

**`Registry::phrased`:** when a request starts with an action's verb and
ends with one of its places of two words or more, Bella runs the action
without the model, filling its slots the way a typed command's are. It
runs last, just before the model, so every earlier path keeps its
sentences, and only when exactly one action reads the request that way.
One-word places ("down", "off", "out") don't count: "put it down" and
"take me out" stay the model's. A delete still asks first. A new app's
`.actions` file gets this without code.

| Sentence | Before | Now |
| --- | --- | --- |
| "Add milk to Notes" (a step of "… and open Files") | model: no command | `write "milk"` |
| "Hey, Bella, add flowers, egg and milk to my notes." | model: `write` | registry words: `write` |
| "Hey, Bella, delete flowers, egg and milk for my notes." | model: `remove` | registry words: `remove`, asked first |
| "replace milk with oat milk in my notes" | model: `change` | registry words: `change "milk / oat milk"` |
| "Add 'delete the old notes' to my notes" | model: `write` | registry words: `write` |

Known gaps it records (`=> want`), left for the next steps:

| Sentence | Now | Wanted | Step |
| --- | --- | --- | --- |
| "Abele maximize my notes." | not for her (no greeting before a near name) | maximize Notes | staged wake |
| "Bella said she would call later." | for her | not for her | staged wake |
| "…can we stop, move Bella to the left and files on the right, please?" | `move` with the whole sentence | `split "Bella and Files"` | — |
| "What time is it?" | `close "Network"` | no command | — |

## 3. Sentences

- `said:` sentences go through the gate.
- `typed:` sentences are typed in her window.
- `=> want <x>` marks a known gap: what she should do and does not yet.
  It is reported, not failed.

**Privacy.** Only sentences the owner said *to Bella* come from the demos
(`run --mic`, 2026-11-04), as Moonshine wrote them ("split my scream",
"Pay billah"). What it heard that was not for her is never copied. The
"not for her" sentences are written for the file.

## 4. Baselines

- Results are compared with `tests/bella/baseline-<model>.txt`, one per
  model, because models choose differently.
- Any change fails, better or worse, until it is looked at.
  `--accept` records the new results; the diff of the baseline file in a
  pull request shows what changed.
- The model's context is empty on the PC; in ZooOS ElephantHandler adds
  the screen and the conversation. A sentence whose result depends on them
  belongs in a QEMU case.

## 5. Verified

| Check | Result |
| --- | --- |
| `cargo xtask bella-regress` (Qwen3 1.7B Q4_0, AVX2) | `PASS`: 37 as recorded, 0 changed; 4 known gaps; about 35 s |
| A baseline line edited (**negative control**) | `FAIL`: `CHANGED typed Tell me a joke` |
| `phrased` turned off in `route` (**negative control**) | `FAIL`: 5 changed |
| `bella-notes` with "add bread to my notes" (no model in the image) | `OK`: "in the registry's words: write bread", then Notes wrote it |
| The same with `phrased` turned off (**negative control**) | `FAIL`: timeout at that line |
| `cargo test -p zoo-infer`: `route`, `phrased_in_its_words` | pass |

## Not done

- The four gaps above.

- It is not part of `verify`: like `assist-check`, it needs the model on
  the PC, and `verify --skip-model` runs without it.
- Spoken answers to her questions (yes, "the second one") are covered by
  unit tests and the `voice-confirm` and `bella-notes` cases, not here.
