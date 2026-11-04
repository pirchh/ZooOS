# Speculative decoding with a draft model, measured (2026-11-04)

Status: **measured, not used by Bella.** On the CPU it is slower, not
faster. The engine pieces and the measurement stay so it can be rerun at
the GPU stage, where checking several tokens at once is nearly free.
Owner approval for the download: the "do everything" decision of
2026-11-03 (roadmap: "the draft model, Qwen3-0.6B, download approved").

## 1. The idea

A small model of the same family guesses `k` tokens; the large model
checks them in one batch, keeps the ones it agrees with, then adds its own
token where it disagrees. The result is exactly greedy decoding with the
large model alone. It pays when (a) many guesses are kept and (b) checking
`k + 1` tokens costs about one step.

## 2. What was built

- `Model::feed_checking` (`infer/src/model.rs`): the batched pass
  (`feed_many`'s) that also returns the most likely next token at every
  position, from one output matrix multiply over the batch.
- `zoo_infer::draft::speculate`: the loop, rolling both caches back to
  just after the kept tokens.
- `cargo xtask draft-check`: a reply ("Tell me three short facts about
  zebras.", Bella's reply prompt) by the default model alone, then with
  the draft at k = 2, 4, 6; it checks the tokens are the same and that
  the two models tokenize alike (same ids for sample text, same
  vocabulary size).
- The draft: Qwen3-0.6B `Q8_0` (tools/models/README.md), and its 4-bit
  copy from `cargo xtask requant --bella qwen3-0.6b`.

## 3. Measured (host, AVX2)

Large model Qwen3-1.7B `Q4_0`; the reply is 20 tokens.

| Draft | k | Time | vs alone | Guesses kept |
| --- | --- | --- | --- | --- |
| — (alone) | — | 3.16 s | 1.00x | — |
| 0.6B `Q4_0` | 2 | 7.56 s | 0.42x | 7 of 26 |
| 0.6B `Q4_0` | 4 | 12.36 s | 0.26x | 7 of 52 |
| 0.6B `Q4_0` | 6 | 24.34 s | 0.13x | 8 of 72 |
| 0.6B `Q8_0` | 2 | 10.79 s (alone 4.64 s that run) | 0.43x | 8 of 24 |

The tokens were the same every time (`draft-check: PASS`).

## 4. Why it does not pay here

- **Few guesses are kept: 27 to 33%.** Speculative decoding usually
  needs well over half. The 0.6B model and the 1.7B model disagree on
  most next tokens of a Bella reply; the 8-bit draft kept about as few.
- **A check is not one step's cost on a CPU.** The engine's batch shares
  each weight's read across tokens, but the dot products are computed per
  token; on the CPU that arithmetic, not memory, is the cost, so checking
  3 tokens costs close to 3 steps, plus the output layer for every
  position (151,936 scores each).

Both change on a GPU: a batch of a few tokens costs about one step there.
Revisit with `cargo xtask draft-check` at the GPU stage.

## 5. Checked unchanged

`feed_many` now shares its code with `feed_checking` (the batch takes an
optional list for the checks). Afterwards: `assist-check` (Qwen3) 36 of 38
and 0 of 8, as before; `bella-convo` and `bella-summary-answer`
(`--accel whpx --cpus 8`) `OK`.
