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

## Not done

- A 4-bit version (a host-side re-quantiser from `Q8_0` to `Q4_0`),
  which should bring its speed near Qwen2.5-1.5B's.
- The command choice (`assist-check`) has not been measured with it.
