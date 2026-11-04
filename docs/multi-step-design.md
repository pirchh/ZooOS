# Requests of several steps (2026-11-04)

Status: implemented. Second item of the owner's voice-control order
(after window actions and tiling): "open Notes and Files, then minimize
Files", "open notes and maximize it".

## 1. How Bella runs them

1. **Split into steps** (`zoo_infer::steps::split_steps`): at "then",
   "and then", ";" and sentence ends; at "and" or a comma only when the
   next word starts a step (`STEP_VERBS`: open, close, maximize, put,
   find, ...). "split Notes and Files" and "search for flour and sugar"
   stay one step. A leading "hey Bella," is dropped. 2 to `MAX_STEPS` (5).
2. **Each step is a request of its own:** a typed command runs at once,
   anything else goes to the model, as before.
3. **"it", "them", "that", "both" name the apps of the step before**
   (`assist::resolve_it`): "open notes" then "maximize it" →
   `maximize Notes`. When the step before named no app ("find my pancake
   recipe and open it"), the word stays, and the follow-up context
   (docs/talk-design.md §6) gives the model the last exchange.
4. **The next step starts when Bella is back to typing**, after the
   model answered and the command ran. Backspace (cancel) or "n" at a
   confirmation drops the rest; the log says how many were not run.

## 2. App names from the request

As for `split` (docs/tiling-design.md), the apps a request names win over
the model's argument for `launch`, `maximize`, `minimize` and `close`
(`assist::apps_named`): "open Notes and Files" launches both. An `open`
naming two or more apps is a launch of them, not a file called "notes
and files". Typed `launch`, `maximize`, `minimize` and `close` take
"and" too.

## 3. Speed between steps

After each command Bella notes the exchange in the model's conversation
(for follow-ups), and the model reads it before its next choice. Between
steps that cost about 1.2 s: the second step of `bella-steps-model` took
1936–2071 ms against 770–871 ms without it. The note is now sent only
after the last step; the exchange is still kept for a follow-up.

## 4. Verified

| Case | Result |
| --- | --- |
| `bella-steps` (TCG, typed) | `OK`: "launch notes and files, then minimize files" → 2 steps, both launched, Files minimized; "open notes and maximize it" → `Bella: step: maximize Notes`, "Notes fills the screen." |
| `bella-steps` with `resolve_it` not replacing (negative control) | `FAIL`: no `Bella: step: maximize Notes` within 120 s |
| `bella-steps-model` (`--accel whpx --cpus 8`) | `OK` 3 of 3: "hey bella open notes and files, then put them side by side" → ``chose launch `Notes and Files` ``, then ``chose split `Notes and Files` ``; steps 785–861 ms and 770–871 ms |
| `assist-check` (Qwen3) | `PASS`: 32 of 34, 0 of 8 (unchanged) |

## 5. Spoken-style requests, fixed on the way

- **"Hey Bella, ..." was small talk.** `small_talk` takes a first word
  "hey" as a greeting, so "hey bella split these 2 windows" got a reply
  in words. Bella now drops a leading "hey Bella", "Bella,", "ok Bella"
  before routing (`bella::strip_address`); "hey Bella" alone is still a
  greeting. Every voice request will start this way.
- **"can you split these 2 windows" was a question about the last
  answer** ("can" + "these"). "can/could/would/will you" + an action
  word is now a request (`polite_request`); "can you tell me more about
  that?" is still a question.
- With the address dropped, "split these 2 windows" is a typed command
  and runs at once (0 ms).

`bella-tiling-model` now also asks "hey bella can you split these 2
windows": ``chose split `` `` and "Notes is on the left and Crash Test on
the right."

Also fixed: a `split` the model chose with no app named ("split these 2
windows") was treated as "no command fits", because its argument is
empty (docs/tiling-design.md shipped with this; its model case named
apps).

## Not done

- A request typed while the model reads the conversation note of the one
  before waits for it (about 0.8 s). Stopping the note for a command
  choice was tried, but no case types fast enough to reach it; it is left
  for voice, where a whole sentence arrives at once.
- Steps run one after the other; nothing runs in parallel.
- A step that fails does not stop the rest (only cancel and "n" do).
- Conditions ("if it's there, open it") and loops.
