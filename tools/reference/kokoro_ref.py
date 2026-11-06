"""Kokoro-82M text to speech in NumPy: an independent reference for the Rust
engine (docs/speech-design.md). Nothing in ZooOS depends on it.

    python -I kokoro_ref.py <model.safetensors> <voice.safetensors> <config.json> <phonemes> <out.wav> [--dump <file>]

Reads the tensors that kokoro_pth.py wrote. Takes phonemes (misaki's
alphabet, as config.json's vocab), writes 24 kHz mono 16-bit audio.
Kokoro's sine source draws random numbers; here they come from `Noise`, a
small generator the Rust engine repeats exactly, so both give the same
samples.
"""

import json
import math
import struct
import sys
import wave

import numpy as np

SR = 24000
# `--f32`: compute in single precision, to see how much precision alone
# moves the output.
DTYPE = np.float32 if "--f32" in sys.argv else np.float64


def load_safetensors(path):
    with open(path, "rb") as f:
        n = struct.unpack("<Q", f.read(8))[0]
        header = json.loads(f.read(n))
        data = f.read()
    t = {}
    for name, v in header.items():
        if name == "__metadata__":
            continue
        a, b = v["data_offsets"]
        t[name.replace(".module.", ".")] = (
            np.frombuffer(data[a:b], dtype="<f4").reshape(v["shape"]).astype(DTYPE)
        )
    return t


class Noise:
    """xorshift64* and Box-Muller: the same sequence as zoo_infer::speak."""

    def __init__(self, seed=0x9E3779B97F4A7C15):
        self.s = seed

    def u32(self):
        s = self.s
        s ^= (s >> 12)
        s ^= (s << 25) & 0xFFFFFFFFFFFFFFFF
        s ^= (s >> 27)
        self.s = s
        return ((s * 0x2545F4914F6CDD1D) & 0xFFFFFFFFFFFFFFFF) >> 32

    def uniform(self):
        return (self.u32() + 0.5) / 4294967296.0

    def normal(self, n):
        out = np.empty(n)
        for i in range(0, n, 2):
            u1, u2 = self.uniform(), self.uniform()
            r = math.sqrt(-2.0 * math.log(u1))
            out[i] = r * math.cos(2 * math.pi * u2)
            if i + 1 < n:
                out[i + 1] = r * math.sin(2 * math.pi * u2)
        return out


TRACE = {}

# --- building blocks -------------------------------------------------------


def wn(t, p):
    """A weight-normed weight: g * v / |v|, the norm over all but dim 0."""
    v = t[p + ".weight_v"]
    g = t[p + ".weight_g"]
    n = np.sqrt((v.reshape(v.shape[0], -1) ** 2).sum(1))
    return v * (g.reshape(-1) / n).reshape((-1,) + (1,) * (v.ndim - 1))


def conv1d(x, w, b=None, stride=1, pad=0, dil=1, groups=1):
    """x [C, T], w [O, C/groups, K]."""
    c, t = x.shape
    o, cg, k = w.shape
    xp = np.pad(x, ((0, 0), (pad, pad)))
    tout = (t + 2 * pad - dil * (k - 1) - 1) // stride + 1
    idx = np.arange(tout)[:, None] * stride + np.arange(k)[None, :] * dil  # [Tout, K]
    if groups == 1:
        cols = xp[:, idx]  # [C, Tout, K]
        y = np.einsum("ctk,ock->ot", cols, w, optimize=True)
    else:
        assert groups == c and cg == 1 and o == c
        cols = xp[:, idx]
        y = np.einsum("ctk,ck->ct", cols, w[:, 0, :])
    if b is not None:
        y = y + b[:, None]
    return y


def conv_transpose1d(x, w, b=None, stride=1, pad=0, out_pad=0, groups=1):
    """x [C, T], w [C, O/groups, K] (PyTorch's layout)."""
    c, t = x.shape
    _, og, k = w.shape
    full = (t - 1) * stride + k
    if groups == 1:
        y = np.zeros((og, full + out_pad))
        contrib = np.einsum("ct,cok->otk", x, w, optimize=True)  # [O, T, K]
        for j in range(k):
            y[:, j : j + (t - 1) * stride + 1 : stride] += contrib[:, :, j]
    else:
        assert groups == c and og == 1
        y = np.zeros((c, full + out_pad))
        for j in range(k):
            y[:, j : j + (t - 1) * stride + 1 : stride] += x * w[:, 0, j][:, None]
    tout = (t - 1) * stride - 2 * pad + k + out_pad
    y = y[:, pad : pad + tout]
    if b is not None:
        y = y + b[:, None]
    return y


