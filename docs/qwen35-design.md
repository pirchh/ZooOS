# Qwen3.5-2B in the engine (2026-11-06)

Status: **implemented, measured, not Bella's model** (version 0.135.0).
The engine runs it on the host and matches the reference (§5); against
Qwen3-1.7B it is better at one judgment and worse at Bella's main job
(§6). The first of the owner's bake-off candidates (approved 11-06,
downloads included): Qwen3.5-2B, then Gemma 4 E2B.

## 1. The download

The official repository, `Qwen/Qwen3.5-2B` at commit
`15852e8c16360a2fea060d615a32b45270f8a8fc`, Apache-2.0. Qwen publishes no
GGUF of this model, so we convert the BF16 safetensors ourselves rather
than take a third party's file (tools/models/README.md).

| File | Bytes | SHA-256 |
| --- | --- | --- |
| `model.safetensors-00001-of-00001.safetensors` | 4,548,221,488 | `aa33250c…ed680b1` (the Hugging Face LFS id) |

Its config, tokenizer, chat template and licence are fetched at the same
commit. The checkpoint also holds a vision tower and a
multi-token-prediction layer; neither is used.

## 2. How it differs from Qwen3

All from the transformers implementation (`modeling_qwen3_5.py`) and the
pinned `config.json`. 24 layers; every 4th (3, 7, 11, 15, 19, 23) is full
attention, the other 18 are **Gated DeltaNet**.

- **Full attention** is Qwen3's (8 query heads, 2 key/value heads,
  head size 256, per-head RMS norms of queries and keys) with two
  changes: `q_proj` also gives an **output gate**, applied as
  `sigmoid(gate)` to the attention output before `o_proj`; and rotary
  embedding turns only the **first 64 of each head's 256 dimensions**
  (θ = 10⁷). Its multimodal rotary is plain rotary for text.
- **Gated DeltaNet** keeps, per head (16 of them), a 128×128 matrix
  instead of a key/value cache. Each token: a causal depthwise
  convolution (4 taps) and SiLU over the query, key and value
  projections; queries and keys L2-normalised, queries scaled by
  1/√128; then `S ← e^g·S`, `δ = β(v − Sᵀk)`, `S ← S + kδᵀ`,
  `o = Sᵀq`, with `β = sigmoid(b)` and `g = −exp(A_log)·softplus(a +
  dt_bias)`. The output is `w·norm(o)·silu(z)` per head, then `out_proj`.
- **Norms are zero-centred**: `x·(1 + w)` (the DeltaNet norm excepted).
- The vocabulary is 248,320 entries (Qwen3: 151,936), byte-level BPE as
  before. Its split pattern adds combining marks (`\p{M}`) to letters
  and the tokenizer applies NFC first; ours does neither, which only
  differs for text with combining marks, rare in what Bella hears.

## 3. The conversion

`cargo xtask qwen35-gguf` (xtask/src/qwen35.rs) writes
`tools/models/qwen3.5-2b-q8_0.gguf` (2,012,005,056 bytes, 326 tensors)
in about 45 s:

