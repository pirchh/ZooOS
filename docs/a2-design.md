# Assistant step 2 (A2): the Assistant learns from use

Status: implemented 2026-11-02 (version 0.66.0). The owner asked for the
Assistant to "learn over time" without losing what the model knows, and
to answer much faster.

## 1. How it learns

The model's weights never change. What changes is a small **memory of
requests and the commands that answered them**, kept in the user's home
folder (`.assistant-memory`, one `request<TAB>command` line per pair,
newest last, at most 16 KiB).

| Event | What is remembered |
| --- | --- |
| A request in words is answered by a command that works (chosen by the model or by memory) | the request → that command |
| A `create` is confirmed with `y` | the request → `create <path>` |
| A request in words is **not** answered well, and the user then types a command that works | the request → the typed command |

"Not answered well" means one of these:
- the model was not there, or failed;
- its command failed;
- the user refused its `create` with `n`.

**This is learning from corrections:** the user shows the Assistant what
they meant once, and it remembers.

## 2. How it answers from memory

- **Before asking the model,** the Assistant asks OtterHandler
  `recall <request>`.
- **OtterHandler compares the words** of the request with each remembered
  request:
  - lower-cased, punctuation dropped;
  - filler words dropped (`a`, `the`, `my`, `please`, …);
  - likeness = shared words / all words, in percent.
- **It replies with the most alike pair:** `| <likeness> <command>`.
- **At 75 % or more,** the Assistant runs that command at once and shows
  `Assistant (remembered): <command>`. The model is not asked.
- **Below 75 %,** the model is asked as before.

**The memory lives in OtterHandler,** which already holds the home folder.
The model service still has no file access at all
(docs/m5c2-design.md §2).

## 3. Why not change the model

- **Fine-tuning on the device** (even a LoRA adapter) needs gradients and
  a training run.
- **It can also forget base knowledge,** which the owner asked to avoid.
- **A memory is exact and visible:** each answer from memory says so, and
  the file can be read, edited or deleted.
- **A later step could** use the memory as examples in the model's prompt
  for requests that are only partly alike, or train an adapter on the
  host from it. Neither is done here.

## 4. Speed

**An answer from memory is a file read and a word comparison.** It costs
milliseconds, where the model takes seconds (A1 measures the model's own
speed-up).

## 5. Checks

The new case `assistant-memory` runs without a model, so it runs in every
`verify`:
1. `where is my pancake recipe` → the model is not installed.
2. `search pancake` → `learned`.
3. `where is the pancake recipe` → `recalled (100% alike)`: the result is
   shown with no model.
4. Restart the desktop, then `pancake recipe where is it` →
   `recalled (80% alike)`.
5. `make a folder called Invoices` is **not** recalled: the model service
   is started and reports no model.
