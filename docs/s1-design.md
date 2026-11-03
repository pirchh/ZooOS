# Stage 1: measure and evaluate

Status: in progress (2026-11-02). Part of the Bella roadmap
(docs/bella-roadmap.md). Goal: numbers for speed and for correctness, so
every later stage proves its gain.

| Item | Status |
| --- | --- |
| B9: a "none of these" answer and a measured `assist-check` | done, version 0.78.0 |
| One timing line per request; peak heap | done, version 0.78.0 |
| Responsiveness while the model runs | open |
| Baselines under TCG and WHPX | timing lines: see docs/verification.md |

## 1. B9: "none of these"

**The problem:**
- Every request became a tool call: "Tell me a joke" ran `list /jokes`.
- The 8-request `assist-check` had only requests with a right command,
  so this was never measured.

**The measurement.** `assist-check` now has 22 requests:
- the original 8;
- 6 more with a right command;
- 8 that no command answers (weather, a joke, email, the time, volume,
  sport, shutting down, deleting files).

It reports two numbers:
- requests with a command answered right;
- requests without one that were **acted on anyway**.

**A confidence threshold alone cannot work.** The tool word's margin over
the runner-up, on the original prompt:

| Requests | Margins |
| --- | --- |
| With a right command (8) | 0.04 to 5.37 |
| Without one (7 tried) | 0.29 to 2.65 |

The ranges overlap: a correct answer can be less sure than a wrong one.

**A fifth choice, `none`.** `zoo_infer::assist` has a fifth tool word,
`none` (`Tool::Nothing`), with no argument. Four system prompts were
measured on the host engine, on all 22 requests:

| Prompt | Right, of 14 | Acted on anyway, of 8 | Prompt tokens |
| --- | --- | --- | --- |
| Original (no `none`) | 12 | **8** | 55 |
| **+ "If the request is not about finding, listing, inspecting or creating files or folders, reply: none"** (chosen) | **10** | **2** | 76 |
| `none` in the command list, "Reply none when …" | 9 | 4 | 70 |
| Examples of each command plus `none` | 8 | 3 | 101 |
| Examples, then "If no command fits, reply: none" | 8 | 2 | 102 |

**Chosen: the second row.** It is right on 16 of 22, against 12 of 22 for
the original.
- **What it loses:** two in-scope requests, both `list` answered as
  `search` ("Show me what's inside Music", "Which files are in
  Documents/recipes?"). A search still finds the folder.
- **What it costs:** 21 more prompt tokens, about 1.2 s more set-up on the
  host for the first request. Stage 2 warms the prompt in the background.
- **Examples made the 0.5B model worse.** It copied them ("search tax
  papers" for taxes). That is a sign of the model's limits; the Stage 5
  model bake-off uses this same set.

**In Bella:** `none` shows "I can only search, list, inspect or create
folders." and leaves the request open, so a command typed next answers it
(and is learned).

**The bar.** `assist-check` passes while at least 10 of 14 are right and
at most 2 of 8 are acted on: today's numbers. A change that does worse
fails it.

## 2. Timing lines and memory

One line per request, so speed claims have numbers behind them:

- **ElephantHandler**, per request in words:

  `timing: setup S ms, reading R ms (N tokens), writing W ms (M tokens),
  total T ms; heap H KiB, peak P KiB`

  - `setup` is the system prompt (first request only).
  - `reading` is the request's tokens, up to the tool choice.
  - `writing` is the argument.
  - At start: `service ready (model mapped, parsed in X ms)`.
- **Bella:** `answered in A ms by <a typed command | memory | the model |
  the model (no command fits)>`, from Enter to the answer shown, through
  the broker.
- **OtterHandler:** `heap peak K KiB` each time its heap's peak passes
  another 64 KiB.
- **`zoo_rt::heap_stats()`** returns bytes in use, the peak and the heap's
  size.
- **`zoo_rt::now_us()`** reads the CPU's cycle counter, calibrated once
  per program against the timer, as PeacockHandler's animation clock is.
  The kernel's monotonic clock counts 10 ms ticks, too coarse: a typed
  command first measured "0 ms".

**Baselines** (2026-11-02, `--accel whpx`, Qwen2.5-0.5B Q4_0, the new
prompt):

| | `bella-live` | `everyday-model` |
| --- | --- | --- |
| Model parsed at service start | 248 ms | 242 ms |
| First request: set-up (system prompt, 76 tokens) | 4720 ms | 4677 ms |
| First request: reading (10 tokens) / writing (3 tokens) | 729 / 254 ms | 747 / 247 ms |
| First request: Enter to answer (Bella) | 6011 ms | 6002 ms |
| Second request: reading (15 tokens) / writing (6 tokens) | 1032 / 499 ms | — |
| Second request: Enter to answer | 1557 ms | — |
| ElephantHandler heap, peak | 18,580 KiB | 18,580 KiB |

- **About 300 ms of the first request is outside the engine:** starting
  the service and parsing the model, before the 5.7 s it reports.
- **A typed command,** through the broker, under TCG: 1 to 6 ms.
- **The first request's set-up** (4.7 s, up from 3.4 s with the
  55-token prompt of A1) is what Stage 2's background warm-up removes.

The model's weights are mapped from the initial filesystem, not the heap,
so the heap figures are the engine's working memory: caches and scores.

## 3. Checks

- **`assist-check` (host):** `PASS` at 10 of 14 right, 2 of 8 acted on.
  - **Negative control:** with `none` never chosen, `FAIL` (8 of 8 acted
    on).
- **`bella-any-case`:** also requires `ms by a typed command`.
- **`bella-live` and `everyday-model`** (WHPX): also require the
  ElephantHandler timing line and `Bella: answered in`.

## 4. Not done yet

- **A responsiveness check:** the time from a key press in Files to its
  frame while the model runs.
- **p50/p95 figures over many requests.** The lines make them possible;
  collecting them is part of the Stage 5 bake-off.
