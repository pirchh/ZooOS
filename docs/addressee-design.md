# Conversation, step 2: is it said to her? (2026-11-06)

Status: **measured, not used by Bella** (version 0.132.0). The judge and
its measurement are in the tree for the model bake-off. Bella's
behaviour is unchanged from 0.131.0 (docs/conversation-design.md).

## 1. The question

After Bella answers, the follow-up window (docs/follow-design.md, 8 s)
takes speech without her name only when today's gate,
`zoo_infer::route::needs_no_model`, can act on it without the model:
"Maximize it.", "Close Notes." Anything else, such as "And what about
Photos?" or "What time is it?", is `not a request`, and needs "Hey Bella"
again. Step 2 was to let the model decide, from what the sentence means,
whether it was said to her. Speech that is not for her must still never
be logged in words, so a judge that takes talk would break that rule.

`tests/bella/addressee.txt` has 18 sentences said to her that the gate
doesn't take and 16 of talk in the room, each after an answer she said
aloud. It was written for this; nothing is copied from the owner's
demos. `cargo xtask bella-addressee` runs the judge on each on the host
and reports both groups, the time, and the score margins.

## 2. What was measured

Qwen3-1.7B Q4_0 (the model on the image), host engine `avx2`, all 34
sentences each time.

| Method | Said to her taken | Talk taken | Time per sentence |
| --- | --- | --- | --- |
| Today's gate alone | 1 of 18 | 0 of 16 | none |
| Ask "is that said to Bella?", score "yes" against "no" | 18 of 18 | 16 of 16 | 2.6–3.5 s |
| The same with what Bella can do, six examples (none from the test file), and the labels "computer"/"person" | 18 of 18 | 16 of 16 | 2.8–4.0 s |
| **Read the sentence twice, as said to her and as said to someone else in the room; compare how likely the words are** | 13 of 18 | 8 of 16 | 7.0–9.6 s |
| Read it once, then compare " Bella" after "The person is talking to" | 18 of 18 | 16 of 16 | 5.7–6.9 s |

Asked directly, the model gives the same answer to every sentence. With
the labels, it put "computer" about 20 above "person" every time, and
its free text after the label echoed the instruction and drifted into
Arabic. A 1.7B model at 4 bits doesn't follow this instruction. Reading
the sentence twice is the only method with a signal (margins: said to
her −2.05 to 9.73, talk −10.17 to 4.77). It still takes half the talk at
0, and strict enough to take no talk (above 4.77) it takes 5 of 18. Such
a cut-off would also be fitted to these 16 sentences.

The time matters too. Each judgment comes before her answer, so 8 s per
sentence would put 8 s of silence before every reply in a conversation.

## 3. Decision

The judge is **not wired into Bella.** `zoo_infer::addressee` keeps the
two-readings method, and `cargo xtask bella-addressee` measures it,
including how much said to her it takes when strict enough to take no
talk. That is the number to compare models by in the bake-off
(approved by the owner on 11-06: Qwen3.5-2B, Gemma 4 E2B and others;
the downloads need the owner's OK). A model is good enough when it takes most of the 18 with none of
the 16, in well under a second.

Until then, the way to talk to her without her name stays as it is:
commands the gate knows, during the follow-up window.

## 4. Not done

- No QEMU case. Nothing in the guest changed except the version.
- Only one model measured.
- The sentences are typed text. Real speech recognition errors would
  make it harder.