def leaky(x, a):
    return np.where(x > 0, x, a * x)


def sigmoid(x):
    return 1.0 / (1.0 + np.exp(-x))


def layer_norm(x, g=None, b=None, eps=1e-5):
    """Over the last axis."""
    m = x.mean(-1, keepdims=True)
    v = ((x - m) ** 2).mean(-1, keepdims=True)
    y = (x - m) / np.sqrt(v + eps)
    if g is not None:
        y = y * g + b
    return y


def lstm(x, t, p):
    """Bidirectional, one layer: x [T, I] -> [T, 2H]."""

    def run(x, sfx):
        wi, wh = t[p + ".weight_ih_l0" + sfx], t[p + ".weight_hh_l0" + sfx]
        bias = t[p + ".bias_ih_l0" + sfx] + t[p + ".bias_hh_l0" + sfx]
        h4 = wi.shape[0]
        hs = h4 // 4
        h = np.zeros(hs)
        c = np.zeros(hs)
        xi = x @ wi.T + bias
        out = np.zeros((x.shape[0], hs))
        for k in range(x.shape[0]):
            z = xi[k] + wh @ h
            i, f, g, o = z[:hs], z[hs : 2 * hs], z[2 * hs : 3 * hs], z[3 * hs :]
            c = sigmoid(f) * c + sigmoid(i) * np.tanh(g)
            h = sigmoid(o) * np.tanh(c)
            out[k] = h
        return out

    fwd = run(x, "")
    bwd = run(x[::-1], "_reverse")[::-1]
    return np.concatenate([fwd, bwd], axis=1)


def linear(x, t, p):
    return x @ t[p + ".weight"].T + t[p + ".bias"]


def adain(x, s, t, p):
    """AdaIN1d: instance norm over time, then (1 + gamma) x + beta."""
    h = linear(s, t, p + ".fc")
    c = x.shape[0]
    gamma, beta = h[:c], h[c:]
    m = x.mean(1, keepdims=True)
    v = ((x - m) ** 2).mean(1, keepdims=True)
    xn = (x - m) / np.sqrt(v + 1e-5)
    return (1 + gamma)[:, None] * xn + beta[:, None]


def adain_resblk(x, s, t, p, upsample=False):
    """AdainResBlk1d: x [C, T]."""
    learned = (p + ".conv1x1.weight_v") in t
    # Residual.
    r = leaky(adain(x, s, t, p + ".norm1"), 0.2)
    if upsample:
        r = conv_transpose1d(
            r, wn(t, p + ".pool"), t[p + ".pool.bias"], stride=2, pad=1, out_pad=1, groups=r.shape[0]
        )
    r = conv1d(r, wn(t, p + ".conv1"), t[p + ".conv1.bias"], pad=1)
    r = leaky(adain(r, s, t, p + ".norm2"), 0.2)
    r = conv1d(r, wn(t, p + ".conv2"), t[p + ".conv2.bias"], pad=1)
    # Shortcut.
    sc = np.repeat(x, 2, axis=1) if upsample else x
    if learned:
        sc = conv1d(sc, wn(t, p + ".conv1x1"))
    return (r + sc) / math.sqrt(2)


