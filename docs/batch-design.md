# Reading a prompt in batches

Status: implemented in version 0.94.0, with docs/vectors-design.md.

## Why

The engine ran one token at a time, prompts included: for each token,
every weight row was read and dotted with that one token's vector. A
prompt of N tokens read the whole model N times. That was the first
suspect for the slow file index: 152 lines took 212 s to embed, and long
requests waited seconds for their first words.

## What changed

- **`Model::feed_many(st, ids)`** feeds `ids` in batches of `BATCH` = 16.
- **One read per row.** Each weight row is read once per batch and dotted
  with every token's vector while it is in cache (`matmul`).
- **Attention per token.** Each token sees the cache up to itself, as
  before.
- **`Model::read_prompt(st, ids, logits, go)`** wraps it for callers:
  batches, the last token through `forward` when its scores are needed,
  and `go(done)` asked before each batch so a request can still be
  cancelled or time out.
- **Used by** the system prompts and requests of `assist` and `talk`, the
  turn endings in `talk`, and `embed`.

**The arithmetic per token is unchanged** (the same dot products, in the
same order), so the results are too:
- `cargo xtask assist-check` gives the same tool, argument and margin for
  all 31 requests with batching on and off;
- `embed-check` gives the same four scores to four decimals.

## Measured

| What | One at a time | Batches of 16 |
| --- | --- | --- |
| `embed-check`'s 4 embeddings, host, release build | 3.86 s | 2.79 s (1.38×) |
| The file index's first build, guest, WHPX (152 lines) | 212 s (sharing the CPU with the chat model's warm-up) | 123 s (idle priority, file name only) |

The gain is smaller than "read the model once instead of N times"
suggests. **Reading a prompt is limited by arithmetic, not by memory:**
the AVX2 dot products run close to one core's limit. Batching saves the
memory traffic but not the arithmetic. The larger levers are more cores
(the kernel runs on one) and doing less arithmetic per token (8-bit
activations).

The guest rows also differ in two other ways: the index now runs at idle
priority, and embeds each line with its file name instead of its full
path, so they are not a clean comparison of batching alone.

## Progress and cancelling

Progress (`busy reading k/n`) and the checks for Backspace and the
deadline now come once per batch, not once per token. With WHPX a batch
of 16 takes about 0.1–0.3 s, which is not noticeable. Under TCG, where
the engine falls back to SSE, a batch takes seconds. So `bella-model`'s
busy screenshot can still show "Thinking... (Backspace cancels)" before
the first "Thinking: reading …"; its check now accepts either. Backspace
still cancels: `ElephantHandler: request cancelled`.

## Not done

- **Generating a reply** is still one token at a time; that is inherent:
  each token depends on the one before.
- **No change for single requests**: only prompts of two or more tokens
  are batched.
