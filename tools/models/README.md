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

To fetch the default model on a new machine (about 430 MB):

    curl -fL -o tools/models/qwen2.5-0.5b-instruct-q4_0.gguf \
      https://huggingface.co/Qwen/Qwen2.5-0.5B-Instruct-GGUF/resolve/9217f5db79a29953eb74d5343926648285ec7e67/qwen2.5-0.5b-instruct-q4_0.gguf
    sha256sum -c tools/models/qwen2.5-0.5b-instruct-q4_0.gguf.sha256   # run inside tools/models

Builds and verify cases that need the model say so and fail with a clear
message if the file is missing or its hash does not match.
