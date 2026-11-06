# Bella speaks: MynahHandler (2026-11-06)

Status: implemented (version 0.130.0). Bella says her answers to spoken
requests aloud, with the Kokoro engine and the word lists of
docs/speech-design.md. Typed requests stay silent.

## 1. The service

**MynahHandler** (`user/src/bin/mynah-handler.rs`; mynah birds copy
human speech) is started by Bella, with a channel, when a voice model is
installed:

- in the boot image as `models/speak.gguf` (the hearing images with her
  voice, and `run --mic`);
- or on the model disk as model 3 (`MODEL_SPEAK`, read whole into
  memory like the hearing model). The model disk does not carry it yet
  (§5).

It maps the model, reads the word lists from it and starts a worker pool
on the other processors. Then it says `ready`, and for each
`say:<text>`:

1. text to phonemes (`zoo_infer::g2p`);
2. phonemes to 24 kHz speech (`zoo_infer::speak`);
3. 24 kHz mono to the device's 48 kHz 16-bit stereo (each sample, then
   the point halfway to the next, on both channels);
4. `AUDIO_WRITE` until all is queued, a tick's sleep when the device is
   full (docs/audio-design.md);
5. `said` back.

`none:<why>` and an exit when there is no voice model or no sound
device. Its log has the words: they are Bella's, already in her window
and her log.

## 2. What Bella says aloud

Only when the request was spoken (`spoken`, as for the pill,
docs/wake-design.md) and her voice is ready:

- the window server's answer ("Notes fills the screen.");
- an answer in words from the model;
- her questions ("Do you want me to close Network?", a delete, a
  routine, forgetting a phrase);
- "Okay, I won't."

Each is logged as `Bella: says aloud: …`. Lines that only show what she
is doing (`Bella: maximize Notes`, "You said: …") are not spoken.

## 3. The check (`voice-speak`)

- The hearing image with her voice. The microphone plays "Hey Bella,
  open notes and maximize it.", as `voice-bella` does.
- The sound device's output is what SDL's disk driver writes next to
  the logs (`sdl-out.raw`, 48 kHz 16-bit stereo): the same device whose
  microphone plays the sentence.
- After the run, the host resamples it to 16 kHz and cuts it into
  utterances with the voice detector. The Moonshine engine writes each
  down, and one must be "Notes fills the screen."
- Negative control: MynahHandler playing silence. Every serial step
  still passes, and the case fails with `audio-mismatch`.

This needs an accelerator: under TCG her voice takes minutes.

## 4. Verified

On one guest processor under WHPX, "Notes fills the screen." (2.1 s of
speech) is made in 2.6–3.0 s and has finished playing about 5 s after
Bella asked for it. Details in docs/verification.md.

## 5. Not done

- **Speed** (docs/speech-design.md §4): she starts speaking only once
  the whole answer is made.
- **The model disk** does not carry the voice yet. `run` without `--mic`
  has no voice.
- **She hears herself.** On a real machine her voice reaches the
  microphone. It is not addressed to her, so it is not taken as a
  request, but BatHandler writes it down. Next: BatHandler stops
  listening while she speaks, and she stops when interrupted (barge-in,
  the conversation step).
- One voice (`af_bella`), American English.
