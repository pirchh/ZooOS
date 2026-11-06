# Asking when unsure (2026-11-04)

Status: implemented (version 0.121.0). The owner asked earlier for
Bella to "ask when unsure": a low-confidence choice, or anything
destructive, gets a "Do you want me to …?" first. The regression set
(docs/regression-design.md) recorded "What time is it?" → `close
"Network"`; that choice was acted on.

## The rule

`zoo_infer::route::ask_first(word, margin, registry)`: Bella asks before
running the model's choice when both hold:

- it **changes things**: `close`, `move`, or an app action whose effect
  is `change` in the registry. Deletes, `trash` and `create` already ask;
  opening, reading and arranging are easily undone and never ask.
- the model was **unsure**: its margin is below `UNSURE_MARGIN` (5.00;
  ElephantHandler reports it in hundredths).

**The threshold comes from measurement.** On `assist-check` (Qwen3 1.7B
Q4_0, 2026-11-04), right choices that change things had these margins:

| Request | Choice | Margin |
| --- | --- | --- |
| "Remove milk from my notes" | `remove` | 12.97 |
| "Quit Notes" | `close Notes` | 28.27 |
| "Write down call the vet tomorrow" | `write` | 8.05 |
| "What time is it?" (wrong) | `close Network` | 0.67 (1.31 in ZooOS, with the screen as context) |

## What happens

- **Yes:** it runs, and the request is learned as meaning it: a phrasing
  the user confirmed.
- **No:** "Okay, I won't.", then she answers in words. A command typed
  next can still answer the request.
- Like every question, it expires after 60 s and can be answered by
  voice.

## Verified

| Check | Result |
| --- | --- |
| `route::unsure` unit test | pass |
| `bella-regress` | "What time is it?" now `close "Network", asked first (unsure)`; 1 changed, accepted |
| `bella-unsure` (WHPX, the model in the image): "what time is it" → `ElephantHandler: chose close Network` → "unsure (1.31)" → asked → n → "Okay, I won't."; no window action, nothing learned | `OK` |
| The same with `ask_first` always false (**negative control**) | `FAIL`: no "unsure" within 300 s |

Since docs/refine-design.md (0.125.0), "What time is it?" is no command,
and `bella-unsure` uses "lose the network window" (`close Network`,
margin 3.83).

## Not done

- One threshold for all models. A model change re-measures it with
  `assist-check`.
- The answer in words after a no is the talk model's, so "what time is
  it" still gets no time: ZooOS has no time zones yet.