- matrices as `Q8_0` (ggml's reference rounding); vectors and the
  convolution as `F32`;
- the zero-centred norms stored as `1 + w`, so the engine's norm is
  unchanged; `A_log` stored as `−exp(A_log)`;
- each attention layer's `q_proj` split into `attn_q` and `attn_gate`
  (it interleaves them per head).

`cargo xtask requant --bella qwen3.5-2b` makes the 4-bit copy as for
Qwen3-1.7B: 1,325,646,528 bytes (192 of 326 tensors to `Q4_0`; the
embedding, the output layer, stays `Q8_0`).

## 4. The engine

`infer/src/model.rs`: each layer is `Mixer::Attn` or `Mixer::Delta`.
The prompt is still read 16 tokens at a time: the projections are
batched, the DeltaNet recurrence goes token by token. Only the 6
attention layers have a key/value cache.

**A recurrent state can't be cut back.** Bella starts each request from
the end of the system prompt by moving the cache position back
(`assist.rs`), and conversations from their last turn (`talk.rs`). The
DeltaNet matrices after the request can't be turned back into the ones
before it, so `State::keep(slot)` copies them (18 layers × 1 MiB, plus
the convolution's last inputs) and `State::rewind(pos)` restores the
copy taken at `pos` (or zeroes them for 0). Two slots: the end of the
system prompt and the end of the conversation. For Qwen2 and Qwen3 both
only set the position, as before. Speculative decoding
(docs/draft-design.md) takes guesses back to any position, so it refuses
a recurrent model.

## 5. Checked against a reference

`tools/reference/qwen35_ref.py`: NumPy, written from the transformers
code, reading the safetensors directly. With `--q8` it rounds each
weight matrix to `Q8_0` exactly as the converter does, so the engine and
the reference have the same weights. `cargo xtask qwen35-check` runs
both on two prompts and compares the top 5 next tokens, their logits and
8 greedy tokens (docs/verification.md).

## 6. The bake-off

Both models in 4 bits, on the host (engine `avx2`), with `--bella
qwen3-1.7b-q4` and `--bella qwen3.5-2b-q4`, on 2026-11-08:

| Check | Qwen3-1.7B | Qwen3.5-2B |
| --- | --- | --- |
| `assist-check`: requests with a command, answered right | **44 of 46** | 38 of 46 |
| `assist-check`: requests without one, acted on | 0 of 8 | 0 of 8 |
| `assist-check`: time per request, median | 1.6 s | 1.4 s |
| `bella-addressee`: said to her, taken with no talk taken | 5 of 18 | **12 of 18** |
| `bella-addressee`: at margin 0, said to her / talk taken | 13 of 18 / 8 of 16 | 17 of 18 / 9 of 16 |
| `bella-addressee`: time per sentence, median | 8.5 s | 8.7 s |
| `bella-regress` (52 requests) | the baseline | 49 the same, 3 different |
| File | 1,129,782,944 bytes | 1,325,646,528 bytes |

- **Tool choice is worse.** Qwen3.5's 8 misses include "Move notes.txt
  into Archive" as `trash` (margin 0.11, so Bella would ask first: the
  unsure guard asks below 5) and "Open the Photos folder" as `list`.
  Bella's tool descriptions and examples were refined against Qwen3
  (docs/refine-design.md), so part of this gap is that tuning, not the
  model.
- **Judging who speech is for is better, still not good enough.** Strict
  enough to take none of the 16 lines of talk, it takes 12 of the 18 said
  to her (Qwen3: 5), and each judgment still takes about 8.7 s.
  docs/addressee-design.md's bar is most of the 18 with none of the 16,
  in well under a second.
- **`bella-regress`:** the three differences are model answers: "get
  rid of that" is answered in words instead of `trash ""` (better: it
  names nothing), "put that somewhere else" likewise, and "documents"
  becomes "Documents" (better). Most of the 52 never reach the model.

**Qwen3-1.7B stays Bella's model.** Qwen3.5-2B is not a clear step up:
better at one judgment, worse at the job Bella does most, and no faster.

**The engine change has no measurable cost for Qwen3.** `assist-check`
with Qwen3 on this branch and on `main` gave the same answers (44 of 46,
0 of 8). Mean time per request: 1.567, 1.656, 1.663 s here against
1.548, 1.544, 1.544 s on `main`, in alternating runs. The host was busy
(the same build's `cargo xtask infer` decoded at 4.4 to 7.5 tokens/s
from one run to the next), and request times are printed to 0.1 s, so a
difference of this size can't be told from the noise.

## 7. Not done

- **Not run in the guest.** Nothing in the boot image chooses it;
  `verify` with `--bella qwen3.5-2b-q4` was not run. The guest runs the
  same engine code for Qwen3, which both suites cover
  (docs/verification.md).
- **Gemma 4 E2B is not ported.** Its layers (sliding and global
  attention, a per-layer embedding table, layers sharing a key/value
  cache) are more work than Qwen3.5's, and this round says a newer small
  model alone is not the step Bella needs.
- **Retuning Bella's tool prompt for Qwen3.5**, which might close part of
  the `assist-check` gap (§6).
- The vision tower and the multi-token-prediction layer.
- NFC and combining marks in the tokenizer (§2).
