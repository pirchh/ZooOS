"""Moonshine speech to text in NumPy: an independent reference for the Rust
engine (docs/hearing-design.md). Nothing in ZooOS depends on it.

    python moonshine_ref.py <model dir> <16 kHz mono 16-bit WAV> [--dump <file>]

Prints the token ids and the transcript. With --dump, writes the encoder
output and the first step's logits as raw little-endian f32 for comparing.
"""

import json
import math
import struct
import sys
import wave

import numpy as np

EPS = 1e-5


def load_safetensors(path):
    with open(path, "rb") as f:
        n = struct.unpack("<Q", f.read(8))[0]
        header = json.loads(f.read(n))
        data = f.read()
    t = {}
    for name, v in header.items():
        if name == "__metadata__":
            continue
        assert v["dtype"] == "F32", (name, v["dtype"])
        a, b = v["data_offsets"]
        t[name] = np.frombuffer(data[a:b], dtype="<f4").reshape(v["shape"])
    return t


def erf(x):
    # Abramowitz and Stegun 7.1.26 is not precise enough for comparing;
    # math.erf per element is exact and fast enough at this size.
    return np.vectorize(math.erf, otypes=[np.float32])(x)


def gelu(x):
    return 0.5 * x * (1.0 + erf(x / math.sqrt(2.0)))


def silu(x):
    return x / (1.0 + np.exp(-x))


def layer_norm(x, w, b=None):
    m = x.mean(-1, keepdims=True)
    v = ((x - m) ** 2).mean(-1, keepdims=True)
    y = (x - m) / np.sqrt(v + EPS) * w
    return y if b is None else y + b


def conv1d(x, w, b, stride):
    # x: (C_in, T), w: (C_out, C_in, K)
    c_out, c_in, k = w.shape
    t_out = (x.shape[1] - k) // stride + 1
    idx = np.arange(k)[None, :] + stride * np.arange(t_out)[:, None]
    cols = x[:, idx]  # (C_in, t_out, K)
    y = np.einsum("ctk,ock->ot", cols, w, optimize=True)
    if b is not None:
        y = y + b[:, None]
    return y


