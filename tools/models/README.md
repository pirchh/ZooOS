# Model weights

**Bella's default model is Qwen3-1.7B in 4 bits** (`qwen3-1.7b-q4_0.gguf`,
since version 0.98.0, the owner's choice). It is made from the downloaded
`Q8_0` file with `cargo xtask requant --bella qwen3-1.7b` (below).
Qwen2.5-0.5B is `--bella 0.5b`; `infer-check` uses it, because the NumPy
reference implements Qwen2.

Model files are downloaded, never committed (`*.gguf` is ignored). The owner
approved this download on 2026-10-30 (DECISIONS.md, "Owner decisions").

| Field | Value |
| --- | --- |
| Model | Qwen2.5-0.5B-Instruct, GGUF, `q4_0` quantization |
| File | `qwen2.5-0.5b-instruct-q4_0.gguf`, 428,730,208 bytes |
| Source | https://huggingface.co/Qwen/Qwen2.5-0.5B-Instruct-GGUF |
| Pinned commit | `9217f5db79a29953eb74d5343926648285ec7e67` |
| SHA-256 | `7671c0c304e6ce5a7fc577bcb12aba01e2c155cc2efd29b2213c95b18edaf6ed` (also in the `.sha256` file here; matches the Hugging Face LFS object id) |
| Licence | Apache-2.0 (model card) |

A second model, chosen with `--bella 1.5b` (docs/talk-design.md). The
owner approved this download on 2026-11-02:

| Field | Value |
| --- | --- |
| Model | Qwen2.5-1.5B-Instruct, GGUF, `q4_0` (its output layer is `Q6_K`) |
| File | `qwen2.5-1.5b-instruct-q4_0.gguf`, 1,066,227,232 bytes |
| Source | https://huggingface.co/Qwen/Qwen2.5-1.5B-Instruct-GGUF |
| Pinned commit | `91cad51170dc346986eccefdc2dd33a9da36ead9` |
| SHA-256 | `dcd819ff094852c38faba6873d8ff0c9d51eadb2844539e52042ae5d647bbfdb` (matches the Hugging Face LFS object id) |
| Licence | Apache-2.0 (model card) |

    curl -fL -o tools/models/qwen2.5-1.5b-instruct-q4_0.gguf \
      https://huggingface.co/Qwen/Qwen2.5-1.5B-Instruct-GGUF/resolve/91cad51170dc346986eccefdc2dd33a9da36ead9/qwen2.5-1.5b-instruct-q4_0.gguf

A third model, under evaluation, chosen with `--bella qwen3-1.7b`
(docs/qwen3-design.md). The owner approved this download on 2026-11-03:

| Field | Value |
| --- | --- |
| Model | Qwen3-1.7B, GGUF, `Q8_0` |
| File | `qwen3-1.7b-q8_0.gguf`, 1,834,426,016 bytes |
| Source | https://huggingface.co/Qwen/Qwen3-1.7B-GGUF |
| Pinned commit | `90862c4b9d2787eaed51d12237eafdfe7c5f6077` |
| SHA-256 | `061b54daade076b5d3362dac252678d17da8c68f07560be70818cace6590cb1a` (matches the file page) |
| Licence | Apache-2.0 (model card) |

    curl -fL -o tools/models/qwen3-1.7b-q8_0.gguf \
      https://huggingface.co/Qwen/Qwen3-1.7B-GGUF/resolve/90862c4b9d2787eaed51d12237eafdfe7c5f6077/Qwen3-1.7B-Q8_0.gguf

Its 4-bit copy, `--bella qwen3-1.7b-q4`, is made from that file, not
downloaded:

    cargo xtask requant --bella qwen3-1.7b   # writes qwen3-1.7b-q4_0.gguf and its .sha256

The result is 1,129,782,944 bytes with SHA-256
`23bd087a7d67959d942223d49d2978fe43d201a6c4d8e377c6cac726eae38d40`;
the run is deterministic (docs/qwen3-design.md).

