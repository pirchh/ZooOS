# M5b design: the local inference engine

Status: implemented and verified 2026-10-30 (results in docs/verification.md).
Second of three M5 steps. M5a provided floating point, heap memory and file
mapping; M5c builds the assistant on this engine.

Goal: run the owner-approved model, **Qwen2.5-0.5B-Instruct** (GGUF `q4_0`,
tools/models/README.md), inside ZooOS, with evidence that it computes the
model correctly, not just that it prints plausible text.

## 1. Engine shape

A new `no_std` + `alloc` crate, `infer/` (`zoo-infer`), with no unsafe code.
The same source runs in ZooOS user programs and on the host, so host
checks and benchmarks exercise exactly the code the guest runs. Dependency:
`libm` 0.2.16 for `exp`, `sqrt`, `pow`, `sin` and `cos` (not available in
`core`).

| Module | Role |
| --- | --- |
| `gguf` | GGUF v3 reader over borrowed bytes (an `fs_map` mapping in the guest). Every length, offset and tensor range is checked; malformed input is an error, never a panic. |
| `quant` | F32, Q4_0 and Q8_0: exact f16 decoding, dequantisation and dot products directly on quantised rows. Other types are rejected (the chosen file uses only these three). |
| `model` | Qwen2: token embedding, 24 layers (RMSNorm, Q/K/V with biases, rotary embedding in the NEOX layout, grouped-query attention with 14 query and 2 key/value heads, SwiGLU feed-forward), final norm and the separate Q8_0 output matrix. One token at a time with a key/value cache. |
| `tokenizer` | GPT-2-style byte-level BPE from the vocabulary and merges in the file; Qwen2's pre-tokenizer pattern re-implemented by hand with Rust's Unicode letter/number/whitespace tests; control tokens such as `<\|im_start\|>` matched literally. |
| `chat` | Qwen2.5 ChatML prompt (system + user, no tools yet) and greedy generation with an early-stop callback (used for cancellation in M5c). |

Weights are never copied: matrix-vector products read the quantised rows in
the mapped file. Only norms and biases (small F32 tensors) are expanded.

## 2. Evidence of correctness

1. **Unit tests** (`cargo test -p zoo-infer`): f16 decoding including
   subnormals, Q4_0/Q8_0 blocks against hand-computed values, the GGUF reader
   on a synthetic file and on every truncation of it, the pre-tokenizer on
   tricky strings, and the byte mapping.
2. **Independent reference** (`tools/reference/qwen_ref.py`, NumPy in a
   project-local venv): its own GGUF reader, dequantisation, tokenizer
   (Python `re` for the split pattern) and forward pass, written separately
   from the Rust code. `cargo xtask infer-check` runs both on five prompts
   chosen to exercise digits, newlines, punctuation runs, non-ASCII text,
   contractions and a file request. **Prompt token ids and generated token ids
   must be identical, and the top-5 next-token logits must name the same
   tokens within 0.02.**
3. **In the guest:** ElephantHandler (§3) must generate exactly the token ids
   the host engine produced for the same prompt.

## 3. In-guest check: ElephantHandler

- A separate **model image** (`build/normal-model/`): the normal kernel with an
  initrd that also contains the model (`models/…gguf`) and `test/elephant.txt`
  (the prompt and the host engine's output ids, written at verify time). The
  other cases keep the small image.
- The `inference` verify case types `run elephant-handler` into the shell.
  ElephantHandler maps the model with `fs_map`, builds the tokenizer and
  model, generates greedily, logs the answer and timings, and exits 0 only
  if its tokens equal the expected ones. The case passes on
  `PROC exit pid=8 name=elephant-handler code=0`.
- If the model is not downloaded, verify stops with instructions;
  `--skip-model` reports the case as not run instead of passing it.

## 4. Speed and acceleration

Software emulation (TCG, the reproducible default) is slow for a 0.5B model,
so the `inference` case gets a long timeout. Verify gained `--accel whpx`
(optional, never required) for quicker local runs, and `--case NAME` to run
one case while iterating. Under WHPX QEMU's default CPU model is used, because
`-cpu max` made the firmware fault. Timings are recorded in
docs/verification.md as observations, not performance claims.

## 5. Deferred

Sampling other than greedy, batching prompt tokens, multi-threading, SIMD
kernels beyond what the compiler auto-vectorises, other quantisation types,
long contexts, tool-calling prompts (M5c).
