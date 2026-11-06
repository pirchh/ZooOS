# Conversation, step 1: she speaks and listens (2026-11-06)

Status: implemented (version 0.131.0). The owner wants Bella to be more
like Jarvis than like Siri: always listening, and able to tell when she
is being talked to. The order agreed on 11-06:

1. her voice (docs/speaking-design.md, 0.130.0);
2. **conversation mode: she keeps listening around her own voice and
   can be interrupted** (this step);
3. deciding from meaning, not only her name, whether speech is for her
   (next);
4. later, speaking up herself.

## 1. Her own voice (half-duplex)

On a real machine her voice reaches the microphone. Before this step
BatHandler wrote it down like anyone's speech. It was never taken as a
request, since it doesn't start with her name. But it was kept for 3 s
as "said just before" (docs/late-design.md). When the follow-up window
opened as she finished, it was passed on as the follow-up, rejected as
"not a request", and the window was used up. Your real follow-up a
moment later then needed "hey Bella" again.

Now:

- Bella sends BatHandler `mute` with her first `say:`, and `unmute`
  when MynahHandler says `quiet` (nothing left to say).
- Speech that began while muted, or within **500 ms** after
  (`TAIL_US`: the device plays 200 ms after the last write, and a room
  echoes), is taken as her voice. It is logged as
  `speech while she spoke (N ms at L dB), taken as her voice`, never in
  words. It is not kept for later, and it uses no window.
- A window that opened while she spoke (her question's 10 s for an
  answer) counts from when she finished.
- The pill shows working while she speaks. The follow-up window
  (docs/follow-design.md, 8 s) opens when she has finished speaking,
  not when the action is done.

There is no echo cancellation. While she speaks, only speech that
clearly starts with her name, or tells her to stop, gets through
(below).

## 2. Interrupting her (barge-in)

What BatHandler still passes on while she speaks, as `barge:<text>`:

| Heard while she speaks | What Bella does |
| --- | --- |
| "Bella, stop.", "stop", "be quiet", "that's enough", "never mind" (`zoo_abi::bella::stop_talking`) | stops; nothing else |
| "Bella." (her name alone) | stops; listens 5 s for the request |
| "Hey Bella, open files." (`addressed`) | stops; takes the request |
| anything else | nothing: taken as her voice |

To stop her, Bella sends MynahHandler `stop`. MynahHandler checks its
channel while it plays, between writes, and after each sentence is made.
On `stop` it drops what was queued, logs
`stopped, N of M ms said` (or `stopped before saying …`) and says
`quiet`. Up to 200 ms already queued in the device still plays.

## 3. Verified

| Check | Result |
| --- | --- |
| `voice-barge`: "Hey Bella, open notes and maximize it.", then "Bella, stop." while she says "Notes fills the screen." | `OK`: `stopped, 1000 of 2125 ms said`; `said it` never logged |
| The same with Bella not sending `stop` (**negative control**) | `FAIL`: no `MynahHandler: stopped` |
| `voice-echo`: the request, "I think it is raining again." while she speaks, then "Split the screen into quarters." after | `OK`: the talk taken as her voice, the split taken as a follow-up; "raining" and "not a request" in no log line |
| The same with Bella not sending `mute` (**negative control**) | `FAIL`: `heard speech after her answer (… just before)` → `not a request (6 words)`; the window is used up |
| `stop_talking` unit test (`zoo-abi`) | pass |

Times and the suites are in docs/verification.md.

## 4. Not done

- **A plain "yes" while she is still asking** is taken as her voice.
  Answer once she has finished, or say "Bella, yes".
- **No echo cancellation.** On a real machine, loud speakers near the
  microphone could make her own voice sound like "Bella …". She rarely
  says her own name, and this is untested on real hardware (deferred by
  the owner).
- **Talk said over her** that is not addressed to her is lost, by
  design.
- Telling whether speech is for her from its meaning is the next step.
