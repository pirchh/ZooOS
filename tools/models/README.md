# Model weights

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

To fetch the default model on a new machine (about 430 MB):

    curl -fL -o tools/models/qwen2.5-0.5b-instruct-q4_0.gguf \
      https://huggingface.co/Qwen/Qwen2.5-0.5B-Instruct-GGUF/resolve/9217f5db79a29953eb74d5343926648285ec7e67/qwen2.5-0.5b-instruct-q4_0.gguf
    sha256sum -c tools/models/qwen2.5-0.5b-instruct-q4_0.gguf.sha256   # run inside tools/models

Builds and verify cases that need the model say so and fail with a clear
message if the file is missing or its hash does not match.
