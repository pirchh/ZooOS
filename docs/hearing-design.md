# Hearing: Moonshine speech to text (2026-11-04)

Status: implemented, step 2 of the voice plan (docs/voice-plan.md §6):
speech to text in `zoo-infer`, checked against a reference on known
recordings, and running in ZooOS. **There is no microphone yet:** the
recordings are files in the boot image. Recording (virtio-sound's receive
queue), voice detection and the "hey Bella" gate come next.

## 1. The model

**Moonshine Tiny** (27 M parameters, MIT; the owner's choice, DECISIONS.md
"Voice models"), downloaded as safetensors (tools/models/README.md,
"Voice"). It reads raw 16 kHz audio of any length, so there is no
spectrogram front end.

| Part | Shape |
| --- | --- |
| Encoder front end | conv 1→288, kernel 127, stride 64, tanh; group norm (one group); conv 288→576, kernel 7, stride 3, GELU; conv 576→288, kernel 3, stride 2, GELU. About 41 frames a second |
| Encoder | 6 layers, width 288, 8 heads of 36; layer norm without bias; GELU MLP of 1152 |
| Decoder | 6 layers: causal self-attention with a cache, attention over the encoder's frames, gated SiLU MLP (`fc1` gives value and gate, 1152 each) |
| Positions | rotary on the first 32 of each head's 36 dimensions, in interleaved pairs (0, 1), (2, 3), ...; θ = 10000; the cross-attention has none |
| Output | the token embedding (32,768 × 288), tied; greedy; start token 1, end token 2; at most 6.5 tokens a second of audio |
| Tokens | SentencePiece pieces: `▁` is a space, `<0xNN>` a byte |

## 2. The pieces

- **`cargo xtask hear-gguf`** (`xtask/src/hear.rs`) converts the
  safetensors, `config.json` and `tokenizer.json` into one GGUF file of F32
  tensors (`tools/models/voice/moonshine-tiny-f32.gguf`, 108,864,064
  bytes). A convolution's `[out, in, k]` weight becomes the matrix
  `[out, in * k]`. xtask reads the JSON with a small parser of its own (no
  new dependency).
- **`zoo_infer::hear`** (`infer/src/hear.rs`): `Hearing::from_gguf`,
  `encode`, `decode`, `transcribe`, plus WAV reading (`wav_samples`, 16 kHz
  mono 16-bit only) and `same_words`. Convolutions are matrix products
  over im2col columns. Every product goes through `par::fill`, so the
  processors share it as they share the chat model's.
- **The reference** (`tools/reference/moonshine_ref.py`): the same model
  in NumPy, written from the architecture, not from the Rust code.

## 3. How it is known to be right

The test recordings are made on the host by Windows' built-in speech
synthesizer (System.Speech: voices "Microsoft David Desktop" and
"Microsoft Zira Desktop"), so nothing is downloaded and the sentence said
is known. `make_recordings` writes `build/voice/<sentence>-<voice>.wav`:
four sentences in two voices.

1. **The reference hears them.** Its transcripts were exact on the first
   run. Pairing the rotary dimensions as halves instead of interleaved
   (negative control) gave "I'm at 10." and "I'm'm not alone alone
   alone ...", so exact transcripts confirm the architecture.
2. **`cargo xtask hear-check`:** on every recording the engine's
   transcript must be the sentence said (`same_words`), its tokens the
   reference's, and its encoder output and first logits within 1e-3 and
   1e-2 of the reference's (limits set before measuring). Measured: 8 of
   8, the same tokens, encoder within 2.9e-4, logits within 5.4e-4.
   Negative control: without the decoder's rotary positions, 6 of 8 FAIL
   ("Put notes on the left and files on the left and files ...").
3. **In ZooOS:** the `hear-ermine` case boots an image with the GGUF file
   and the recordings in its boot image. ErmineTest transcribes every one
   and exits 0 only if all are heard right. `hear-mislabelled` lists the
   first recording with another sentence: ErmineTest reports `MISHEARD`
   and exits 1, which the case requires.

"Monitor three" is written "monitor 3"; the expected sentence says so,
and Bella reads both (`assist::monitor_named`).

## 4. Speed

ErmineTest, 8 recordings, 24.9 s of audio in all (`hear-ermine`):

| Machine | Total | First recording (3.4 s of audio) |
| --- | --- | --- |
| TCG, 1 processor | 90.2 s | 13.9 s |
| WHPX, 1 processor | 2.41 s | 332 ms |
| WHPX, 8 processors | 0.73 s | 100 ms |

**About 0.1 s to hear a spoken command on 8 processors**, F32 throughout.
The host (`hear-check`, one thread, unoptimised xtask with the engine at
`opt-level = 3`) takes about 1 s for each.

## Not done

- Intel HD Audio for real PCs (recording in QEMU: docs/microphone-design.md).
- Voice detection (Silero VAD, approved) and the "hey Bella" gate.
- Hearing from the model disk (the model is only in the hearing image's
  boot image), and Bella acting on what was heard.
- Kokoro text to speech.
- 8-bit weights (not needed at this speed).
