# Bella answers as she writes

Status: implemented in version 0.93.0. The owner asked for 4–14 s replies
to feel shorter: an instant "hang tight" line, and the reply shown word by
word.

## Replies shown as they are written

- **`TalkStep::Partial { text }`.** `Talk::generate` (`infer/src/talk.rs`)
  reports the reply so far after each token, cleaned as the final reply
  is.
- **`part <text>` messages.** ElephantHandler sends each one to Bella as
  progress. Like the `busy` lines, it is dropped when Bella's queue is
  full. Each one carries the whole reply so far, so a dropped one loses
  nothing.
- **One transcript line.** Bella writes the first `part` as a new line
  and rewrites that line for each later one. The final `say` replaces it
  with the finished reply. If the model service ends mid-reply, the
  partial line stays, and the next reply starts a new line.
- **Timing lines:**
  - ElephantHandler: `said … in N ms (first words at M ms)`;
  - Bella: `first words on screen N ms after Enter`.

## Said at once

A request Bella knows will take the model a while gets one line before
any work starts:

| Request | Lines, taken in turn |
| --- | --- |
| Summarise a file | "Let me read that file, one moment...", "Reading it now, hang tight...", "On it, reading the file..." |
| What is on the screen | "Let me look at your screen...", "One sec, checking what's open...", "Looking at your windows..." |

They are taken in turn (`App::acks`), so the same line does not come
twice in a row. Each is logged as `Bella: said first: …`. Replies in words
get no such line: with the default 0.5B model their first words arrive
within a second (below).

## Measured (WHPX)

From Enter to the first words on Bella's screen, and to the whole reply.
The model service's own `first words at` is 20–1000 ms earlier.

| Request | 0.5B first words | 0.5B whole | 1.5B first words | 1.5B whole | Qwen3-1.7B first words | Qwen3-1.7B whole |
| --- | --- | --- | --- | --- | --- | --- |
| Small talk ("who are you?") | 0.34 s | 0.78 s | 1.0 s | 3.5 s | 1.8 s | 5.2 s |
| A joke | 0.62 s | 0.84 s | 2.1 s | 6.4 s | 3.6 s | — |
| From a fact (the dentist) | 0.74 s | 0.94 s | 2.5 s | 4.3 s | 3.7 s | 5.5 s |
| What am I working on? | 1.6 s | 2.2 s | 5.3 s | 8.7 s | 8.5 s | 13.7 s |
| Summarise `Documents/notes.txt` | 2.2 s | 2.2 s | 7.7 s | 11.9 s | 10.3 s | 14.8 s |

The first words wait for the request to be read: the longer the request
(a screen snapshot, a file), the later they come. The instant line covers
that wait for those two.

## Checks

- **`bella-summary`** (TCG): `said first: Let me read that file, one
  moment...`.
- **`bella-context`** (TCG): `said first: Let me look at your screen...`.
- **`bella-summary-answer`** (WHPX): `first words on screen` before the
  summary is finished.
