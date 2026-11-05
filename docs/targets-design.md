# Targets, results and bound questions (2026-11-04)

Status: implemented (version 0.118.0). From an outside design review the
owner shared: "Which milk entry, today's or Monday's? is more useful than
Do you want me to remove milk?"; a yes must answer one pending question,
not whatever is open; results need a request id; and "Don't delete the
milk entry" must not delete it.

## 1. Results with request ids

| Direction | Form |
| --- | --- |
| Bella → app | `do:<program>:<action>:<slots>␟#<id>[␟@<target>]` (`␟` is the unit separator) |
| app → Bella | `did:<status>␟#<id>␟<sentence>[␟<choice>...]`, status `ok`, `err` or `choose` |

- `zoo_infer::registry::{Outcome, Status, split_arg}` read and write these.
- The window server passes them through unchanged, and still checks the
  slots against the registry. The id and the target are not slots.
- A plain sentence, from an app not yet changed, is read as `ok`.
- **Bella keeps her last action's id** and acts on a `choose` only for that
  request.

## 2. "Which one?"

- **Notes no longer picks the newest match.** `notes::remove` and
  `notes::change` find every matching line. With one, they edit it. With
  more than one and no target, they change nothing and return the lines.
- Bella lists them: `Which one: 1. Shopping: flour, eggs, milk. | 2.
  flowers, bread and milk`.
- **The answer:**
  - a number key;
  - spoken: "the first", "the last", a bare "two", or a word only one
    line has ("the shopping one", "the one with flowers"; "milk", in
    both, is not enough);
  - no, n or Back leaves it.
- Bella sends the action again with `@<line>`, and Notes edits only that
  line.

## 3. Questions bound to one proposal

- A question (a delete, "which one?", "is this what you meant?") is open
  for **60 s**. Answered later, it says "That question has expired;
  please ask again." and does nothing.
- A spoken answer is taken only while BatHandler was told to wait for it
  (docs/listening-design.md), and each question asks anew.

## 4. Not doing what was asked not to

`zoo_abi::bella::negated`: "don't …", "do not …", "never …" and "never
mind" give "Okay, I won't." and nothing else. This is checked before the
model, steps or memory.

## 5. The review's test sentences

| Sentence | Result |
| --- | --- |
| "Add 'delete the old notes' to my notes" | `write "delete the old notes"` (assist-check, margin 14.32) |
| "Write 'bread and butter'" | a typed command (`write` then text), before the model; the model alone said `none` |
| "Add milk to Notes and open Files" | two steps: "Add milk to Notes", "open Files" (`steps` unit test) |
| "Don't delete the milk entry" | nothing done (`negated`, and in QEMU: "don't remove flowers") |

## 6. Verified

| Check | Result |
| --- | --- |
| `cargo test -p zoo-infer notes`, `registry`; `zoo-abi` | two matches give `Choose`; a target picks one; a wrong target is not found; outcomes round-trip; ids and targets aren't slots; `spoken_choice`; `negated` |
| `bella-notes` (updated) | `OK`: "remove milk" → asked → y → "Which one: 1. Shopping… \| 2. flowers, bread and milk" → 2 → only that line changed; "don't remove flowers" → nothing done; "remove flowers" → n → kept |
| `bella-notes` with "newest wins" put back (negative control) | `FAIL`: no "Which one" |
| `voice-confirm`, `bella-actions`, `net-bella-browse` | `OK` (Network answers with the id too) |
| `assist-check` | 44 of 46 |

## Not done

- Notes with titled notes (several notes as targets).
- Results for the system's own commands (files, windows).
- Partial results of a multi-step request reported as one.
