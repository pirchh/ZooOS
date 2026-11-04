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
| Background | follows quieter frames down at once, rises 0.05 dB a frame (2.5 dB/s) in every frame |
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

## Not done

- Bella answering aloud (Kokoro).
- The hearing model on the everyday model disk, and a microphone in
  `cargo xtask run`. Desktop cases count on fixed process numbers, so this
  waits for its own step.
- Intel HD Audio for real PCs; Silero VAD for noisy rooms.
- Telling a request apart from Bella's own voice once she speaks, for
  example by not listening while she plays.
