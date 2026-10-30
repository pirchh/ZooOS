# Reference implementation

`qwen_ref.py` is an independent NumPy implementation of the Qwen2.5 model
and tokenizer, used only to check the Rust engine (`infer/`); see
docs/m5b-design.md §2. Nothing in ZooOS depends on it.

Set-up (repository-local; the venv is ignored by Git):

    python -m venv tools/reference/.venv
    tools/reference/.venv/Scripts/python -m pip install -r tools/reference/requirements.txt   # Windows
    # or tools/reference/.venv/bin/python on Linux/macOS

Then, with the model in tools/models/:

    cargo xtask infer-check

Tested with Python 3.12 and numpy 2.3.4 (pinned in requirements.txt).
