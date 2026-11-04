# Listening for "hey Bella" (2026-11-04)

Status: implemented and verified in QEMU. Step 3 of the voice plan's order
(docs/voice-plan.md §6): voice detection, the "hey Bella" gate, and Bella
acting on what she hears. Bella listens when a hearing model is installed
(today: the hearing test image) and the machine has a microphone (today:
QEMU's virtio-sound). She does not answer aloud yet (Kokoro is next).

## 1. The path

    microphone ──AUDIO_READ──▶ BatHandler: voice detection ─▶ Moonshine ─▶ "hey Bella"?
                                                                              │ yes
                                                                              ▼
                                Bella ◀──── heard:<text> ─────────────────────┘
                                  │  as if typed: steps, "it", window actions
                                  ▼
                              PeacockHandler

- **BatHandler** (`user/src/bin/bat-handler.rs`) is Bella's hearing.
  1. Bella starts it with a channel, after her other helpers, and only when
     a hearing model is installed (`models/hear.gguf` in the boot image,
     or model 2 on the model disk). Desktops without one start exactly the
     processes they did before.
  2. It reads the microphone first and owns it from then on
     (docs/microphone-design.md). Without a microphone it says
     `none:no microphone` and ends before reading 108 MB of model.
  3. It loads Moonshine, starts a worker per other processor, reads the
     microphone every 20 ms, and feeds the voice detector.
  4. It transcribes each utterance and logs it.
  5. It sends `heard:<text>` only when `addressed` says the utterance
     speaks to Bella.
  6. It ends when Bella's channel closes, which releases the microphone.
- **Bella** (`hearing_said`):
  - She logs `listening for "hey Bella"`.
  - She trims the sentence's closing mark (".", ",", "!", ";").
  - She shows "You said: ..." and submits it as typed: the address is
    dropped, then steps, "it" and window actions follow.
  - When she is busy, she says she heard but is still working, rather
    than dropping the request silently.

## 2. Voice detection (`infer/src/listen.rs`)

No model: Silero VAD (approved) is ONNX-only and would need an ONNX
runtime of our own. An energy detector needs nothing, can be tested
exactly, and is enough for a quiet room. It is replaced if real rooms need
more (DECISIONS.md, "Listening").

| Rule | Value |
| --- | --- |
| Frame | 20 ms (320 samples) |
| Speech frame | 12 dB above the background and above −50 dBFS |
| Background | the quietest frame of the last 3 s, leaving out digital silence (below −90 dBFS) (since 0.114.0, §6) |
| Start | 3 speech frames in a row; the utterance keeps the 200 ms before |
| End | 700 ms without speech (pauses between words stay inside); at most 200 ms of that tail is kept |
| Limits | 12 s at most; fewer than 10 speech frames (200 ms) is a click, dropped |

Unit tests (`cargo test -p zoo-infer listen`) cover:
- one burst is one utterance of the right length;
- silence and hiss give nothing;
- a 400 ms pause stays inside one utterance;
- two requests 1.5 s apart are two;
- a 100 ms click is dropped;
- steady noise gives at most one utterance and then nothing.

The first version failed two of these, which is how two mistakes were
found. It counted the 200 ms before a click as the click's length. And its
background stopped following the level during speech, so a fan restarted
an utterance every 12 s.

On real speech, `cargo xtask hear-check` puts every recording between 1 s
and 2 s of silence. The detector must find exactly one utterance, and
Moonshine must hear it as said. All 8 pass, with utterances of 2440 to
2880 ms.

## 3. The gate (`zoo_abi::bella::addressed`)

- **Form:** an optional greeting (hey, hi, ok, okay, hello), then "Bella",
  then the request, with any punctuation between. Moonshine writes both
  "Hey Bella," and "Hey, Bella,", and the first version
  (`strip_address`, matching "hey bella" as one string) missed the second.
- **Rejected:** "hey bella" alone, "Hey Bellamy, ...", "hey there bella
  ...", and anything not starting with her name.
- `strip_address` now uses it, so typed and spoken requests drop the
  address the same way.
- `hear-check` also requires the gate to pass exactly the two "Hey
  Bella" recordings of the eight.
- **Numbers:** `same_words` treats the numbers zero to twelve as words or
  digits, because Moonshine wrote "monitor three" for the trimmed
  utterance and "monitor 3" for the padded file.

## 4. Verified

| Case | Result |
| --- | --- |
| `voice-bella` (TCG) | `OK`: BatHandler heard "Hey, Bella, open notes and maximize it." (2800 ms of speech, 10.7 s to hear); Bella: 2 steps, open notes / maximize Notes; "Notes fills the screen." |
| `voice-bella` (WHPX, 8 processors) | `OK`: 103 ms to hear |
| `voice-not-for-bella` | `OK`: heard "Put notes on the left and files on the right," then "not for Bella"; Bella never heard it and nothing was placed |
| `voice-not-for-bella` with the gate passing everything (negative control) | `FAIL`: "`Bella: heard` appeared, and must not" |

**From the end of speech to the request reaching Bella:** the detector's
700 ms wait plus the time to hear, 0.1 s on 8 processors. That's about
0.8 s, derived from these two parts; the logs carry no timestamps to
measure it end to end.

## 5. Trying it with your own voice (`run --mic`, 0.113.0)

    cargo xtask run --mic --accel whpx --cpus 8

This boots the everyday desktop in QEMU, with the hearing model in its
boot image and Bella's chat model on the model disk. QEMU's sound device
takes its microphone from this PC's default recording device, through SDL
(`-audiodev sdl,in.mixing-engine=off,...`, the path the `mic-` cases
checked).

- Say "Hey Bella, open notes and maximize it".
- What BatHandler hears is printed on the terminal (`BatHandler: heard
  ...`).
- Nothing is saved: the serial output of `run` goes to the terminal only.
- The microphone is open only while QEMU runs, and only for `run --mic`.
  `verify` never uses it.
- This is QEMU only: nothing boots on the PC itself, and its disks and
  Windows are not touched.

Checked without opening the real microphone. The same command was run
with `SDL_AUDIODRIVER=disk` and `SDL_DISKAUDIOFILEIN` set to the test
recording, so SDL read the file instead:
- QEMU started with the sound device;
- BatHandler listened with 8 threads;
- it heard "Hey, Bella, open notes and maximize it." in 152 ms;
- Bella reported "Notes fills the screen."

**Not yet tried with a person's voice or a real microphone.** A room's
noise, distance from the microphone and a person's voice may need the
detector's levels changed (§2). A PC with no recording device is not
tested either.

## 6. The first real voice (0.114.0)

The owner tried `run --mic` and said "hey Bella, maximize my notes"
several times. Nothing happened. BatHandler had heard:

| Heard | What it was |
| --- | --- |
| "So" (11920 ms) | room noise taken as speech, cut at the 12 s limit |
| "Abele maximize my notes." (3740 ms) | the owner, with "hey Bella" lost, so not for Bella |
| "????…" (12000 ms) | room noise again |
| "You" (1820 ms) | noise |

**The cause was the detector's background level.**
- The microphone starts with digital silence, and the first version took
  that as the background (about −120 dB).
- The room's noise then looked like speech, and the background could rise
  only 2.5 dB a second, so it took about 25 s to catch up.
- Meanwhile the detector stayed "in speech" until the 12 s limit, and the
  soft start of "hey Bella" was lost.
- The test recordings have exact silence around speech, so no case met
  this.

**Fixes:**
- The background is now the quietest frame of the last 3 seconds, with
  digital silence (below −90 dBFS) left out. Speech dips toward the room's
  level between syllables, so the background holds during speech. Steady
  noise, such as a fan turned up, becomes background within 3 s.
- The gate accepts a word one letter from "Bella" after a greeting ("hey
  Bela", "hey Bello"). Without a greeting the name must be exact.
- BatHandler logs `room level N dB` when the background settles and when
  it shifts by 6 dB. Each `heard` line gives the utterance's level, so a
  real microphone's numbers can be compared with the detector's
  thresholds.

**Tests that reproduce it:**
- **Unit tests:** `digital_silence_then_room_noise` (300 ms of zeros, room
  noise at −45 dB, a request, more noise) and `louder_room_becomes_background`
  (−60 dB, then −40 dB). Both failed on the old detector, with no
  utterance ever ending and 3940 ms of noise taken as speech; both pass
  now.
- **`hear-check`:** it also puts every recording in a noisy room
  (`in_room`: 300 ms of digital silence, then −45 dB noise under
  everything). There must be one utterance, no longer than the recording
  plus 0.5 s, heard as said. It passes, with utterances of 2380–2800 ms.
  With the old detector it fails: the utterances run on to 6.6–7.1 s.
- **QEMU cases:** the `voice-` cases now hear that noisy room through the
  microphone. Measured: `room level -45 dB`, "Hey Bella, open notes and
  maximize it." at −19 dB, Notes maximized.

Still to confirm with the owner's voice (`run --mic`).

## 7. The second try (0.114.0)

With the new detector the owner's speech came through as sensible
utterances: about −40 dB against a room at −60 to −87 dB. "hey Bella,
maximize my notes" still did nothing. It came out as two utterances,
"They bellow," and "Maximize my notes.", because the owner paused after
the name. Three more problems showed in the same session:

| Found | Fix |
| --- | --- |
| A pause after her name splits the request; "hey" heard as "they" | `name_only`: her name alone makes BatHandler take the next utterance within 5 s as the request (`heard ... after her name`). Greetings include "they", "hay", "hei", "eh". After a greeting, up to two letters off "Bella" count for words of 4–6 letters ("bellow", "villa"), one for 7 ("Bellamy" does not). "hello", "below", "yellow" and other real words never count |
| BatHandler logged every utterance's words, including conversation not meant for Bella | **Speech not for Bella is never logged as words**, only `speech not for Bella (N ms at L dB)`. Only requests and her name are logged as text |
| While BatHandler transcribed, the device's 10 receive buffers (400 ms) filled, and the rest was lost, here the request after her name | **The timer tick drains the device into the 8 s ring** (`virtio_sound::tick`, `try_lock`, no allocation), so recording goes on while the reader is busy |
| Ctrl+C in the terminal ended xtask but not QEMU: the microphone stayed open after the run was "stopped" | **Every child of xtask is in a job object that ends it when xtask ends** (`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, `xtask/src/process.rs`) |

Verified:
- **`voice-pause` (new):** the microphone plays "Hey Bella.", a second of
  room noise, then "Open notes and maximize it.". BatHandler heard her name
  ("Hey villa"), then the request "after her name", and Bella maximized
  Notes. It failed before the tick drain: the request was never heard.
- **`voice-not-for-bella`:** now passes with `speech not for Bella (2500 ms
  at -18 dB)` and no words.
- **QEMU cleanup:** xtask was killed outright during `run`. With the job
  object, QEMU ended with it. With the old guard (negative control), QEMU
  kept running.

## Not done

- Bella answering aloud (Kokoro).
- The hearing model on the everyday model disk (`run --mic` carries it in
  the boot image instead). Desktop cases count on fixed process numbers,
  so this waits for its own step.
- Intel HD Audio for real PCs; Silero VAD for noisy rooms.
- Telling a request apart from Bella's own voice once she speaks, for
  example by not listening while she plays.
