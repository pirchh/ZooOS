# Staged wake and the listening pill (2026-11-04)

Status: implemented (version 0.120.0). This is step 4 of the order from
the outside design review the owner shared. The owner asked for Bella to
work like Siri: any greeting plus her name wakes her, and something
Apple-like at the top right shows she is listening, not a microphone
icon.

## 1. The stages

| Stage | What decides it | What shows |
| --- | --- | --- |
| Speech | BatHandler's detector (docs/listening-design.md) | nothing; words not logged |
| Possible: her name alone | `zoo_abi::bella::name_only` | the pill, **listening**, for 5 s |
| Accepted: a request said to her | `addressed`, or anything in the 5 s after her name or 10 s after her question | the pill, **working** |
| Authorized: what she does | `zoo_infer::route` and the registry (docs/regression-design.md); a delete still asks | the pill goes when she is done |

## 2. The gate, sharper

- **About her, not to her.** Her bare name followed straight away by
  "said", "is", "and", "thinks" and the like ("Bella said she would call
  later", "Bella and I went to the shop") is not a request. With a
  greeting before her name or a mark after it ("Bella, is it raining?")
  it still is. "can", "could", "will" and "would" are not on the list:
  "Bella can you open notes" is a request.
- **Her name later in the utterance.** Speech to text joins what came
  before into one utterance: "That's done now. Bella, open notes." Her
  name counts after a clause boundary (`.` `,` `!` `?` `;`) when it is
  set off, by a greeting before it or a mark after it. "I told her, Bella
  would open it" and "I told Bella to open the notes" stay not for her.
- **Commands in any case.** Speech to text writes "Open notes": the
  command check ignores case, and Bella runs the command word in lower
  case.
- **"Abele"** is three letters from "Bella", too far to take without a
  greeting; it stays a recorded gap.

## 3. The pill

A dark capsule (`PILL_FILL`) at the top centre of the menu bar
(`zoo_abi::ui::pill_rect`), like Apple's island, with five bars in Bella's
colours, rose to cyan (`PILL_BARS`). It has no microphone icon and no text.

- **Listening:** every bar rises and falls out of step, like a voice.
- **Working:** one light sweeps across the bars; the others are dimmed.
- Drawn by the window server (PeacockHandler) at a 30 ms step, and only
  the pill's rectangle is redrawn. When the pill is up, the server wakes
  every 30 ms instead of once a second.

**Who decides what it shows:** Bella, from her own state (`update_pill`):

- **Listening:** after her name until BatHandler's 5 s window ends, and
  while a question waits for a spoken answer.
- **Working:** from "hearing" (below) until her request is done, for at
  least 1 s so a quick one is seen.
- **Off:** otherwise.

It is only ever for spoken requests: typed ones never show it. The window
server takes `pill:` messages from Bella only.

- **"hearing":** after her name, BatHandler tells Bella when it starts
  writing down what followed, and "lost" if it can't. The pill goes
  straight to working, not off, while a long request is transcribed
  (8 s on QEMU's emulated processor).
- **Kept up while she works:** Bella sends the state again every 4 s.
  The window server takes the pill down after 10 s without word from her,
  so it cannot stay up if Bella stops.

## 4. A wrong lesson fixed

In `voice-pause`, "Open notes" (capital O) wasn't recognised as a typed
command, so it went unanswered. Then step two, "maximize Notes", was
taken as the user's correction, and she *learned* `Open notes` →
`maximize Notes`. Two fixes:

- commands in any case (above);
- **a step of a request of several is never a correction.**

`voice-pause` now checks that nothing is learned.

## 5. Verified

| Check | Result |
| --- | --- |
| `voice-pause`: pill Listening → screenshot → Working → done → Off; never timed out; nothing learned | `OK`, three runs out of three; screen: capsule `[11, 11, 16]`, rose and cyan bars; after: no pill |
| `voice-pause` with the pill not drawn (**negative control**) | `FAIL`: `screen 1-listening: no pill` |
| `voice-not-for-bella`: speech without her name | `OK`, and no `pill` line at all |
| `bella-notes`: typed requests | `OK`, and no `pill` line at all |
| `voice-confirm` | `OK` |
| `bella-regress` | "Bella said she would call later" changed to not for her; new sentences recorded; 3 known gaps |
| Unit tests (`zoo-abi`): about her, a set-off name later in the utterance, commands in any case | pass |

## Not done

- Endpointing: a whole-sounding request ends at a 300 ms pause on a fast processor (docs/endpoint-design.md, 0.127.0).
- Follow-ups without her name are in docs/follow-design.md (0.124.0).
- **Barge-in:** stopping her mid-answer by voice.
- The pill doesn't show her hearing level, and doesn't appear on other
  monitors.