class Moonshine:
    def __init__(self, model_dir):
        cfg = json.load(open(f"{model_dir}/config.json"))
        self.t = load_safetensors(f"{model_dir}/model.safetensors")
        self.d = cfg["hidden_size"]
        self.heads = cfg["encoder_num_attention_heads"]
        self.hd = self.d // self.heads
        rot = int(self.hd * cfg["partial_rotary_factor"])
        self.rot = rot - rot % 2
        self.theta = cfg["rope_theta"]
        self.enc_layers = cfg["encoder_num_hidden_layers"]
        self.dec_layers = cfg["decoder_num_hidden_layers"]
        self.bos = cfg["decoder_start_token_id"]
        self.eos = cfg["eos_token_id"]
        tok = json.load(open(f"{model_dir}/tokenizer.json", encoding="utf-8"))
        self.vocab = {i: s for s, i in tok["model"]["vocab"].items()}
        for a in tok.get("added_tokens", []):
            self.vocab[a["id"]] = a["content"]

    def rope(self, x, pos):
        # x: (heads, T, hd). Interleaved pairs (0,1), (2,3), ... over the
        # first `rot` dimensions; the rest pass through.
        inv = 1.0 / self.theta ** (np.arange(0, self.rot, 2, dtype=np.float32) / self.rot)
        ang = np.asarray(pos, dtype=np.float32)[:, None] * inv[None, :]  # (T, rot/2)
        cos, sin = np.cos(ang), np.sin(ang)
        r, rest = x[..., : self.rot], x[..., self.rot :]
        x1, x2 = r[..., 0::2], r[..., 1::2]
        o = np.empty_like(r)
        o[..., 0::2] = x1 * cos - x2 * sin
        o[..., 1::2] = x2 * cos + x1 * sin
        return np.concatenate([o, rest], -1)

    def split(self, x):
        return x.reshape(x.shape[0], self.heads, self.hd).transpose(1, 0, 2)

    def attend(self, q, k, v, causal_from=None):
        s = q @ k.transpose(0, 2, 1) / math.sqrt(self.hd)
        if causal_from is not None:
            tq, tk = s.shape[1], s.shape[2]
            mask = np.arange(tk)[None, :] > (causal_from + np.arange(tq))[:, None]
            s = np.where(mask[None], -np.inf, s)
        s = s - s.max(-1, keepdims=True)
        p = np.exp(s)
        p /= p.sum(-1, keepdims=True)
        o = p @ v  # (heads, Tq, hd)
        return o.transpose(1, 0, 2).reshape(q.shape[1], self.d)

    def encode(self, audio):
        t = self.t
        x = audio[None, :].astype(np.float32)
        x = np.tanh(conv1d(x, t["model.encoder.conv1.weight"], None, 64))
        # GroupNorm with one group: over all channels and times.
        m, v = x.mean(), x.var()
        x = (x - m) / np.sqrt(v + EPS)
        x = x * t["model.encoder.groupnorm.weight"][:, None] + t["model.encoder.groupnorm.bias"][:, None]
        x = gelu(conv1d(x, t["model.encoder.conv2.weight"], t["model.encoder.conv2.bias"], 3))
        x = gelu(conv1d(x, t["model.encoder.conv3.weight"], t["model.encoder.conv3.bias"], 2))
        x = x.T.copy()  # (T, d)
        pos = np.arange(x.shape[0])
        for i in range(self.enc_layers):
            p = f"model.encoder.layers.{i}."
            h = layer_norm(x, t[p + "input_layernorm.weight"])
            q = self.rope(self.split(h @ t[p + "self_attn.q_proj.weight"].T), pos)
            k = self.rope(self.split(h @ t[p + "self_attn.k_proj.weight"].T), pos)
            v = self.split(h @ t[p + "self_attn.v_proj.weight"].T)
            x = x + self.attend(q, k, v) @ t[p + "self_attn.o_proj.weight"].T
            h = layer_norm(x, t[p + "post_attention_layernorm.weight"])
            h = gelu(h @ t[p + "mlp.fc1.weight"].T + t[p + "mlp.fc1.bias"])
            x = x + h @ t[p + "mlp.fc2.weight"].T + t[p + "mlp.fc2.bias"]
        return layer_norm(x, t["model.encoder.layer_norm.weight"])

    def decode(self, enc, max_tokens):
        t = self.t
        cross = []
        for i in range(self.dec_layers):
            p = f"model.decoder.layers.{i}.encoder_attn."
            cross.append((self.split(enc @ t[p + "k_proj.weight"].T), self.split(enc @ t[p + "v_proj.weight"].T)))
        cache = [(np.zeros((self.heads, 0, self.hd), np.float32),) * 2 for _ in range(self.dec_layers)]
        ids, tok, first_logits = [], self.bos, None
        emb = t["model.decoder.embed_tokens.weight"]
        for step in range(max_tokens):
            x = emb[tok][None, :]
            for i in range(self.dec_layers):
                p = f"model.decoder.layers.{i}."
                h = layer_norm(x, t[p + "input_layernorm.weight"])
                q = self.rope(self.split(h @ t[p + "self_attn.q_proj.weight"].T), [step])
                k = self.rope(self.split(h @ t[p + "self_attn.k_proj.weight"].T), [step])
                v = self.split(h @ t[p + "self_attn.v_proj.weight"].T)
                ck, cv = cache[i]
                ck, cv = np.concatenate([ck, k], 1), np.concatenate([cv, v], 1)
                cache[i] = (ck, cv)
                x = x + self.attend(q, ck, cv) @ t[p + "self_attn.o_proj.weight"].T
                h = layer_norm(x, t[p + "post_attention_layernorm.weight"])
                q = self.split(h @ t[p + "encoder_attn.q_proj.weight"].T)
                x = x + self.attend(q, *cross[i]) @ t[p + "encoder_attn.o_proj.weight"].T
                h = layer_norm(x, t[p + "final_layernorm.weight"])
                h = h @ t[p + "mlp.fc1.weight"].T + t[p + "mlp.fc1.bias"]
                val, gate = np.split(h, 2, -1)
                x = x + (silu(gate) * val) @ t[p + "mlp.fc2.weight"].T + t[p + "mlp.fc2.bias"]
            logits = (layer_norm(x, t["model.decoder.norm.weight"]) @ emb.T)[0]
            if first_logits is None:
                first_logits = logits
            tok = int(np.argmax(logits))
            if tok == self.eos:
                break
            ids.append(tok)
        return ids, first_logits

    def text(self, ids):
        out = bytearray()
        for i in ids:
            s = self.vocab.get(i, "")
            if len(s) == 6 and s.startswith("<0x") and s.endswith(">"):
                out.append(int(s[3:5], 16))
            elif not (s.startswith("<") and s.endswith(">")):
                out += s.replace("▁", " ").encode("utf-8")
        return out.decode("utf-8", "replace").strip()


def read_wav(path):
    with wave.open(path, "rb") as w:
        assert w.getframerate() == 16000 and w.getnchannels() == 1 and w.getsampwidth() == 2, path
        a = np.frombuffer(w.readframes(w.getnframes()), dtype="<i2")
    return a.astype(np.float32) / 32768.0


def main():
    model_dir, wav = sys.argv[1], sys.argv[2]
    m = Moonshine(model_dir)
    audio = read_wav(wav)
    enc = m.encode(audio)
    # Moonshine's own limit: about 6.5 tokens a second of audio.
    max_tokens = int(math.ceil(len(audio) / 16000 * 6.5)) + 1
    ids, first = m.decode(enc, max_tokens)
    print("frames", enc.shape[0])
    print("ids", " ".join(map(str, ids)))
    print("text", m.text(ids))
    if "--dump" in sys.argv:
        with open(sys.argv[sys.argv.index("--dump") + 1], "wb") as f:
            f.write(enc.astype("<f4").tobytes())
            f.write(first.astype("<f4").tobytes())


if __name__ == "__main__":
    main()
