"""NumPy reference for the text part of Qwen3.5-2B (docs/qwen35-design.md).

Reads the official BF16 safetensors directly and runs token ids through
all 24 layers, written from the Hugging Face transformers implementation
(`modeling_qwen3_5.py`): 18 Gated DeltaNet layers and 6 gated full
attention layers. Slow and simple on purpose; it is the check for the
engine, not an engine.

    python -I qwen35_ref.py MODEL_DIR [--ids 1,2,3 | --words The,Ġcapital] [--gen N] [--q8]

Prints the top-5 logits after the prompt and each new token's id. With
`--q8`, every weight matrix is first rounded to Q8_0 as `cargo xtask
qwen35-gguf` stores it (ggml's `quantize_row_q8_0_ref`), so what is left
between this and the engine is the engine's own arithmetic.
"""

import json
import sys

import numpy as np


def q8_round(w):
    """A matrix through Q8_0 and back: blocks of 32 along each row."""
    rows, cols = w.shape
    b = w.reshape(rows, cols // 32, 32)
    d = np.abs(b).max(-1, keepdims=True) / 127.0
    with np.errstate(divide="ignore", invalid="ignore", over="ignore"):
        inv = np.where(d != 0, 1.0 / np.where(d != 0, d, 1), 0).astype(np.float32)
        x = b * inv
        # Rounded half away from zero and cast as Rust's `as i8` does:
        # saturating, NaN (0 times an overflowed 1/d) to 0.
        q = np.clip(np.nan_to_num(np.trunc(x + np.copysign(0.5, x)), nan=0.0), -128, 127)
    return (q * d.astype(np.float16).astype(np.float32)).reshape(rows, cols)


class Weights:
    q8 = False

    def __init__(self, path):
        with open(path, "rb") as f:
            n = int.from_bytes(f.read(8), "little")
            self.header = json.loads(f.read(n))
        self.base = 8 + n
        self.data = np.memmap(path, dtype=np.uint8, mode="r")

    def get(self, name):
        info = self.header[name]
        a, b = info["data_offsets"]
        raw = self.data[self.base + a : self.base + b]
        if info["dtype"] == "F32":
            w = raw.view(np.float32).reshape(info["shape"]).copy()
        else:
            assert info["dtype"] == "BF16", (name, info["dtype"])
            w = (raw.view(np.uint16).astype(np.uint32) << 16).view(np.float32).reshape(info["shape"])
        if self.q8 and w.ndim == 2:
            w = q8_round(w)
        return w


def rms(x, eps):
    return x / np.sqrt((x * x).mean(-1, keepdims=True) + eps)


def silu(x):
    return x / (1.0 + np.exp(-x))


def softplus(x):
    return np.log1p(np.exp(x))


def sigmoid(x):
    return 1.0 / (1.0 + np.exp(-x))


def run(w, cfg, ids):
    t = cfg["text_config"]
    eps = t["rms_norm_eps"]
    hd = t["head_dim"]
    nh = t["num_attention_heads"]
    nkv = t["num_key_value_heads"]
    rope = t["rope_parameters"]
    rot = int(hd * rope["partial_rotary_factor"])
    theta = rope["rope_theta"]
    lk = t["linear_num_key_heads"]
    lv = t["linear_num_value_heads"]
    dk = t["linear_key_head_dim"]
    dv = t["linear_value_head_dim"]
    ker = t["linear_conv_kernel_dim"]
    p = "model.language_model."
    T = len(ids)
    emb = w.get(p + "embed_tokens.weight")
    x = emb[ids].copy()
    inv = 1.0 / theta ** (np.arange(0, rot, 2, dtype=np.float64) / rot)
    ang = np.arange(T)[:, None] * inv[None, :]
    cos = np.cos(np.concatenate([ang, ang], -1)).astype(np.float32)
    sin = np.sin(np.concatenate([ang, ang], -1)).astype(np.float32)

    def rope_apply(v):  # v: [T, heads, hd]
        r, rest = v[..., :rot], v[..., rot:]
        half = rot // 2
        rh = np.concatenate([-r[..., half:], r[..., :half]], -1)
        r = r * cos[:, None, :] + rh * sin[:, None, :]
        return np.concatenate([r, rest], -1)

    for i, kind in enumerate(t["layer_types"]):
        L = p + f"layers.{i}."
        h = rms(x, eps) * (1.0 + w.get(L + "input_layernorm.weight"))
        if kind == "full_attention":
            qg = (h @ w.get(L + "self_attn.q_proj.weight").T).reshape(T, nh, 2 * hd)
            q, gate = qg[..., :hd], qg[..., hd:].reshape(T, nh * hd)
            k = (h @ w.get(L + "self_attn.k_proj.weight").T).reshape(T, nkv, hd)
            v = (h @ w.get(L + "self_attn.v_proj.weight").T).reshape(T, nkv, hd)
            q = rms(q, eps) * (1.0 + w.get(L + "self_attn.q_norm.weight"))
            k = rms(k, eps) * (1.0 + w.get(L + "self_attn.k_norm.weight"))
            q, k = rope_apply(q), rope_apply(k)
            out = np.zeros((T, nh, hd), np.float32)
            group = nh // nkv
            mask = np.triu(np.full((T, T), -np.inf, np.float32), 1)
            for hh in range(nh):
                s = q[:, hh] @ k[:, hh // group].T * hd**-0.5 + mask
                s = np.exp(s - s.max(-1, keepdims=True))
                s /= s.sum(-1, keepdims=True)
                out[:, hh] = s @ v[:, hh // group]
            out = out.reshape(T, nh * hd) * sigmoid(gate)
            mix = out @ w.get(L + "self_attn.o_proj.weight").T
        else:
            A = L + "linear_attn."
            qkv = h @ w.get(A + "in_proj_qkv.weight").T  # [T, 6144]
            z = (h @ w.get(A + "in_proj_z.weight").T).reshape(T, lv, dv)
            b = h @ w.get(A + "in_proj_b.weight").T
            a = h @ w.get(A + "in_proj_a.weight").T
            cw = w.get(A + "conv1d.weight").reshape(qkv.shape[1], ker)
            pad = np.concatenate([np.zeros((ker - 1, qkv.shape[1]), np.float32), qkv], 0)
            conv = np.zeros_like(qkv)
            for j in range(ker):
                conv += pad[j : j + T] * cw[:, j]
            conv = silu(conv)
            kd = lk * dk
            q = conv[:, :kd].reshape(T, lk, dk)
            k = conv[:, kd : 2 * kd].reshape(T, lk, dk)
            v = conv[:, 2 * kd :].reshape(T, lv, dv)
            q = q / np.sqrt((q * q).sum(-1, keepdims=True) + 1e-6)
            k = k / np.sqrt((k * k).sum(-1, keepdims=True) + 1e-6)
            q = q / np.sqrt(dk)
            beta = sigmoid(b)
            g = -np.exp(w.get(A + "A_log")) * softplus(a + w.get(A + "dt_bias"))
            S = np.zeros((lv, dk, dv), np.float32)
            o = np.zeros((T, lv, dv), np.float32)
            for tt in range(T):
                S = S * np.exp(g[tt])[:, None, None]
                mem = np.einsum("hkv,hk->hv", S, k[tt])
                delta = (v[tt] - mem) * beta[tt][:, None]
                S = S + k[tt][:, :, None] * delta[:, None, :]
                o[tt] = np.einsum("hkv,hk->hv", S, q[tt])
            o = rms(o, eps) * w.get(A + "norm.weight") * silu(z)
            mix = o.reshape(T, lv * dv) @ w.get(A + "out_proj.weight").T
        x = x + mix
        h = rms(x, eps) * (1.0 + w.get(L + "post_attention_layernorm.weight"))
        gu = silu(h @ w.get(L + "mlp.gate_proj.weight").T) * (h @ w.get(L + "mlp.up_proj.weight").T)
        x = x + gu @ w.get(L + "mlp.down_proj.weight").T
    x = rms(x, eps) * (1.0 + w.get(p + "norm.weight"))
    return x[-1] @ emb.T


def main():
    d = sys.argv[1]
    args = sys.argv[2:]
    cfg = json.load(open(d + "/config.json", encoding="utf-8"))
    w = Weights(d + "/model.safetensors-00001-of-00001.safetensors")
    w.q8 = "--q8" in args
    gen = int(args[args.index("--gen") + 1]) if "--gen" in args else 0
    if "--ids" in args:
        ids = [int(s) for s in args[args.index("--ids") + 1].split(",")]
    else:
        vocab = json.load(open(d + "/tokenizer.json", encoding="utf-8"))["model"]["vocab"]
        ids = [vocab[s] for s in args[args.index("--words") + 1].split(",")]
    print("ids", ",".join(map(str, ids)))
    logits = run(w, cfg, ids)
    top = np.argsort(-logits)[:5]
    print("top5", " ".join(f"{i}:{logits[i]:.4f}" for i in top))
    for _ in range(gen):
        nxt = int(np.argmax(logits))
        ids.append(nxt)
        print("next", nxt, flush=True)
        logits = run(w, cfg, ids)


if __name__ == "__main__":
    main()
