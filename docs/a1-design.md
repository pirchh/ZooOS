# Assistant step 1 (A1): a faster model engine

Status: implemented 2026-11-02 (version 0.65.0). The owner asked for the
Assistant to be "much faster". The model and its answers are unchanged;
only how the arithmetic is done changed.

## 1. Changes

- **Vector dot products** (`infer/src/quant.rs`).
  - Q4_0 and Q8_0 rows are now multiplied 16 values at a time with
    `core::simd` (safe code; the pinned nightly provides it).
  - Q4_0 nibbles are unpacked in vectors.
  - Partial sums stay in vector lanes and are added up once per row.
- **No scores for prompt tokens nobody reads** (`Model::feed`).
  - The output projection, 151,936 × 896, is the largest matrix.
  - Prompt tokens before the last, the Assistant's system prompt, and
    tool-word tokens before the last skip it.
  - Scores are computed only where they are read, so this is exact.
- **Rotary frequencies precomputed** once per model, instead of a `powf`
  for every pair, head, layer and token.

## 2. Measurements

These are this machine's observations, single runs, before (commit
`4c84eda`) and after.

**Host** (`cargo xtask infer` and `cargo xtask assist-check`):

| | Before | After |
| --- | --- | --- |
| Prefill, 26 tokens | 8.1 tok/s (3.21 s) | 13.2 tok/s (1.97 s) |
| Generation | 5.9 tok/s | 9.8 tok/s |
| Assistant system prompt, 55 tokens | 9.6 s | 4.2 s |
| Assistant requests (8 cases) | 2.3–3.9 s | 1.1–1.9 s |

**Guest, `--accel whpx`**, from the serial logs (100 ticks = 1 s):

| | Before | After |
| --- | --- | --- |
| `inference`: prefill 26 tokens | 437 ticks | 164 ticks |
| `inference`: generate 7 tokens | 116 ticks | 58 ticks |
| `assistant-live`: system prompt, 55 tokens | 922 ticks | 340 ticks |
| `assistant-live`: "Find my pancake recipe" | 1155 ticks | 437 ticks |
| `assistant-live`: "Make a folder called Projects/2026" | 369 ticks | 151 ticks |

## 3. Accuracy

The answers are unchanged:
- **`infer-check`** (against the NumPy reference): every prompt still
  gives the same tokens, with top-5 logits within 0.00006 (the limit is
  0.02).
- **`assist-check`:** all 8 cases choose the same tool and argument, with
  the same margins as before.
- **In the guest,** the tokens still match the host engine exactly.

## 4. Considered and not taken

- **8-bit activations with integer products** (as llama.cpp does for
  Q4_0). It was tried and is about 25 % faster again: 16.7 tok/s prefill,
  12.3 tok/s generation. But it moves logits by up to 0.21 against the
  float reference, so `infer-check` fails its 0.02 limit, although the
  generated tokens stayed the same. Taking it means changing that agreed
  check, so it is left for the owner to decide.
- **AVX2, and more than one CPU,** would give more. They need CPU-feature
  detection in the guest and SMP in the kernel, so they are later steps.
