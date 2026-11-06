# Answers said quickly (2026-11-06)

Status: implemented (version 0.126.0). Running the suite under WHPX
(docs/refine-design.md) failed `voice-confirm`, `voice-follow` and
`voice-follow-chat`. Their recordings waited 12 s before the answer,
tuned for TCG, where writing down the request takes about 9 s. Under WHPX,
Bella asked within a second, and the answer came after her window had
closed.

The real problem was the reverse case. **On a slow processor, an answer
said quickly was lost.** BatHandler writes down the request, and Bella
asks her question (`await`) only once it is written down. A "yes" said
during those seconds is heard before the window opens, so it counted as
not for her.

## The fix

BatHandler keeps the last sentence not for her, in memory only and never
logged, for `LATE_US` (3 s) after it was written down. When Bella opens a
window (`await` for an answer, `follow` for a follow-up) within that
time, the sentence counts as said in it:

- an answer: `heard "Yes," as her answer (said just before; …)`;
- a follow-up: `heard speech after her answer (…, just before)`, still
  without its words, and still only taken if it is a command
  (docs/follow-design.md).

Anything said to her in between makes the kept sentence stale. After
3 s it is dropped.

The voice recordings now wait 3 s, and the same recording passes both
ways:

| Case | TCG | WHPX |
| --- | --- | --- |
| `voice-confirm` | `OK`, through the kept sentence | `OK`, inside the window |
| `voice-follow` | `OK`, kept | `OK` |
| `voice-follow-chat` | `OK`, kept, then not a request | `OK` |
| `voice-confirm` with `LATE_US` 0 (**negative control**) | `FAIL`: no "yes" heard | — |
| `voice-not-for-bella`, `voice-pause`, `voice-bella` | `OK` | — |

## Privacy

The kept sentence is the same text BatHandler had already transcribed. It
stays in memory for at most 3 s and is logged only if her window takes
it, as before.
