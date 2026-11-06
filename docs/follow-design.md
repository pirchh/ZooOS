# Follow-ups without her name (2026-11-05)

Status: implemented (version 0.124.0). The owner wants Bella to work like
Siri, and the review listed "follow-up as a conversation state":
after she does a spoken request, the next command needn't start with
"hey Bella".

## The window

When a spoken request is done (the pill would go), Bella tells BatHandler
`follow`. For **8 s** the next utterance is sent to her as `follow:`
instead of being dropped as not for her. The pill shows listening while
the window is open. There is one window per request: a follow-up that is
accepted opens the next.

## Only commands, and no words in the log until they are hers

What follows an answer may be someone talking in the room, so a
follow-up is taken only when it is something Bella does without the
model: every step a typed command, an action in the registry's words, or
a layout (`zoo_infer::route::needs_no_model`).

| After "Hey Bella, open notes." | Result |
| --- | --- |
| "Maximize notes." | taken: Notes fills the screen |
| "I think it is raining again." | not for her: `speech after her answer, not a request (6 words)` |
| "What time is it?", "thank you" | not for her (unit tests) |

**Privacy:**

- BatHandler logs a possible follow-up without its words: `heard speech
  after her answer (N ms at L dB)`.
- Bella logs the words only once she takes them.
- She doesn't ask her memory about a follow-up. `voice-follow-chat`
  showed that a lookup puts the words in OtterHandler's audit log, so a
  routine's name as a follow-up still needs "hey Bella".

## Verified

| Check | Result |
| --- | --- |
| `voice-follow`: "Hey Bella, open notes." → Notes opened → `listening for a follow-up` → "Maximize notes." taken without her name → Notes fills the screen | `OK` |
| `voice-follow-chat`: then "I think it is raining again." → not a request, 6 words; nothing done; "raining" in no log line | `OK` |
| No `follow` sent (**negative control**) | `FAIL`: no speech after her answer |
| The gate always yes (**negative control**) | `FAIL`: the talk is taken as a request |
| `voice-pause`, `voice-confirm`, `voice-not-for-bella`, `voice-bella`, `mic-hear` | `OK` |
| `route::follow_ups` unit test | pass |

## Not done

- Follow-ups that need the model ("and what about Photos?"). They would
  need the model to judge whether the words are for her before they are
  logged.
- Endpointing: the end of speech is still 700 ms of quiet.
