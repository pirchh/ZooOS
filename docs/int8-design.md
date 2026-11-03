# 8-bit activations

Status: implemented in version 0.95.0. Part of the owner's goal of answers
in under a second (2026-11-03). This was the "int8 activations" decision
left open on 2026-11-02; the owner chose it on 2026-11-03.

## What changed

Each weight row was dotted with the activation vector in floating point:
every 4-bit or 8-bit weight was widened to a float and multiplied. Now,
for `Q4_0` and `Q8_0` rows, the activations are first quantized like a
`Q8_0` row: per 32 values, `d = max |x| / 127` and `q = round(x / d)`.
The dot product is then done in integers, as llama.cpp does by default.

- **`quant::Q8x`:** the quantized vector. `matvec` quantizes its input
  once for all rows, and `matmul` quantizes each token's vector once.
- **`avx2::dot_q8x`:** 32 products per block in one `maddubs`. That
  instruction multiplies unsigned bytes by signed ones, so the weights'
  signs are moved onto the activations first, as `_mm256_sign_epi8`
  allows. The products are summed to 32-bit integers and scaled once per
  block.
- **`quant::dot_q8x_portable`:** the same in plain integers, for the SSE
  path (TCG).
- **`Q6_K` rows** (Qwen2.5-1.5B's output layer) keep the floating-point
  path.
- **`--f32-activations`** on the host's `infer`, `infer-check`,
  `assist-check` and `embed-check` turns it off, to compare.

## The bar, set before measuring

1. **`infer-check`:** the same output tokens as the NumPy reference, with
   top-5 logits within 0.5 (`INT8_LOGIT_TOLERANCE`; the floating-point
   path keeps 0.02).
2. **`assist-check` (0.5B):** at least 17 of 23 right and at most 2 of 8
   acted on.
3. **`embed-check`:** the model card's scores within 0.03.
4. **The model cases still pass.**
5. **Measurably faster,** or it is not kept.

## Results

1. **`infer-check`: failed on one point.**
   - All five prompts give exactly the same prompt and output tokens.
   - Three are within the bar: max diffs 0.171, 0.208 and 0.084.
   - On two, the 5th-ranked token differs (near-ties):
     - "Translate 'café'…": 5th is token 17360 at 16.63, against 76661
       at 16.78 for the reference;
     - "Create a folder…": 5th is 8420 at 18.37, against 40 at 18.37.

   The top 4 tokens are the same on every prompt.
   **The check reports `FAIL`, and is recorded as such.** It is kept as
   it is: it compares the top 5 in order, so it does not pass this.
2. **`assist-check`:** `PASS`, 17 of 23 and 2 of 8. All its tools and
   arguments are as before.
3. **`embed-check`:** `PASS`, 0.7629, 0.1412, 0.1374 and 0.5987 against
   the card's 0.7646, 0.1414, 0.1355 and 0.6000.
4. **Model cases:** `bella-talk`, `bella-facts-answer`,
   `bella-context-answer` and `bella-summary-answer` are all `OK` with
   0.5B and with Qwen3-1.7B `Q4_0`.
5. **About 1.3× faster** (WHPX, first words / whole reply):

| Request | 0.5B before | 0.5B now | Qwen3 `Q4_0` before | Qwen3 `Q4_0` now |
| --- | --- | --- | --- | --- |
| Small talk | 0.34 / 0.78 s | 0.24 / 0.62 s | 1.5 / 2.6 s | 1.1 / 2.0 s |
| From a fact | 0.74 / 0.94 s | 0.54 / 0.70 s | 3.0 / 4.6 s | 2.2 / 3.5 s |
| What am I working on? | 1.6 / 2.2 s | 1.2 / 1.5 s | 6.6 / 12.7 s | 4.6 / 7.1 s |
| Summarise `Documents/notes.txt` | 2.2 / 2.2 s | 1.6 / 1.7 s | 8.8 / 12.4 s | 6.6 / 11.3 s |
| Warm-up, system prompt (157 tokens) | 3.48 s | 2.78 s | — | 12.9 s |

**Kept**, despite bar 1. Of the bar's five points it met four. The miss is
a swap between near-tied candidates in 5th place, which greedy decoding
never picks; every output token and every command choice is unchanged.

## Not done

- **AVX-VNNI.** The owner's CPU (i9-12900K) has `vpdpbusd`, which does
  `maddubs` + `madd` in one instruction. WHPX's guest CPU model would
  need to expose it.
- **`Q6_K` with 8-bit activations.**