def adain_resblock1(x, s, t, p, k, dil):
    """AdaINResBlock1 (Snake activations)."""
    for i in range(3):
        a1 = t[f"{p}.alpha1.{i}"].reshape(-1)[:, None]
        a2 = t[f"{p}.alpha2.{i}"].reshape(-1)[:, None]
        xt = adain(x, s, t, f"{p}.adain1.{i}")
        xt = xt + (1 / a1) * np.sin(a1 * xt) ** 2
        d = dil[i]
        xt = conv1d(xt, wn(t, f"{p}.convs1.{i}"), t[f"{p}.convs1.{i}.bias"], pad=(k * d - d) // 2, dil=d)
        xt = adain(xt, s, t, f"{p}.adain2.{i}")
        xt = xt + (1 / a2) * np.sin(a2 * xt) ** 2
        xt = conv1d(xt, wn(t, f"{p}.convs2.{i}"), t[f"{p}.convs2.{i}.bias"], pad=(k - 1) // 2)
        x = xt + x
    return x


def interp_linear(x, scale, out_len):
    """F.interpolate(mode='linear', align_corners=False) along axis 0 with
    the given scale factor: source = (i + 0.5) / scale - 0.5."""
    n = x.shape[0]
    src = (np.arange(out_len) + 0.5) / scale - 0.5
    src = np.maximum(src, 0.0)
    i0 = np.minimum(np.floor(src).astype(np.int64), n - 1)
    i1 = np.minimum(i0 + 1, n - 1)
    w = (src - i0)[:, None] if x.ndim == 2 else (src - i0)
    return x[i0] * (1 - w) + x[i1] * w


def hann(n):
    return 0.5 - 0.5 * np.cos(2 * np.pi * np.arange(n) / n)


def stft(x, n_fft, hop):
    """torch.stft(center=True, reflect padding, periodic Hann)."""
    p = n_fft // 2
    xp = np.pad(x, (p, p), mode="reflect")
    frames = 1 + (len(xp) - n_fft) // hop
    win = hann(n_fft)
    idx = np.arange(frames)[:, None] * hop + np.arange(n_fft)[None, :]
    seg = xp[idx] * win  # [F, N]
    k = np.arange(n_fft // 2 + 1)
    basis = np.exp(-2j * np.pi * np.outer(np.arange(n_fft), k) / n_fft)  # [N, K]
    spec = seg @ basis  # [F, K]
    # DC and Nyquist are real: an FFT gives them no imaginary part, so a
    # negative value's angle is pi, never -pi from rounding (6.28 apart
    # as the decoder's input).
    spec[:, 0] = spec[:, 0].real
    spec[:, -1] = spec[:, -1].real
    return np.abs(spec).T, np.angle(spec).T  # [K, F]


def istft(mag, phase, n_fft, hop):
    """torch.istft(center=True), periodic Hann."""
    spec = (mag * np.exp(1j * phase)).T  # [F, K]
    frames = spec.shape[0]
    n = np.arange(n_fft)
    k = np.arange(n_fft // 2 + 1)
    # Real inverse DFT of a one-sided spectrum.
    wk = np.ones(len(k)) * 2
    wk[0] = 1
    if n_fft % 2 == 0:
        wk[-1] = 1
    basis = np.exp(2j * np.pi * np.outer(k, n) / n_fft)  # [K, N]
    seg = np.real((spec * wk) @ basis) / n_fft  # [F, N]
    win = hann(n_fft)
    total = n_fft + hop * (frames - 1)
    y = np.zeros(total)
    wsum = np.zeros(total)
    for f in range(frames):
        y[f * hop : f * hop + n_fft] += seg[f] * win
        wsum[f * hop : f * hop + n_fft] += win**2
    p = n_fft // 2
    y = y[p : total - p]
    wsum = wsum[p : total - p]
    return y / np.where(wsum > 1e-11, wsum, 1.0)


# --- the model ---------------------------------------------------------------


def albert(ids, t):
    p = "bert"
    T = len(ids)
    x = (
        t[p + ".embeddings.word_embeddings.weight"][ids]
        + t[p + ".embeddings.position_embeddings.weight"][:T]
        + t[p + ".embeddings.token_type_embeddings.weight"][0]
    )
    x = layer_norm(
        x, t[p + ".embeddings.LayerNorm.weight"], t[p + ".embeddings.LayerNorm.bias"], 1e-12
    )
    x = linear(x, t, p + ".encoder.embedding_hidden_mapping_in")
    L = p + ".encoder.albert_layer_groups.0.albert_layers.0"
    heads = 12
    hd = 768 // heads
    for _ in range(12):
        q = linear(x, t, L + ".attention.query").reshape(T, heads, hd).transpose(1, 0, 2)
        k = linear(x, t, L + ".attention.key").reshape(T, heads, hd).transpose(1, 0, 2)
        v = linear(x, t, L + ".attention.value").reshape(T, heads, hd).transpose(1, 0, 2)
        sc = q @ k.transpose(0, 2, 1) / math.sqrt(hd)
        sc = np.exp(sc - sc.max(-1, keepdims=True))
        sc = sc / sc.sum(-1, keepdims=True)
        ctx = (sc @ v).transpose(1, 0, 2).reshape(T, 768)
        a = linear(ctx, t, L + ".attention.dense")
        a = layer_norm(a + x, t[L + ".attention.LayerNorm.weight"], t[L + ".attention.LayerNorm.bias"], 1e-12)
        f = linear(a, t, L + ".ffn")
        f = 0.5 * f * (1 + np.tanh(math.sqrt(2 / math.pi) * (f + 0.044715 * f**3)))  # gelu_new
        f = linear(f, t, L + ".ffn_output")
        x = layer_norm(f + a, t[L + ".full_layer_layer_norm.weight"], t[L + ".full_layer_layer_norm.bias"], 1e-12)
    return x  # [T, 768]


def ada_layer_norm(x, s, t, p):
    """x [T, C]: layer norm over C, then (1 + gamma) x + beta."""
    h = linear(s, t, p + ".fc")
    c = x.shape[1]
    return (1 + h[:c]) * layer_norm(x) + h[c:]


def duration_encoder(d_en, s, t):
    """d_en [T, 512] -> [T, 640]."""
    T = d_en.shape[0]
    sT = np.repeat(s[None, :], T, axis=0)
    x = np.concatenate([d_en, sT], axis=1)
    p = "predictor.text_encoder.lstms"
    for i in range(6):
        if i % 2 == 0:
            x = lstm(x, t, f"{p}.{i}")
        else:
            x = ada_layer_norm(x, s, t, f"{p}.{i}")
            x = np.concatenate([x, sT], axis=1)
    return x


def text_encoder(ids, t):
    x = t["text_encoder.embedding.weight"][ids].T  # [512, T]
    for i in range(3):
        p = f"text_encoder.cnn.{i}"
        x = conv1d(x, wn(t, p + ".0"), t[p + ".0.bias"], pad=2)
        x = layer_norm(x.T, t[p + ".1.gamma"], t[p + ".1.beta"]).T
        x = leaky(x, 0.2)
    return lstm(x.T, t, "text_encoder.lstm").T  # [512, T]


def generator(x, s, f0, t, cfg, noise):
    g = "decoder.generator"
    ist = cfg["istftnet"]
    rates = ist["upsample_rates"]
    ks = ist["upsample_kernel_sizes"]
    n_fft = ist["gen_istft_n_fft"]
    hop = ist["gen_istft_hop_size"]
    scale = int(np.prod(rates)) * hop  # 300
    # The harmonic source (SineGen, SourceModuleHnNSF).
    f0u = np.repeat(f0, scale)  # [L]
    L = len(f0u)
    harm = np.arange(1, 10)
    fn = f0u[:, None] * harm[None, :]  # [L, 9]
    rad = (fn / SR) % 1.0
    rad_d = interp_linear(rad, 1.0 / scale, L // scale)
    phase = np.cumsum(rad_d, axis=0) * 2 * np.pi
    phase = interp_linear(phase * scale, float(scale), L)
    sines = np.sin(phase) * 0.1
    uv = (f0u > 10).astype(np.float64)[:, None]
    noise_amp = uv * 0.003 + (1 - uv) * 0.1 / 3
    sines = sines * uv + noise_amp * noise.normal(L * 9).reshape(L, 9)
    har_source = np.tanh(sines @ t[g + ".m_source.l_linear.weight"][0] + t[g + ".m_source.l_linear.bias"][0])
    spec, ph = stft(har_source, n_fft, hop)
    har = np.concatenate([spec, ph], axis=0)  # [22, frames]
    TRACE["har"] = har
    nk = len(ist["resblock_kernel_sizes"])
    for i in range(len(rates)):
        x = leaky(x, 0.1)
        if i + 1 < len(rates):
            sf = int(np.prod(rates[i + 1 :]))
            xs = conv1d(har, t[f"{g}.noise_convs.{i}.weight"], t[f"{g}.noise_convs.{i}.bias"], stride=sf, pad=(sf + 1) // 2)
            TRACE[f"nc{i}"] = xs
            xs = adain_resblock1(xs, s, t, f"{g}.noise_res.{i}", 7, [1, 3, 5])
        else:
            xs = conv1d(har, t[f"{g}.noise_convs.{i}.weight"], t[f"{g}.noise_convs.{i}.bias"])
            TRACE[f"nc{i}"] = xs
            xs = adain_resblock1(xs, s, t, f"{g}.noise_res.{i}", 11, [1, 3, 5])
        u, k = rates[i], ks[i]
        x = conv_transpose1d(x, wn(t, f"{g}.ups.{i}"), t[f"{g}.ups.{i}.bias"], stride=u, pad=(k - u) // 2)
        if i == len(rates) - 1:
            x = np.concatenate([x[:, 1:2], x], axis=1)  # ReflectionPad1d((1, 0))
        TRACE[f"xs{i}"] = xs
        TRACE[f"up{i}"] = x
        x = x + xs
        acc = None
        for j in range(nk):
            r = adain_resblock1(
                x, s, t, f"{g}.resblocks.{i * nk + j}",
                ist["resblock_kernel_sizes"][j], ist["resblock_dilation_sizes"][j],
            )
            acc = r if acc is None else acc + r
        x = acc / nk
        TRACE[f"x{i}"] = x
    x = leaky(x, 0.01)
    x = conv1d(x, wn(t, g + ".conv_post"), t[g + ".conv_post.bias"], pad=3)
    half = n_fft // 2 + 1
    mag = np.exp(x[:half])
    phs = np.sin(x[half:])
    return istft(mag, phs, n_fft, hop)


def decoder(asr, f0, n, s, t, cfg, noise):
    d = "decoder"
    F0 = conv1d(f0[None, :], wn(t, d + ".F0_conv"), t[d + ".F0_conv.bias"], stride=2, pad=1)
    N = conv1d(n[None, :], wn(t, d + ".N_conv"), t[d + ".N_conv.bias"], stride=2, pad=1)
    x = np.concatenate([asr, F0, N], axis=0)
    x = adain_resblk(x, s, t, d + ".encode")
    asr_res = conv1d(asr, wn(t, d + ".asr_res.0"), t[d + ".asr_res.0.bias"])
    res = True
    for i in range(4):
        if res:
            x = np.concatenate([x, asr_res, F0, N], axis=0)
        up = i == 3
        x = adain_resblk(x, s, t, f"{d}.decode.{i}", upsample=up)
        if up:
            res = False
    TRACE["dec"] = x
    return generator(x, s, f0, t, cfg, noise)


def synthesize(phonemes, t, pack, cfg, speed=1.0, dump=None):
    vocab = cfg["vocab"]
    ids = [vocab[c] for c in phonemes if c in vocab]
    ref = pack[len(ids) - 1].reshape(-1)  # [256]
    s_dec, s_pred = ref[:128], ref[128:]
    tok = np.array([0] + ids + [0])
    bert = albert(tok, t)
    d_en = linear(bert, t, "bert_encoder")  # [T, 512]
    d = duration_encoder(d_en, s_pred, t)  # [T, 640]
    x = lstm(d, t, "predictor.lstm")
    dur = sigmoid(linear(x, t, "predictor.duration_proj.linear_layer")).sum(-1) / speed
    pred = np.maximum(np.round(dur), 1).astype(np.int64)
    idx = np.repeat(np.arange(len(tok)), pred)
    en = d[idx].T  # [640, F]
    sh = lstm(en.T, t, "predictor.shared").T  # [512, F]

    def branch(name):
        y = sh
        for i in range(3):
            y = adain_resblk(y, s_pred, t, f"predictor.{name}.{i}", upsample=(i == 1))
        return conv1d(y, t[f"predictor.{name}_proj.weight"], t[f"predictor.{name}_proj.bias"])[0]

    f0 = branch("F0")
    n = branch("N")
    TRACE["f0"], TRACE["n"], TRACE["d"] = f0, n, d
    t_en = text_encoder(tok, t)  # [512, T]
    asr = t_en[:, idx]
    audio = decoder(asr, f0, n, s_dec, t, cfg, Noise())
    if dump is not None:
        dump["durations"] = pred
        dump["f0"] = f0
    return audio, pred


def write_wav(path, audio, rate=SR):
    pcm = np.clip(audio * 32767, -32768, 32767).astype("<i2")
    with wave.open(path, "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(rate)
        w.writeframes(pcm.tobytes())


def main():
    model, voice, config, phonemes, out = sys.argv[1:6]
    t = load_safetensors(model)
    pack = list(load_safetensors(voice).values())[0]
    cfg = json.load(open(config, encoding="utf-8"))
    dump = {}
    audio, dur = synthesize(phonemes, t, pack, cfg, dump=dump)
    write_wav(out, audio)
    print("durations", " ".join(str(int(v)) for v in dur))
    print(f"samples {len(audio)} ({len(audio) / SR:.2f} s), peak {np.abs(audio).max():.3f}")
    if "--dump" in sys.argv:
        dst = sys.argv[sys.argv.index("--dump") + 1]
        np.asarray(audio, dtype="<f4").tofile(dst)
        for k, v in TRACE.items():
            np.asarray(v, dtype="<f4").tofile(dst + "." + k)


if __name__ == "__main__":
    main()
