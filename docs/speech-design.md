# Bella's voice: the Kokoro engine (2026-11-06)

Status: implemented as a library and checked on the host (version
0.129.0). Bella does not speak yet: text to phonemes and the service
that plays her answers are the next steps (§6). The owner approved
Kokoro-82M on 2026-11-04 (docs/voice-plan.md) and confirmed the download
on 2026-11-06; the files had been fetched on 11-04 and match their pinned
hashes (tools/models/README.md, "Voice").

## 1. The model

Kokoro-82M (hexgrad, Apache-2.0) with the `af_bella` voice: StyleTTS 2's
inference path, 82 M parameters, 24 kHz mono.

| Part | What it does |
| --- | --- |
| ALBERT (12 shared layers, 768 wide) | reads the phonemes in context |
| Prosody predictor (LSTMs, style-adaptive layer norm) | frames per phoneme (25 ms each), then pitch (F0) and energy curves |
| Text encoder (3 convolutions, an LSTM) | the phonemes' acoustic features, repeated by their durations |
| iSTFTNet decoder | style-adaptive residual blocks, then a generator driven by a 9-harmonic sine source at the predicted pitch, ending in an inverse STFT (20-point, hop 5) |

The voice is a table of 510 styles, one per phoneme count. Each style
is 256 values: half for the decoder, half for the predictor.

## 2. From PyTorch files to GGUF, without running them

The weights come as `kokoro-v1_0.pth` and `voices/af_bella.pt`: zip files
holding a pickle. **A pickle can run code, so it is never unpickled.**

- `cargo xtask speak-gguf` (xtask/src/kokoro.rs) reads the zip itself.
- It interprets the pickle with a machine that knows only the 23 opcodes
  and the 3 names a state dict uses: `collections.OrderedDict`,
  `torch._utils._rebuild_tensor_v2` and `torch.FloatStorage`. Any other
  name or opcode is refused.
- The opcodes and names were listed first with Python's `pickletools`,
  which only reads.

The GGUF it writes (`tools/models/voice/kokoro-82m-f32.gguf`, 327 MB, 448
tensors, F32) is prepared for the engine:

- weight norm folded into each weight;
- convolutions as im2col matrices `[out][in × k]`;
- transposed convolutions as `[out × k][in]`, so one product gives every
  output's contributions, added up by stride;
- each LSTM's two biases as one;
- the voice as `voice` `[510][256]`.

## 3. The engine and its check

`zoo_infer::speak::Speech` (no_std) takes phonemes in misaki's alphabet
(the model's 178 tokens) and returns 24 kHz audio and each phoneme's
frames.

- Activations are `[channel][time]`.
- Products go through the shared parallel `matmul`.
- Convolutions run in chunks of 512 time steps, so a long sentence needs
  bounded memory.
- The sine source, its phase and both STFTs are computed in f64; the
  rest is in f32.
- Kokoro's source draws random numbers. Here they come from a small
  generator (xorshift64* and Box-Muller), the same in the engine and the
  reference, so both give the same samples.

**The reference** is tools/reference/kokoro_ref.py, an independent NumPy
implementation written from the model's structure. `kokoro_pth.py` reads
the same files with a restricted unpickler and writes safetensors for it.
There is no PyTorch on this machine, so the reference is checked by
**hearing it back**: Moonshine (docs/hearing-design.md) heard its
"həlˈO, ˌI æm bˈɛlə." as "Hello, I am Bella."

**`cargo xtask speak-check`:** for three sentences given as phonemes,

- the engine and the reference must predict the same frames;
- their audio must differ by at most 0.5% (relative RMS);
- the engine's audio, resampled to 16 kHz, must be heard by the Moonshine
  engine as the sentence.

| Sentence | Frames | Difference | Heard |
| --- | --- | --- | --- |
| "Hello, I am Bella." | 80, as the reference | 1.25e-3 | "Hello, I am Bella." |
| "Notes fills the screen." | 85, as the reference | 1.45e-3 | "Notes fills the screen." |
| "The dentist is on Friday." | 97, as the reference | 7.39e-4 | "The dentist is on Friday." |

**Why 0.5%:** the reference itself moves 0.28% (relative RMS) when its
pitch curve is nudged by up to 4e-4 Hz. That is the size of the
engine's f32 rounding there (max 4.2e-4). Rounding the reference to f32
throughout moves it by only 7e-5 at most.

**A bug the check found.** The first engine's audio differed from the
reference by 4.9%, yet it was heard correctly. Tracing intermediate values
(`SPEAK_TRACE=1` writes them to build/speak/) showed the source spectrum
was the first to differ, in its phases.

- DC and Nyquist bins are real. A matrix DFT leaves a rounding-sized
  imaginary part of either sign there, so a negative value's angle came
  out as π in one and −π in the other: 6.28 apart, as the decoder's
  input.
- An FFT, as PyTorch uses, gives them no imaginary part, so the angle is
  π.
- Both now drop it: 4.9% became 0.07–0.15%.
- With the fix undone, `speak-check` fails (docs/verification.md).

## 4. Speed (measured on the host, not yet in ZooOS)

On one host thread, optimised: 42.6 s for 2.00 s of audio, 51.0 s for
2.42 s, about 21× slower than real time. Almost all of it is the
generator's convolutions on 128–256 channels at 2,400–14,500 time steps.
A rough count gives about 150 billion multiply-adds for 2.4 s of speech.

**This is too slow for conversation.** Before Bella speaks for real:

- a blocked matrix kernel for these products (the current `matmul` takes
  one dot product per output, memory-bound);
- all 8 processors through the existing pool;
- speaking a sentence while the next is computed.

Each is to be measured in QEMU with WHPX, before and after.

## 5. Not checked into Git

The weights, the GGUF, the safetensors copies and build/speak/ are
generated or downloaded (`.gitignore`). To reproduce:

1. the download in tools/models/README.md;
2. `cargo xtask speak-gguf`;
3. `cargo xtask speak-check`, which needs the reference venv
   (tools/reference/README.md) and the hearing model.

## 6. Next

1. Text to phonemes: misaki's word lists (Apache-2.0, to fetch with the
   owner's earlier approval in docs/voice-plan.md), with a fallback for
   words not in them.
2. Speed (§4).
3. A service that speaks Bella's answers through `AUDIO_WRITE`, checked
   in QEMU by recording the audio and hearing it back.
4. Then conversation (the owner's direction, 2026-11-06): staying in a
   conversation without her name, stopping when interrupted, and deciding
   by meaning whether she is spoken to.
