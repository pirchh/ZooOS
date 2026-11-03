# Bella's file index by meaning

Status: implemented in version 0.94.0, with docs/lookup-design.md and
docs/batch-design.md. This is
the knowledge track's "smart vector store" (docs/bella-roadmap.md). The
owner approved the embedding model on 2026-11-03.

## The model

| Field | Value |
| --- | --- |
| Model | Qwen3-Embedding-0.6B, official GGUF, `Q8_0` (639 MB) |
| Licence | Apache-2.0 (the card's tag and the file's `general.license`). The card's prose once calls it "the latest proprietary model of the Qwen family"; the published weights carry the Apache-2.0 tag. |
| Shape | Qwen3: 28 layers, width 1024, 16 query and 8 key/value heads of 128, no output layer |
| Pooling | the last token, after an appended `<|endoftext|>` (`add_eos_token`, `pooling_type` 3); L2-normalised |
| Pinned | tools/models/README.md |

It runs on the engine that runs Bella's chat models
(`infer/src/embed.rs`): `Model::final_hidden` gives the hidden state after
the output norm, and `Embedder` cuts it to `DIM` = 256 dimensions (the
model is trained for shorter vectors, "MRL") and scales it to length 1.

- **Queries** get the card's instruction format: "Instruct: Given a
  question, retrieve lines from the user's own notes and files that
  answer it\nQuery:…".
- **Documents** get no instruction. Each line is embedded with its
  file's name in front ("notes.txt: Call the vet about Zara the zebra.").
  The full path cost tokens and said little.

**Checked against the model card** (`cargo xtask embed-check`, on the
host, full 1024 dimensions):

| Query / document | Ours | Card |
| --- | --- | --- |
| capital of China / Beijing | 0.7639 | 0.7646 |
| capital of China / gravity | 0.1424 | 0.1414 |
| gravity / Beijing | 0.1361 | 0.1355 |
| gravity / gravity | 0.6005 | 0.6000 |

At 256 dimensions, "when do I need to take the zebra to the animal
doctor?" scores 0.7454 with the vet line, against 0.3300 and 0.2918 for
the shopping and pancake lines.

## The model disk, version 2

The model disk held one model (`ZOOMODL1`). Version 2 (`ZOOMODL2`) holds up
to `MAX_MODELS` = 4: a count, then each model's first block and length.
- **Model 0** is Bella's chat model, as before.
- **Model 1** is the embedding model. `xtask`'s `model_disk` adds it
  whenever `tools/models/` has it, so `run` gets the index too.

The system calls gain the model's number:
- `MODEL_SIZE`'s `a1` is the model's number;
- `MODEL_READ`'s block number carries it in its top byte
  (`MODEL_WHICH_SHIFT` = 56).

Programs that pass 0 (ElephantHandler) work as before. The kernel still
reads version 1 disks.

## MagpieHandler

A new service (`user/src/bin/magpie-handler.rs`). Bella starts it when
the model disk has an embedding model, with a channel and a copy of her
home handle.

**It runs at idle priority**, a new third class (`SET_BACKGROUND` 2,
`zoo_rt::set_idle`). An idle program runs only when no normal or
background program is ready, and gets no turn for waiting. So the first
index never delays the desktop or the chat model; it fills the time
nothing else wants.

1. **Start.** It reads the embedding model from the model disk.
2. **Index.** It walks the home folder's text files, skipping hidden
   ones, so Bella's own files never go in; at most 200 files, 16 KiB each,
   2,000 lines of up to 300 characters. It embeds each line and keeps the
   vectors in `.bella-index`, with each file's FNV-1a hash.
3. **Incremental.** Before each question the index is refreshed. Lines of
   unchanged files keep their vectors; changed and new files are
   embedded; files that are gone drop out. After a restart, the index is
   read back from `.bella-index`, not rebuilt.
4. **It stops when Bella has gone.** Before each line it embeds, it
   checks that someone still holds the other end of its channel
   (`channel_peer`), so a first index never outlives the desktop.
5. **`ready <lines>`** once the first index is built. Until then, Bella
   uses the word lookup.
6. **`query <question>`** answers with up to 3 lines, scoring at least
   0.45 (cosine), as `| path: line`, best first, then `ok` or `none`.

**Bella** (docs/lookup-design.md):
- asks MagpieHandler first, and the broker's word lookup when the index
  is not ready, has ended, or does not answer within 30 s;
- treats an index answer of `none` as "nothing in your files", without a
  word lookup.

## Index format (`.bella-index`)

```
"ZOOVIX01"   8 bytes
dim          u32 (256)
count        u32
count times: file hash u64, path (u16 length + UTF-8), line (u16 length + UTF-8), dim × f32
```

All little-endian. A file in another format, or with another `dim`, is
ignored and rebuilt.

## Measured (WHPX, the test home folder: 6 files, 152 lines)

| What | Time |
| --- | --- |
| Reading the embedding model from the model disk | 0.46 s (609 MB) |
| The first index | 123 s, at idle priority (212 s before batching, idle priority and file names only, while sharing the CPU with the chat model's warm-up) |
| The index after a restart, read from `.bella-index` | 0 ms to bring up to date, 0 embedded |
| A question: refresh, embed, compare 152 lines | 1.1 s |
| "what do my notes say about the animal doctor?" | best line 0.608, the vet line; answer "The vet about Zara the zebra." in 4.5 s |

The chat model's warm-up took 5.0 s with the index starting beside it,
against 3.5 s without. Reading the embedding model from the disk holds
the processor in the kernel (B10, docs/stage2-design.md §3).

## Checks

- **`cargo xtask embed-check`:** the card's four scores within 0.03 (all
  were within 0.001), and Bella's case. `PASS`.
- **`bella-vectors`** (WHPX, with the embedding model on the model disk):
  1. The index is built and `ready`.
  2. "what do my notes say about the animal doctor?" finds the vet line,
     which shares no word with the question beyond "notes", and the answer
     names Zara or the zebra.
  3. After a desktop restart: `index: … (0 embedded, …)`, so it was read
     back, not rebuilt.
  - **Negative control:** with the similarity bar at 0.99, `FAIL`: "0 of
    152 lines near enough", and Bella said she found nothing.

## Not done

- One vector per line. Paragraphs, and lines that only make sense
  together, are not joined.
- The index is refreshed when asked, not when files change. Saving a
  file and asking at once embeds it during the question.
- Only UTF-8 text files.
