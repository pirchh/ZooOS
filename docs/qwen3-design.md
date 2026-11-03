# Qwen3 in the engine

Status: implemented in version 0.93.0, as an evaluation model
(`--bella qwen3-1.7b`). The default stays Qwen2.5-0.5B.

The owner asked whether newer models suit Bella better (2026-11-03) and
approved downloading Qwen3-1.7B after its model card was read.

## The model

| Field | Value |
| --- | --- |
| Model | Qwen3-1.7B, official GGUF, `Q8_0` (the only file Qwen publishes) |
| Licence | Apache-2.0, no use restrictions on the model card |
| Shape | 28 layers, width 2048, 16 query and 8 key/value heads of 128, feed-forward 6144, tied embeddings |
| File | 1,834,426,016 bytes; pinned in tools/models/README.md |

## What the engine needed

- **The `qwen3` architecture.** The same keys under a `qwen3.` prefix.
  The head size comes from `attention.key_length` when the file states
  it.
- **No q/k/v biases.** They are optional now: a missing bias tensor is
  an empty vector, skipped.
- **QK-norm.** Optional per-head RMS norms of queries and keys
  (`attn_q_norm`, `attn_k_norm`), applied before RoPE.
- **Query width separate from model width.** `n_head * head_dim` need
  not equal the width (it does for 1.7B; it does not for Qwen3-0.6B). The
  attention output has its own buffer (`State::xq`).
- **Thinking off.** Qwen3 reasons inside `<think>…</think>` first unless
  told not to. `chat::reply_open` opens each new reply with an empty
  `<think>\n\n</think>\n\n`, as Qwen's template does with
  `enable_thinking=False`. Earlier turns kept in the conversation are
  written without it, as the template does.
- **`<think>` as one token.** The tokenizer now matches user-defined
  tokens (GGUF type 4) literally in input, like control tokens. On the
  host: `<think>` = 151667, `</think>` = 151668.

## Measured

On the host (`cargo xtask infer --bella qwen3-1.7b`, debug build): "The
capital of France is Paris."

In the guest with WHPX: see the table in docs/stream-design.md.

- **Reading the model.** 1,749 MB from the model disk in 1.2–1.3 s.
- **Speed.** About 1.2–1.6× the time of Qwen2.5-1.5B per reply. Part of
  that is the size: the official file is 8-bit, while our Qwen2.5 files
  are 4-bit, so every weight read is twice as many bytes.
- **Quality.** Its replies are the best of the three. For example, its
  summary of `Documents/notes.txt`: "The file lists items to buy: flour,
  eggs, and milk. It also mentions calling the vet about Zara the zebra."
  It also made up a detail once: "a simple file with 360 bytes of
  content". The size was in the conversation from the `open` that came
  before, but it was not asked for.

## The 4-bit copy (`--bella qwen3-1.7b-q4`)

`cargo xtask requant --bella qwen3-1.7b` (`xtask/src/requant.rs`) writes
`tools/models/qwen3-1.7b-q4_0.gguf`:
- **The metadata** is copied byte for byte.
- **The 196 two-dimensional `Q8_0` weight matrices** become `Q4_0`, using
  ggml's reference rounding (`quantize_row_q4_0_ref`).
- **The token embedding stays `Q8_0`.** With tied embeddings it is also
  the output layer.
- **Size:** 1,834,426,016 → 1,129,782,944 bytes.
- **SHA-256:** `23bd087a7d67959d942223d49d2978fe43d201a6c4d8e377c6cac726eae38d40`.
  A second run gives the same hash.

On the host it still says "The capital of France is Paris."

In the guest with WHPX, the whole reply (first words), against the 8-bit file and Qwen2.5-1.5B:

| Request | Qwen2.5-1.5B | Qwen3 `Q8_0` | Qwen3 `Q4_0` |
| --- | --- | --- | --- |
| Small talk | 3.5 s (1.0) | 5.2 s (1.8) | 2.6 s (1.5) |
| A joke | 6.4 s (2.1) | — | 4.4 s (3.1) |
| From a fact | 4.3 s (2.5) | 5.5 s (3.7) | 4.6 s (3.0) |
| What am I working on? | 8.7 s (5.3) | 13.7 s (8.5) | 12.7 s (6.6) |
| Summarise `Documents/notes.txt` | 11.9 s (7.7) | 14.8 s (10.3) | 12.4 s (8.8) |

The model reads from the model disk in 0.8–0.9 s (1,077 MB). Its summary
keeps the details: "The file lists shopping items: flour, eggs, milk. It
also mentions calling the vet about Zara the zebra."

Long requests stay slower than Qwen2.5-1.5B, because Qwen3 is wider (2048
against 1536). Reading a request costs computation, not only bytes.

**Command choice** (`cargo xtask assist-check --bella qwen3-1.7b-q4`, on
the host): `PASS`.
- 17 of 23 requests with a command were answered right, the same as 0.5B.
- 0 of 8 requests without one were acted on; 0.5B acted on 2 of them.
- Misses: "Find my pancake recipe", "List the files in Photos", "Look
  for anything about taxes" and "Open the Photos folder" went to `none`;
  "Open my notes file" opened `notes`; "How much space am I using?" went
  to `inspect`. The prompt was tuned on 0.5B.

## Not done

- The default model is the owner's choice. Qwen3 `Q4_0` is the strongest
  candidate measured: about Qwen2.5-1.5B's speed, better summaries, and
  no wrong actions in `assist-check`.
- No prompt tuning for Qwen3.
