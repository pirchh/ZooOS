# M5c-2 design: the assistant app and the model loop

Status: implemented 2026-10-30 (results in docs/verification.md). Last M5
step; builds on M5b (inference engine) and M5c-1 (filesystem, directory
capabilities, OtterHandler broker).

M5 acceptance (MILESTONES.md): real results, scoped operations, path escape
rejection, cancellation, bounded resources, useful failure reporting, and a
responsive desktop while inference runs. M5c-1 covered the first three
through the broker; this step adds the model and the user interface.

## 1. Speed constraint and what it decides

Measured in M5b: under software emulation (TCG) the 0.5B model takes about
10 s per prompt token; with WHPX about 0.35 s. A usual tool-calling chat
prompt (a system prompt describing tools as JSON, ≈ 300 tokens) would take
≈ 50 min under TCG and ≈ 100 s under WHPX before the first answer. So:

- **Short prompt, guided decoding** (§3). The system prompt is 55 tokens,
  fed once per model service and reused for every request by rewinding the
  key/value cache; a request adds 10–15 tokens.
- **Verification splits** (§5). Everything except the model's choices is
  verified in the guest under TCG. The choices are verified on the host with
  the same engine code (`cargo xtask assist-check`) and in the guest only
  with an accelerator; without one that case is reported `SKIP`, never `OK`.

## 2. Processes

| Process | Role | Holds |
| --- | --- | --- |
| Assistant (new) | The "Assistant" window: input line, status line, transcript, confirmations | window channel; a home handle that it moves into OtterHandler unused |
| ElephantHandler (service mode) | Maps the model; answers `choose <request>` with a tool and argument; sends progress; obeys `cancel` | one channel — **no directory handle** |
| OtterHandler | Runs the tools on home (M5c-1) | the home directory handle |
| PeacockHandler | Window server; gives each Assistant start a fresh copy of its home handle | home handle |
| Zookeeper (session) | Takes `fs_root`, narrows it to `home/user`, closes the root, passes a copy to each window-server start | home handle |

Assistant starts OtterHandler and ElephantHandler on first use (so
process ids at desktop start do not change with them). The model process
can never reach files.

## 3. Guided decoding (`infer/src/assist.rs`)

1. **Tool:** after the prompt, compare the next-token scores of the first
   token of `search`, `list`, `inspect` and `create`; pick the highest. The
   difference to the runner-up is reported as the margin.
2. **Argument:** feed the tool word, then generate greedily with every token
   restricted to path characters (`[A-Za-z0-9 ._/-]`), stopping when the
   unrestricted best token would end the line or the turn, or after 24
   tokens.
3. **Typed commands** (`search …`, `list …`, `inspect …`, `create …`,
   `undo`) skip the model and go straight to the broker; requests in other
   words go to the model.
4. **`create` always asks:** Assistant first shows OtterHandler's `plan`
   and runs `create` only after `y`. `undo` is available as a command.
5. OtterHandler's `search` matches when every word of the query appears in
   an entry's path or (for files) its contents, so a model answer like
   `pancake recipe` finds `Documents/recipes/pancakes.txt`.

## 4. Cancellation and budgets

- ElephantHandler checks its channel with `try_recv` before every token
  (system prompt, request and argument); `cancel` ends the request and it
  answers `err cancelled`. Backspace on the Assistant sends `cancel` while
  it is thinking. A cancelled system-prompt set-up is redone on the next
  request.
- Budgets: request ≤ 200 bytes and ≤ 64 tokens (`err request too long`),
  argument ≤ 24 tokens, a context sized for exactly that, and a deadline of
  60,000 ticks (10 min) per request (`err timed out`). Progress lines
  (`busy setup k/n`, `busy reading k/n`, `busy writing k`) are dropped
  rather than waited for when the window's queue is full.
- Failures are shown as text: model not installed, cancelled, timed out,
  broker errors with their reason.

## 5. Verification

| Check | Where | Default verify? |
| --- | --- | --- |
| `assistant`: typed commands, search result, plan + `y` before create, undo, not found, path escape refused, missing model reported | guest, TCG, normal image | yes |
| `assistant-model`: while ElephantHandler computes, Files takes focus and moves its selection (screenshot shows the Assistant still thinking); Backspace cancels and the window is ready again | guest, TCG, model image | yes (needs the model) |
| `assistant-live`: two requests in words → model → broker → result | guest, model image | only with `--accel`; otherwise `SKIP` |
| Model picks the right tool and argument for 8 fixed requests | host, `cargo xtask assist-check` (same engine code) | separate command |

Negative controls are recorded in docs/verification.md.

## 6. Deferred

Multi-turn conversation, answers in prose, more tools (reading file
contents to the user, moving or renaming), a confidence threshold on the
margin, speech.
