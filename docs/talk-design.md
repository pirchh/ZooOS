# Bella talks; a choice of model

Status: implemented 2026-11-02 (version 0.83.0). The owner's direction
after trying Bella ("this is just a plain language assistant"):
- fix sentences that start with a command word;
- let Bella answer in words;
- add Qwen2.5-1.5B now.

Pulled forward from Stage 5 of docs/bella-roadmap.md.

## 1. Routing a line

The rules are in `zoo_abi::bella`, as plain text rules with host unit
tests.

- **A typed command:**
  - A command word followed by a path or search words, and none of
    `SENTENCE_WORDS` (a, the, my, me, for, of, to, please, what, where,
    is, are, and, tell, …).
  - So `search pancake recipe` is a command, and "search for my music
    and tell me the path" is a request in words.
  - Before, any line starting with a command word was run literally, and
    that request searched for every word and found nothing.
- **Small talk:**
  - A greeting or thanks (hi, hello, hey, thanks, …), or a question about
    Bella herself ("your name", "who are you", "what can you do", …).
  - It is answered in words at once, without asking for a command. The
    0.5B model sent most of these to `search`.
- **Everything else** goes to Bella's memory, then the model's command
  choice.

**Search drops filler words** ("my", "the", …), so `search my music`
finds Music. The model often keeps them.

## 2. Replies in words

- **New in `zoo_infer::talk`:** `Talk`, with its own system prompt (61
  tokens): Bella, built into ZooOS, offline, can search, list and inspect
  files and create folders, answers in one or two short sentences.
- **The reply:** at most 48 tokens, greedy, one paragraph (it stops at a
  line break or the turn's end), control characters made spaces.
- **ElephantHandler** serves `talk <request>` with progress lines (`busy
  setup/reading/writing`), then `say <text>`.
- **It warms the reply prompt** after the command prompt. A request
  arriving then interrupts that warm-up, which resumes when no request is
  waiting, so the first request is never held back by it.

**Bella asks for a reply in words:**
- for small talk;
- when the command choice is `none`;
- when the model chose `search` or `create` with nothing to act on.

**Safety: the reply is only shown, never run.** As with the command
choice, the model sees only the line typed, never a file's name or
contents. The request stays open, so a command typed next still answers
it.

## 3. A choice of model: `--bella 0.5b | 1.5b`

`infer::MODELS` lists the models, each pinned by its `.sha256` file in
tools/models/:
- `0.5b`: Qwen2.5-0.5B-Instruct Q4_0, the default and the test suite's
  model;
- `1.5b`: Qwen2.5-1.5B-Instruct Q4_0, Apache-2.0, pinned to commit
  `91cad511…`, SHA-256 `dcd819ff…`, 1,066,227,232 bytes.

`run`, `infer`, `infer-check`, `assist-check` and `verify` take `--bella`.

**What 1.5B needed:**
- **The `Q6_K` format** for its output layer. GGML's 6-bit "k-quant":
  256 values in 210 bytes, 16 signed sub-block scales and one f16 scale.
  `quant::q6_k_block` follows GGML's `dequantize_row_q6_K`, and has a
  unit test with a hand-computed block.
  - **Checked end to end:** 1.5B answers "The capital of France is
    Paris." on the host, and passes `assist-check`.
- **A larger heap:** `HEAP_LIMIT` goes from 1 GiB to 4 GiB, since the
  weights alone are 1.07 GB in ElephantHandler's heap.
- **A larger machine:** `run` and `verify` give the guest 4 GiB with a
  non-default model.
- **The model disk only.** A 1 GB model does not fit the boot image's FAT
  partition, so `verify --bella 1.5b` skips the cases that need the model
  in the boot image.

**Measured:**

| | 0.5B | 1.5B |
| --- | --- | --- |
| `assist-check`, right with a command (of 14) | 10 | **11** (one miss is `invoices` for `Invoices`) |
| `assist-check`, acted on without one (of 8) | 2 | 2 |
| Typical tool-word margin | 0.3–5 | 5–12 |
| Host: request / system prompt | 0.9 s / 4.6 s | 3.3 s / 16.6 s |
| Guest (WHPX): read from the model disk | 341 ms (408 MB) | 743 ms (1,016 MB) |
| Guest: warm-up / request after it | 4.9 s / 1.0 s | 17.6 s / 3.7 s |
| Guest: ElephantHandler heap | 437 MB | 1,066 MB |

Sample replies from the host engine:
- **0.5B, "Tell me a joke":** "Why did the tomato turn red? Because it
  saw the salad dressing!"
- **1.5B, "Tell me a joke":** "Sure, here's a joke for you: Why couldn't
  the bicycle stand alone? Because it was two-tired!"
- **1.5B, "Shut down the computer":** "I'm sorry, but I can't perform
  that action."

1.5B is about 3.5 times slower. Stage 3's wider vector code is the plan
to win that back.

## 4. Checks

- **Host unit tests:** `zoo_abi::bella` covers typed commands, sentences,
  and small talk and its near misses ("hiking photos", "this is high").
  `zoo_infer` has the hand-computed `Q6_K` block.
- **`bella-talk`** (WHPX, 0.5B):
  - "whats your name" →
    ``said `My name is Bella, the assistant built into ZooOS.` ``;
  - "Tell me a joke" → `no command fits`, then a joke;
  - "search for my music" → the model's `search my music`, never `ms by
    a typed command`;
  - the screenshot shows the conversation.
- **`everyday-model --bella 1.5b`** (WHPX): `OK`, `read the model from the
  model disk: 1016 MB in 743 ms`, `chose search `pancake recipe``.
- **Unchanged and still passing:** `bella-live` (first answer 1,102 ms),
  `bella`, `bella-memory`, `memory-guard` and `bella-any-case`.

## 5. Not done

- **Conversation memory:** each line is still answered on its own. That
  is next.
- **Broad access** (the owner chose "broad, confirm deletes") and
  **knowledge** (a vector store; web lookups once there is HTTPS) are
  planned in docs/bella-roadmap.md.
