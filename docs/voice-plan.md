# Voice plan: proposal for the owner (2026-11-04)

Status: **approved by the owner on 2026-11-04** as recommended: Moonshine
Tiny, Kokoro-82M (CC BY attribution accepted), Silero VAD; no espeak-ng,
no openWakeWord (DECISIONS.md, "Voice models"). Written as a proposal; the owner's
end goal is "Hey Bella, open Notes and maximize it on my screen", spoken,
with Bella answering aloud (docs/bella-roadmap.md, Stage 9). Window
actions, tiling, several steps, monitors and app actions are done; playback
is done (docs/audio-design.md). What remains needs models, and models need
the owner's approval to download. Researched 2026-11-04; sources are the
model cards and repositories named below.

## 1. What is needed

1. **Hearing:** a microphone driver (recording), voice detection, and
   speech to text.
2. **The wake word** "Hey Bella".
3. **Speaking:** text to speech, played through the sound driver.

All of it local, offline, on the CPU, with weights under Apache-2.0 or
MIT (the owner's rule).

## 2. Speech to text

| Model | Parameters | Licence | LibriSpeech test-clean WER | Files |
| --- | --- | --- | --- | --- |
| **Moonshine Tiny** (Useful Sensors) | 27 M | MIT | 4.52% | ONNX encoder 30.9 MB + decoder 78.2 MB; no GGUF (we would convert) |
| Moonshine Base | 62 M | MIT | 3.23% | ONNX |
| **Whisper base.en** (OpenAI) | 74 M | MIT | 4.2% | `ggml-base.en.bin` 142 MiB (whisper.cpp; `-q8_0` 78 MiB) |
| Whisper tiny.en | 39 M | MIT | 5.6% | 75 MiB (`-q8_0` 42 MiB) |

Both are encoder-decoder transformers our engine could run:
- **Whisper** reads 80-band log-mel spectrograms of 30 s windows. Its files
  are ready in ggml format.
- **Moonshine** reads raw 16 kHz audio of any length, which means less
  front-end code and is faster on short commands, but needs converting.

**Recommendation:** Moonshine Tiny for speed on short commands; Whisper
base.en if converting is a problem.

## 3. Text to speech

| Model | Parameters | Licence | Notes |
| --- | --- | --- | --- |
| **Kokoro-82M** (hexgrad) | 82 M | Apache-2.0, code and weights | 24 kHz. Has a voice named `af_bella`. Text to phonemes with misaki (Apache-2.0); its espeak-ng fallback can be turned off. Training data includes two CC BY sources, so attribution may be due. 327 MB (`.pth`), voice 523 kB |
| Piper (rhasspy) | — | **Avoid:** the current Piper is GPL-3.0, and it needs espeak-ng (GPL-3.0-or-later). Some voices' data forbids commercial use | — |

**Recommendation:** Kokoro-82M with misaki, without espeak-ng.

## 4. Wake word and hearing hardware

- **Wake word:** no extra model.
  1. A voice-activity detector (Silero VAD, about 2 MB, MIT; its licence
     file to be confirmed) decides when someone speaks.
  2. The speech-to-text model transcribes the utterance.
  3. Bella acts when it starts with "hey Bella". She already drops that
     address before reading a request (docs/multi-step-design.md).
- **Rejected:**
  - openWakeWord's pre-trained models (CC BY-NC-SA, non-commercial);
  - Porcupine (proprietary).
- **Hardware:**
  - virtio-sound for QEMU first (done for playback). Recording uses its
    receive queue.
  - Intel HD Audio for real PCs later.
  - QEMU's WAV backend only records output. A microphone test would feed
    a known WAV file to the speech-to-text model directly at first, and
    to the driver later through QEMU's D-Bus audio interface (Windows
    support unverified).

## 5. Downloads that need the owner's approval

| File | Size | Licence | Source |
| --- | --- | --- | --- |
| Moonshine Tiny `encoder_model.onnx`, `decoder_model_merged.onnx` | 31 + 78 MB | MIT | huggingface.co/UsefulSensors/moonshine (onnx/merged/tiny/float) |
| *or* Whisper `ggml-base.en.bin` | 142 MiB | MIT | huggingface.co/ggerganov/whisper.cpp |
| Kokoro `kokoro-v1_0.pth`, `config.json`, `voices/af_bella.pt` | 327 MB + 2 kB + 523 kB | Apache-2.0 | huggingface.co/hexgrad/Kokoro-82M |
| misaki (source and dictionaries) | small | Apache-2.0 (dictionaries to check) | github.com/hexgrad/misaki |
| Silero VAD | about 2 MB | MIT (to confirm) | github.com/snakers4/silero-vad |

**Decisions for the owner:**
1. Moonshine or Whisper.
2. Accept Kokoro's CC BY training-data attribution.
3. Confirm: no GPL espeak-ng, no openWakeWord pre-trained models.

## 6. Order once approved

1. Recording in the virtio-sound driver. **Done** (docs/microphone-design.md, 0.111.0).
2. The speech-to-text engine in `zoo-infer`, checked against the reference
   on known WAV files. **Done** (docs/hearing-design.md, 0.110.0).
3. Voice detection and the "hey Bella" gate.
4. Kokoro text to speech through `AUDIO_WRITE`.
5. Intel HD Audio for real PCs.
