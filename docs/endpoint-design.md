# Ending at a pause (2026-11-06)

Status: implemented (version 0.127.0). Before this, an utterance ended
only after 700 ms of quiet (docs/listening-design.md §2), so every spoken
request waited 700 ms after the last word before it was written down.
That wait keeps pauses between words inside one utterance. It's also the
largest fixed part of Bella's reply time on a fast processor, where
writing down takes about 0.2 s.

## 1. What changed

- **The detector** (`zoo_infer::listen`) offers the speech so far as a
  `Heard::Pause` at 300 ms of quiet (`PAUSE_FRAMES`). The utterance goes
  on, and still ends as before at 700 ms, unless BatHandler takes it at the
  pause and calls `cut`.
- **BatHandler** writes down the speech at a pause only when that should
  take at most 400 ms (`TRY_AT_PAUSE_US`), judged by the last utterance's
  rate in µs per second of speech. 400 ms is what the end would still
  wait. The microphone keeps recording meanwhile, so a try that is not
  taken costs next to nothing. Under QEMU's emulated processor (seconds to
  hear) it is never tried, so nothing changes there. The very first
  utterance has no rate yet and waits as before.
- **Taken** only when the text is for her (her name, or her window after
  her name, question or answer) and **sounds finished**
  (`zoo_abi::bella::sounds_finished`):
  - it ends with `.`, `!` or `?`, not `,`;
  - its last word isn't one a sentence doesn't end on ("and", "the",
    "to", "my", …);
  - it isn't her name alone, because the request follows that.
- **Otherwise** the end decides, as before. Speech not for her is never
  taken at a pause, and the words of a try are never logged: only
  `not over at a pause (N ms to hear)`.

## 2. Measured (WHPX, 8 processors)

| | Before | After |
| --- | --- | --- |
| Quiet waited after the last word | 700 ms | 300 ms when taken at the pause |
| `voice-quick`: "Open notes and maximize it." after her name | the end at 700 ms | taken at the pause in 3 of 3 runs; 187, 226, 232 ms to hear |
| From the last word to Bella (quiet + hearing) | about 0.9 s | about 0.5 s |

The last row is derived from the two logged parts, as in
docs/listening-design.md §4. Serial lines carry no timestamps to measure
it end to end.

Under WHPX, `voice-follow-chat`'s follow-up and `voice-pause`'s request
were taken at the pause too. `voice-confirm`'s "Yes," and `voice-follow`'s
"Hey billah open notes" were not: one ends with a comma, the other has no
mark. Both waited for the end and passed.

**`hear-check`, the 8 recordings:** at each 300 ms pause the speech so
far must not sound finished unless it is the whole sentence. 6 would be
taken whole; 2 ("…on the right," and "…monitor three,") wait for the end.
None was cut short.

## 3. The risk, and why it is accepted

A speaker who stops for 300–700 ms after a whole-sounding clause ("Hey
Bella, open notes. … and maximize it") now gets two utterances, not one.
The second still comes in her follow-up window (docs/follow-design.md),
and with BatHandler's hold (docs/late-design.md), so a command there is
still taken, but as its own request. The recordings have no pause that
long inside a sentence, so this case is covered by the unit tests only.

## 4. Verified

See docs/verification.md.

## Not done

- A model for the end of speech (a voice-activity or turn-taking model)
  instead of the loudness detector and the word list.
- Words in other languages in the list of words a sentence doesn't end on.
