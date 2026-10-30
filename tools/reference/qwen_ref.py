"""Independent NumPy reference for Qwen2.5-0.5B-Instruct in GGUF q4_0
(docs/m5b-design.md §4). Written separately from the Rust engine
(`infer/`): its own GGUF reader, dequantisation, tokenizer (Python `re` for
the split pattern) and forward pass. Prints `prompt_ids=…`, `output_ids=…` and `first_top=id:logit,…` lines, in
the same format as `cargo xtask infer`.

Usage: python qwen_ref.py MODEL.gguf "user message" MAX_NEW
"""
import re
import struct
import sys

import numpy as np

SYSTEM = "You are a helpful assistant."


# ---------------------------------------------------------------- GGUF
def read_gguf(path):
    data = np.memmap(path, dtype=np.uint8, mode="r")
    buf = memoryview(data)
    pos = 0

    def take(n):
        nonlocal pos
        out = bytes(buf[pos:pos + n])
        pos += n
        return out

    def u32():
        return struct.unpack("<I", take(4))[0]

    def u64():
        return struct.unpack("<Q", take(8))[0]

    def string():
        return take(u64()).decode("utf-8")

    scalar = {0: "B", 1: "b", 2: "H", 3: "h", 4: "I", 5: "i", 6: "f", 7: "?", 10: "Q", 11: "q", 12: "d"}

    def value(t):
        if t == 8:
            return string()
        if t == 9:
            et, n = u32(), u64()
            if et == 8:
                return [string() for _ in range(n)]
            fmt = scalar[et]
            size = struct.calcsize(fmt)
            return list(struct.unpack("<" + fmt * n, take(size * n)))
        return struct.unpack("<" + scalar[t], take(struct.calcsize(scalar[t])))[0]

    assert take(4) == b"GGUF" and u32() == 3
    n_tensors, n_kv = u64(), u64()
    meta = {}
    for _ in range(n_kv):
        k = string()
        meta[k] = value(u32())
    infos = []
    for _ in range(n_tensors):
        name = string()
        nd = u32()
        dims = [u64() for _ in range(nd)]
        infos.append((name, dims, u32(), u64()))
    align = meta.get("general.alignment", 32)
    start = (pos + align - 1) // align * align
    tensors = {}
    for name, dims, ty, off in infos:
        cols = dims[0]
        rows = int(np.prod(dims[1:])) if len(dims) > 1 else 1
        tensors[name] = (ty, rows, cols, start + off)
    return meta, tensors, data


def f16(raw):
    return raw.view(np.float16).astype(np.float32)


