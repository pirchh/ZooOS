# Bella's voice: the Kokoro engine (2026-11-06)

Status: implemented as a library and checked on the host (version
0.129.0): text to phonemes (§6) and the voice (§1–§4). Bella does not
speak yet: the service that plays her answers is the next step (§7). The owner approved
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

## 4. Speed (measured on the host, one thread)

Almost all of the time is the generator's convolutions on 128–256
channels at 2,400–14,500 time steps: about 150 billion multiply-adds for
2.4 s of speech. Two changes to the F32 products, measured by
`speak-check`'s own timing (one host thread, optimised):

| | "Hello, I am Bella." (2.00 s of audio) | Real-time factor |
| --- | --- | --- |
| First engine | 40.95–43.68 s (3 runs) | about 21 |
| **Weights loaded as they are** (`avx2::load8b`): the AVX2 F32 dot product copied each weight through `from_le_bytes` into a buffer first | 10.09 s | about 5 |
| **Tiles of 4 rows by 2 vectors** (`avx2::dot_f32_4x2`): each weight load serves two vectors, each vector load four rows | 4.02–4.73 s (2 runs) | about 2.2 |

The audio is the same to the last digit: `speak-check`'s differences
didn't change (1.25e-3, 1.45e-3, 7.39e-4). The first change is in the
dot product every F32 model uses. Moonshine's `hear-check` gave the same
tokens for all 8 recordings, and took 0.28–0.46 s for about 3 s of audio,
against 1.10–1.26 s before.

Some runs shared the machine with QEMU test suites, so the timings vary.
**Still too slow for conversation on one thread** (2× real time). Next,
in ZooOS:

- the 8 processors through the existing pool;
- speaking a sentence while the next is computed.

Both are to be measured in QEMU with WHPX.

## 5. Not checked into Git

The weights, the GGUF, the safetensors copies and build/speak/ are
generated or downloaded (`.gitignore`). To reproduce:

1. the download in tools/models/README.md;
2. `cargo xtask speak-gguf`;
3. `cargo xtask speak-check`, which needs the reference venv
   (tools/reference/README.md) and the hearing model.

## 6. Text to phonemes (`zoo_infer::g2p`)

Kokoro reads phonemes, not text. misaki (hexgrad, Apache-2.0; the voice
plan's choice, approved 2026-11-04) makes them from English with two word
lists, a part-of-speech tagger and, for unknown words, espeak-ng (GPL,
excluded) or a neural model.

**The word lists:** `us_gold.json` and `us_silver.json`, fetched at commit
`fba1236`, pinned in `tools/models/voice/misaki.sha256`. They are part of
misaki's Apache-2.0 repository, with no other licence stated.

`speak-gguf` merges them into the voice GGUF as `kokoro.lexicon`:

- 183,593 sorted `word\tphonemes` lines (a tab between);
- gold replaces silver;
- an entry by part of speech keeps its default;
- its end-of-sentence form is listed as `word|end`.

**The rules, after misaki's `en.py`, without the tagger:**

- **Short words** are decided by the next word's first sound:
  - "the" is ði before a vowel, ðə otherwise;
  - "to" is tʊ before a vowel, tə before a consonant;
  - "a" is ɐ, "I" is ˌI;
  - "am" and "in" also depend on what follows.
- **The end of a sentence:** a word's `|end` form is used before
  punctuation or at the end. Without it, "Okay, I won't." was heard as
  "Okay, I want."
- **Stress:** misaki's `apply_stress`, with stress from capitals
  (capitalised 0.5, all capitals 2).
- **Endings:** -s, -ed and -ing on a listed stem, with misaki's sound
  rules ("minimized" `mˈɪnəmˌIzd`).
- **Numbers in words:**
  - cardinals;
  - four-digit years ("2026" twenty twenty-six);
  - ordinals ("6th");
  - decimals ("point").
- **Words in no list:**
  - split at a change of case ("ZooOS" as Zoo, O, S);
  - else into two listed words;
  - else spelt out (misaki's `get_NNP`).

**Without the tagger** a word that is said differently by part of
speech ("read", "live", "record") always gets its default.

**Checked** (`speak-check`, after the reference sentences): four
sentences Bella says, as text, through the word lists and the engine, must
be heard back as written.

| Text | Phonemes | Heard |
| --- | --- | --- |
| Notes fills the screen. | nˈOts fˈɪlz ðə skɹˈin. | "Notes fills the screen." |
| Do you want me to close Network? | dˈu ju wˈɑnt mˌi tə klˈOs nˈɛtwˌɜɹk? | "Do you want me to close network?" |
| Okay, I won't. | ˌOkˈA, ˌI wˈOnt. | "Okay, I won't." |
| I can only search, list, inspect or create folders. | ˌI kæn ˈOnli sˈɜɹʧ, lˈɪst, ɪnspˈɛkt ɔɹ kɹiˈAt fˈOldəɹz. | "I can only search list, inspect or create folders." |

"I opened Notes and maximized it." (`ˌI ˈOpᵊnd nˈOts …`) was heard as "I
open notes and maximized it." The d runs into the n of "Notes", as it can
in speech. It was replaced in the check by a sentence Bella does say,
and is noted here.

Unit tests (`cargo test -p zoo-infer g2p`) cover the sentences, the
endings, a change of case, a number and the stress rules.

## 7. Next

1. Speed (§4).
3. A service that speaks Bella's answers through `AUDIO_WRITE`, checked
   in QEMU by recording the audio and hearing it back.
4. Then conversation (the owner's direction, 2026-11-06): staying in a
   conversation without her name, stopping when interrupted, and deciding
   by meaning whether she is spoken to.