A bake-off model, chosen with `--bella qwen3.5-2b` (docs/qwen35-design.md;
measured, not Bella's model). The owner approved the bake-off's downloads
on 2026-11-06. Qwen publishes it only as safetensors:

| Field | Value |
| --- | --- |
| Model | Qwen3.5-2B, BF16 safetensors (text and vision; the text is used) |
| Folder | `qwen3.5-2b/`: `model.safetensors-00001-of-00001.safetensors` (4,548,221,488 bytes), `config.json`, `tokenizer.json`, `tokenizer_config.json`, `chat_template.jinja`, `LICENSE`, `model.safetensors.index.json` |
| Source | https://huggingface.co/Qwen/Qwen3.5-2B |
| Pinned commit | `15852e8c16360a2fea060d615a32b45270f8a8fc` |
| SHA-256 | `aa33250c4fc64891ddfaba3a314fd9542ea371843c387178b425fbcc5ed680b1` (`qwen3.5-2b.sha256`; matches the Hugging Face LFS object id) |
| Licence | Apache-2.0 (model card) |

    mkdir tools/models/qwen3.5-2b
    for f in config.json tokenizer.json tokenizer_config.json chat_template.jinja LICENSE              model.safetensors.index.json model.safetensors-00001-of-00001.safetensors; do
      curl -fL -o tools/models/qwen3.5-2b/$f         https://huggingface.co/Qwen/Qwen3.5-2B/resolve/15852e8c16360a2fea060d615a32b45270f8a8fc/$f
    done
    cargo xtask qwen35-gguf                    # qwen3.5-2b-q8_0.gguf (2,012,005,056 bytes)
    cargo xtask requant --bella qwen3.5-2b     # qwen3.5-2b-q4_0.gguf (1,325,646,528 bytes)

The converted files' SHA-256 are in `qwen3.5-2b-q8_0.gguf.sha256` and
`qwen3.5-2b-q4_0.gguf.sha256`.

The second bake-off download, not used yet (Gemma 4 E2B is not ported):
Google's own 4-bit GGUF, `google/gemma-4-E2B-it-qat-q4_0-gguf` at commit
`675cff42a74c774d6cb76f76d8eacb49b48c9b93`, file
`gemma-4-E2B_q4_0-it.gguf` (3,349,516,256 bytes, SHA-256
`fa401b55b07ee70a54c6dae3903c783a6e65064312529ea57175cb5f8dec6634`,
`gemma-4-e2b.sha256`), Apache-2.0, in `gemma-4-e2b/`.

The embedding model for Bella's file index (docs/vectors-design.md). The
owner approved this download on 2026-11-03. When it is here, the model
disk carries it as model 1:

| Field | Value |
| --- | --- |
| Model | Qwen3-Embedding-0.6B, GGUF, `Q8_0` |
| File | `qwen3-embedding-0.6b-q8_0.gguf`, 639,150,592 bytes |
| Source | https://huggingface.co/Qwen/Qwen3-Embedding-0.6B-GGUF |
| Pinned commit | `370f27d7550e0def9b39c1f16d3fbaa13aa67728` |
| SHA-256 | `06507c7b42688469c4e7298b0a1e16deff06caf291cf0a5b278c308249c3e439` (matches the file page) |
| Licence | Apache-2.0 (model card tag and the file's `general.license`) |

    curl -fL -o tools/models/qwen3-embedding-0.6b-q8_0.gguf       https://huggingface.co/Qwen/Qwen3-Embedding-0.6B-GGUF/resolve/370f27d7550e0def9b39c1f16d3fbaa13aa67728/Qwen3-Embedding-0.6B-Q8_0.gguf

The draft model for speculative decoding (docs/draft-design.md: measured
slower on the CPU, so Bella does not use it; kept for `cargo xtask
draft-check`). Approved with the "do everything" decision of 2026-11-03:

| Field | Value |
| --- | --- |
| Model | Qwen3-0.6B, GGUF, `Q8_0` |
| File | `qwen3-0.6b-q8_0.gguf`, 639,446,688 bytes |
| Source | https://huggingface.co/Qwen/Qwen3-0.6B-GGUF |
| Pinned commit | `23749fefcc72300e3a2ad315e1317431b06b590a` |
| SHA-256 | `9465e63a22add5354d9bb4b99e90117043c7124007664907259bd16d043bb031` (matches the Hugging Face LFS object id) |
| Licence | Apache-2.0 (model card) |

    curl -fL -o tools/models/qwen3-0.6b-q8_0.gguf       https://huggingface.co/Qwen/Qwen3-0.6B-GGUF/resolve/23749fefcc72300e3a2ad315e1317431b06b590a/Qwen3-0.6B-Q8_0.gguf
    cargo xtask requant --bella qwen3-0.6b   # qwen3-0.6b-q4_0.gguf, 419,245,728 bytes,
                                             # SHA-256 2a989299c3cf92df644cd7875dfee3c576b2429d0b66972a47a63233149e9c3d

To fetch the default model on a new machine (about 430 MB):

    curl -fL -o tools/models/qwen2.5-0.5b-instruct-q4_0.gguf \
      https://huggingface.co/Qwen/Qwen2.5-0.5B-Instruct-GGUF/resolve/9217f5db79a29953eb74d5343926648285ec7e67/qwen2.5-0.5b-instruct-q4_0.gguf
    sha256sum -c tools/models/qwen2.5-0.5b-instruct-q4_0.gguf.sha256   # run inside tools/models

Builds and verify cases that need the model say so and fail with a clear
message if the file is missing or its hash does not match.

## Voice

The owner approved the voice plan on 2026-11-04 (docs/voice-plan.md,
DECISIONS.md "Voice models"): Moonshine Tiny for speech to text, Kokoro-82M
for speech, Silero VAD (fetched when voice detection is built). Not
committed; `tools/models/voice/*.sha256` pin every file
(`sha256sum -c` inside `tools/models/voice`).

| Field | Moonshine Tiny | Kokoro-82M |
| --- | --- | --- |
| Use | speech to text | text to speech (voice `af_bella`) |
| Source | https://huggingface.co/moonshine-ai/moonshine-tiny (was `UsefulSensors/`) | https://huggingface.co/hexgrad/Kokoro-82M |
| Pinned commit | `390624ed33d594443aa4aa221f5b9f283b545b5a` | `f3ff3571791e39611d31c381e3a41a3af07b4987` |
| Weights | `model.safetensors`, 108,389,192 bytes, F32 | `kokoro-v1_0.pth`, 327,212,226 bytes; `voices/af_bella.pt`, 523,425 bytes |
| SHA-256 | `867cd221…48c736` (matches the LFS id) | `496dba11…18ad1e4`, `8cb64e02…8e48d6` (match the LFS ids) |
| Licence | MIT (model card) | Apache-2.0 (model card); training data includes CC BY sources, credited as the model card asks |

    M=https://huggingface.co/moonshine-ai/moonshine-tiny/resolve/390624ed33d594443aa4aa221f5b9f283b545b5a
    for f in model.safetensors config.json tokenizer.json preprocessor_config.json generation_config.json; do
      curl -fL -o tools/models/voice/moonshine-tiny/$f $M/$f; done
    K=https://huggingface.co/hexgrad/Kokoro-82M/resolve/f3ff3571791e39611d31c381e3a41a3af07b4987
    for f in kokoro-v1_0.pth config.json voices/af_bella.pt; do
      curl -fL -o tools/models/voice/kokoro-82m/$f $K/$f; done

The safetensors Moonshine replaces the plan's ONNX files: the same model
and licence, but a plain format (a JSON header and raw tensors) that the
NumPy reference and the Rust engine read without an ONNX runtime.
`kokoro-v1_0.pth` is a PyTorch zip; it is read as data (its tensors), never
unpickled with code execution. `cargo xtask speak-gguf` turns it and
`af_bella.pt` into `voice/kokoro-82m-f32.gguf` (docs/speech-design.md §2),
with misaki's word lists inside it (§6). The lists, from
https://github.com/hexgrad/misaki (Apache-2.0), commit
`fba1236595f2d2bf21d414ba6e57d25256afada3`, are pinned in
`voice/misaki.sha256`:

    M=https://raw.githubusercontent.com/hexgrad/misaki/fba1236595f2d2bf21d414ba6e57d25256afada3
    for f in misaki/data/us_gold.json misaki/data/us_silver.json misaki/en.py LICENSE; do
      curl -fsSL -o tools/models/voice/misaki/$(basename $f) $M/$f; done

`en.py` is read for its rules, never run.