def dequant(data, info):
    ty, rows, cols, off = info
    n = rows * cols
    if ty == 0:  # F32
        return np.frombuffer(data, dtype=np.float32, count=n, offset=off).reshape(rows, cols)
    if ty == 2:  # Q4_0: [f16 d][16 bytes: low nibbles = 0..15, high = 16..31]
        blocks = np.frombuffer(data, dtype=np.uint8, count=n // 32 * 18, offset=off).reshape(-1, 18)
        d = f16(blocks[:, :2].copy()).reshape(-1, 1)
        qs = blocks[:, 2:]
        lo = (qs & 0x0F).astype(np.int8) - 8
        hi = (qs >> 4).astype(np.int8) - 8
        vals = np.concatenate([lo, hi], axis=1).astype(np.float32) * d
        return vals.reshape(rows, cols)
    if ty == 8:  # Q8_0: [f16 d][32 x i8]
        blocks = np.frombuffer(data, dtype=np.uint8, count=n // 32 * 34, offset=off).reshape(-1, 34)
        d = f16(blocks[:, :2].copy()).reshape(-1, 1)
        q = blocks[:, 2:].view(np.int8).astype(np.float32)
        return (q * d).reshape(rows, cols)
    raise ValueError(f"unsupported tensor type {ty}")


# ----------------------------------------------------------- tokenizer
def byte_chars():
    printable = list(range(33, 127)) + list(range(161, 173)) + list(range(174, 256))
    m, n = {}, 0
    for b in range(256):
        if b in printable:
            m[b] = chr(b)
        else:
            n += 1
            m[b] = chr(255 + n)
    return m


L = r"[^\W\d_]"            # letters (approximates \p{L})
N = r"\d"                  # numbers (approximates \p{N})
NOT_LN_NL = r"(?:_|[^\r\n\w])"
PUNCT = r"(?:_|[^\s\w])"
SPLIT = re.compile(
    rf"(?i:'s|'t|'re|'ve|'m|'ll|'d)|{NOT_LN_NL}?{L}+|{N}| ?{PUNCT}+[\r\n]*|\s*[\r\n]+|\s+(?!\S)|\s+"
)


class Tokenizer:
    def __init__(self, meta):
        self.tokens = meta["tokenizer.ggml.tokens"]
        types = meta["tokenizer.ggml.token_type"]
        self.vocab = {t: i for i, t in enumerate(self.tokens)}
        self.ranks = {}
        for r, m in enumerate(meta["tokenizer.ggml.merges"]):
            a, b = m.split(" ", 1)
            self.ranks.setdefault((a, b), r)
        self.special = sorted(
            [(t, i) for i, t in enumerate(self.tokens) if types[i] == 3], key=lambda x: -len(x[0])
        )
        self.bc = byte_chars()
        self.eos = meta["tokenizer.ggml.eos_token_id"]
        self.control = {i for i, t in enumerate(types) if t == 3}

    def bpe(self, piece):
        syms = [self.bc[b] for b in piece.encode("utf-8")]
        while len(syms) > 1:
            pairs = [(self.ranks.get((syms[i], syms[i + 1])), i) for i in range(len(syms) - 1)]
            pairs = [p for p in pairs if p[0] is not None]
            if not pairs:
                break
            _, i = min(pairs)
            syms[i:i + 2] = [syms[i] + syms[i + 1]]
        return [self.vocab[s] for s in syms]

    def encode(self, text):
        out = []
        pattern = "(" + "|".join(re.escape(t) for t, _ in self.special) + ")"
        for part in re.split(pattern, text):
            if not part:
                continue
            if part in self.vocab and self.vocab[part] in self.control:
                out.append(self.vocab[part])
                continue
            for piece in SPLIT.findall(part):
                out.extend(self.bpe(piece))
        return out


# --------------------------------------------------------------- model
def rmsnorm(x, w, eps):
    return x / np.sqrt(np.mean(x * x) + eps) * w


def rope(v, hd, pos, base):
    half = hd // 2
    freq = base ** (-2.0 * np.arange(half, dtype=np.float64) / hd)
    ang = pos * freq
    c, s = np.cos(ang).astype(np.float32), np.sin(ang).astype(np.float32)
    v = v.reshape(-1, hd)
    a, b = v[:, :half].copy(), v[:, half:].copy()
    v[:, :half] = a * c - b * s
    v[:, half:] = a * s + b * c
    return v.reshape(-1)


def main():
    path, user, max_new = sys.argv[1], sys.argv[2], int(sys.argv[3])
    meta, tensors, data = read_gguf(path)
    tok = Tokenizer(meta)
    prompt = (
        f"<|im_start|>system\n{SYSTEM}<|im_end|>\n<|im_start|>user\n{user}<|im_end|>\n<|im_start|>assistant\n"
    )
    ids = tok.encode(prompt)

    n_layer = meta["qwen2.block_count"]
    n_embd = meta["qwen2.embedding_length"]
    n_head = meta["qwen2.attention.head_count"]
    n_kv = meta["qwen2.attention.head_count_kv"]
    hd = n_embd // n_head
    base = meta["qwen2.rope.freq_base"]
    eps = meta["qwen2.attention.layer_norm_rms_epsilon"]
    W = lambda name: dequant(data, tensors[name])
    layers = []
    for l in range(n_layer):
        p = f"blk.{l}."
        layers.append({k: W(p + k) for k in [
            "attn_norm.weight", "attn_q.weight", "attn_k.weight", "attn_v.weight", "attn_q.bias",
            "attn_k.bias", "attn_v.bias", "attn_output.weight", "ffn_norm.weight", "ffn_gate.weight",
            "ffn_up.weight", "ffn_down.weight"]})
    out_norm = W("output_norm.weight").reshape(-1)
    output = W("output.weight") if "output.weight" in tensors else W("token_embd.weight")
    emb_info = tensors["token_embd.weight"]
    kcache = [[] for _ in range(n_layer)]
    vcache = [[] for _ in range(n_layer)]
    group = n_head // n_kv

    def forward(token, pos):
        ty, rows, cols, off = emb_info
        x = dequant(data, (ty, 1, cols, off + token * (cols // 32) * 18)).reshape(-1).astype(np.float32)
        for l, L_ in enumerate(layers):
            h = rmsnorm(x, L_["attn_norm.weight"].reshape(-1), eps)
            q = L_["attn_q.weight"] @ h + L_["attn_q.bias"].reshape(-1)
            k = L_["attn_k.weight"] @ h + L_["attn_k.bias"].reshape(-1)
            v = L_["attn_v.weight"] @ h + L_["attn_v.bias"].reshape(-1)
            q, k = rope(q, hd, pos, base), rope(k, hd, pos, base)
            kcache[l].append(k.reshape(n_kv, hd))
            vcache[l].append(v.reshape(n_kv, hd))
            K = np.stack(kcache[l])  # (t, n_kv, hd)
            V = np.stack(vcache[l])
            out = np.zeros(n_embd, dtype=np.float32)
            for hh in range(n_head):
                kvh = hh // group
                sc = K[:, kvh, :] @ q[hh * hd:(hh + 1) * hd] / np.sqrt(hd)
                sc = np.exp(sc - sc.max())
                sc /= sc.sum()
                out[hh * hd:(hh + 1) * hd] = sc @ V[:, kvh, :]
            x = x + L_["attn_output.weight"] @ out
            h = rmsnorm(x, L_["ffn_norm.weight"].reshape(-1), eps)
            g = L_["ffn_gate.weight"] @ h
            u = L_["ffn_up.weight"] @ h
            x = x + L_["ffn_down.weight"] @ (g / (1.0 + np.exp(-g)) * u)
        return output @ rmsnorm(x, out_norm, eps)

    logits = None
    for pos, t in enumerate(ids):
        logits = forward(t, pos)
    top = np.argsort(-logits)[:5]
    first_top = [[int(i), float(logits[i])] for i in top]
    out_ids = []
    pos = len(ids)
    for _ in range(max_new):
        nxt = int(np.argmax(logits))
        if nxt == tok.eos or nxt in tok.control:
            break
        out_ids.append(nxt)
        logits = forward(nxt, pos)
        pos += 1
    # Same line format as `cargo xtask infer`, so the two can be compared.
    print("prompt_ids=" + ",".join(map(str, ids)))
    print("output_ids=" + ",".join(map(str, out_ids)))
    print("first_top=" + ",".join(f"{i}:{l:.4f}" for i, l in first_top))


if __name__ == "__main__":
    main()
