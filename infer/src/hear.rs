//! Hearing: Moonshine speech to text (docs/hearing-design.md). Reads 16 kHz
//! mono audio and writes text, greedily, from the GGUF file `cargo xtask
//! hear-gguf` makes. The same steps as `tools/reference/moonshine_ref.py`.
//!
//! - Encoder: three convolutions (kernel 127 stride 64, tanh, one-group
//!   norm; kernel 7 stride 3, GELU; kernel 3 stride 2, GELU), then
//!   transformer layers with partial rotary positions.
//! - Decoder: causal self-attention with a cache, attention over the
//!   encoder's frames, a gated SiLU MLP, and the embedding as output layer.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::gguf::{Gguf, TensorInfo, Value};
use crate::quant::{self, TYPE_F32};

const EPS: f32 = 1e-5;
/// Tokens per second of audio Moonshine writes at most (its own limit),
/// plus one.
const TOKENS_PER_SECOND: f32 = 6.5;
pub const RATE: usize = 16_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HearError {
    NotMoonshine,
    MissingKey(&'static str),
    MissingTensor,
    BadShape,
    /// Less than the first convolution's kernel (127 samples, 8 ms).
    TooShort,
}

impl HearError {
    pub fn name(self) -> &'static str {
        match self {
            HearError::NotMoonshine => "not a Moonshine model",
            HearError::MissingKey(_) => "a setting is missing",
            HearError::MissingTensor => "a tensor is missing",
            HearError::BadShape => "a tensor has the wrong shape",
            HearError::TooShort => "the audio is too short",
        }
    }
}

struct EncLayer<'a> {
    ln1: Vec<f32>,
    q: TensorInfo<'a>,
    k: TensorInfo<'a>,
    v: TensorInfo<'a>,
    o: TensorInfo<'a>,
    ln2: Vec<f32>,
    fc1: TensorInfo<'a>,
    fc1_b: Vec<f32>,
    fc2: TensorInfo<'a>,
    fc2_b: Vec<f32>,
}

struct DecLayer<'a> {
    ln1: Vec<f32>,
    q: TensorInfo<'a>,
    k: TensorInfo<'a>,
    v: TensorInfo<'a>,
    o: TensorInfo<'a>,
    ln2: Vec<f32>,
    xq: TensorInfo<'a>,
    xk: TensorInfo<'a>,
    xv: TensorInfo<'a>,
    xo: TensorInfo<'a>,
    ln3: Vec<f32>,
    fc1: TensorInfo<'a>,
    fc1_b: Vec<f32>,
    fc2: TensorInfo<'a>,
    fc2_b: Vec<f32>,
}

/// Moonshine, over a GGUF file in memory.
pub struct Hearing<'a> {
    pub dim: usize,
    heads: usize,
    head_dim: usize,
    /// One frequency per rotated pair of each head's first dimensions.
    inv_freq: Vec<f32>,
    bos: u32,
    eos: u32,
    max_tokens: usize,
    conv1: TensorInfo<'a>,
    gn_w: Vec<f32>,
    gn_b: Vec<f32>,
    conv2: TensorInfo<'a>,
    conv2_b: Vec<f32>,
    conv3: TensorInfo<'a>,
    conv3_b: Vec<f32>,
    enc: Vec<EncLayer<'a>>,
    enc_norm: Vec<f32>,
    dec: Vec<DecLayer<'a>>,
    dec_norm: Vec<f32>,
    embed: TensorInfo<'a>,
    tokens: Vec<&'a str>,
}

fn floats(t: &TensorInfo) -> Vec<f32> {
    t.data
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect()
}

/// `outs[t * rows + r] = W[r] · xs[t]` (+ `bias[r]`) for `n` vectors.
fn matmul(w: &TensorInfo, bias: Option<&[f32]>, xs: &[f32], n: usize) -> Vec<f32> {
    let (rows, cols) = (w.rows, w.cols);
    let mut out = vec![0f32; rows * n];
    crate::par::fill(
        &mut out,
        rows,
        n,
        w.row_bytes() * rows * n.min(4),
        &|r, t| {
            quant::dot(w.ty, w.row(r), &xs[t * cols..(t + 1) * cols]) + bias.map_or(0.0, |b| b[r])
        },
    );
    out
}

/// Layer norm without bias over each `dim`-long vector.
fn layer_norm(x: &mut [f32], dim: usize, w: &[f32]) {
    for v in x.chunks_exact_mut(dim) {
        let m = v.iter().sum::<f32>() / dim as f32;
        let var = v.iter().map(|a| (a - m) * (a - m)).sum::<f32>() / dim as f32;
        let s = 1.0 / libm::sqrtf(var + EPS);
        for (a, g) in v.iter_mut().zip(w) {
            *a = (*a - m) * s * g;
        }
    }
}

fn gelu(x: f32) -> f32 {
    0.5 * x * (1.0 + libm::erff(x * core::f32::consts::FRAC_1_SQRT_2))
}

fn silu(x: f32) -> f32 {
    x / (1.0 + libm::expf(-x))
}

/// A convolution as a matrix product: `x` is `[c_in][t_in]`, `w` the
/// matrix `[c_out][c_in * k]`; the result is `[t_out][c_out]`.
fn conv(
    x: &[f32],
    c_in: usize,
    k: usize,
    stride: usize,
    w: &TensorInfo,
    b: Option<&[f32]>,
) -> (Vec<f32>, usize) {
    let t_in = x.len() / c_in;
    let t_out = (t_in - k) / stride + 1;
    let mut cols = vec![0f32; t_out * c_in * k];
    for t in 0..t_out {
        let row = &mut cols[t * c_in * k..(t + 1) * c_in * k];
        for c in 0..c_in {
            row[c * k..(c + 1) * k]
                .copy_from_slice(&x[c * t_in + t * stride..c * t_in + t * stride + k]);
        }
    }
    (matmul(w, b, &cols, t_out), t_out)
}

/// `[t][c]` to `[c][t]`.
fn transpose(x: &[f32], t: usize, c: usize) -> Vec<f32> {
    let mut o = vec![0f32; x.len()];
    for i in 0..t {
        for j in 0..c {
            o[j * t + i] = x[i * c + j];
        }
    }
    o
}

impl<'a> Hearing<'a> {
    pub fn from_gguf(g: &Gguf<'a>) -> Result<Hearing<'a>, HearError> {
        if !matches!(g.get("general.architecture"), Some(Value::Str("moonshine"))) {
            return Err(HearError::NotMoonshine);
        }
        let u = |k: &'static str| {
            g.u32(k)
                .map(|v| v as usize)
                .map_err(|_| HearError::MissingKey(k))
        };
        let dim = u("moonshine.hidden_size")?;
        let heads = u("moonshine.encoder_num_attention_heads")?;
        if u("moonshine.decoder_num_attention_heads")? != heads || dim % heads != 0 {
            return Err(HearError::BadShape);
        }
        let head_dim = dim / heads;
        let factor = g
            .f32("moonshine.partial_rotary_factor")
            .map_err(|_| HearError::MissingKey("moonshine.partial_rotary_factor"))?;
        let theta = g
            .f32("moonshine.rope_theta")
            .map_err(|_| HearError::MissingKey("moonshine.rope_theta"))?;
        let rot = {
            let r = (head_dim as f32 * factor) as usize;
            r - r % 2
        };
        let inv_freq = (0..rot / 2)
            .map(|i| 1.0 / libm::powf(theta, (2 * i) as f32 / rot as f32))
            .collect();
        let vocab = u("moonshine.vocab_size")?;
        let tensor = |name: &str, rows: usize, cols: usize| -> Result<TensorInfo<'a>, HearError> {
            let t = *g.tensor(name).ok_or(HearError::MissingTensor)?;
            if t.ty != TYPE_F32 || t.rows != rows || t.cols != cols {
                return Err(HearError::BadShape);
            }
            Ok(t)
        };
        let vector = |name: &str, n: usize| tensor(name, 1, n).map(|t| floats(&t));
        let ff = u("moonshine.intermediate_size")?;
        let mut enc = Vec::new();
        for i in 0..u("moonshine.encoder_num_hidden_layers")? {
            let p = |s: &str| alloc::format!("encoder.layers.{i}.{s}");
            enc.push(EncLayer {
                ln1: vector(&p("input_layernorm.weight"), dim)?,
                q: tensor(&p("self_attn.q_proj.weight"), dim, dim)?,
                k: tensor(&p("self_attn.k_proj.weight"), dim, dim)?,
                v: tensor(&p("self_attn.v_proj.weight"), dim, dim)?,
                o: tensor(&p("self_attn.o_proj.weight"), dim, dim)?,
                ln2: vector(&p("post_attention_layernorm.weight"), dim)?,
                fc1: tensor(&p("mlp.fc1.weight"), ff, dim)?,
                fc1_b: vector(&p("mlp.fc1.bias"), ff)?,
                fc2: tensor(&p("mlp.fc2.weight"), dim, ff)?,
                fc2_b: vector(&p("mlp.fc2.bias"), dim)?,
            });
        }
        let mut dec = Vec::new();
        for i in 0..u("moonshine.decoder_num_hidden_layers")? {
            let p = |s: &str| alloc::format!("decoder.layers.{i}.{s}");
            dec.push(DecLayer {
                ln1: vector(&p("input_layernorm.weight"), dim)?,
                q: tensor(&p("self_attn.q_proj.weight"), dim, dim)?,
                k: tensor(&p("self_attn.k_proj.weight"), dim, dim)?,
                v: tensor(&p("self_attn.v_proj.weight"), dim, dim)?,
                o: tensor(&p("self_attn.o_proj.weight"), dim, dim)?,
                ln2: vector(&p("post_attention_layernorm.weight"), dim)?,
                xq: tensor(&p("encoder_attn.q_proj.weight"), dim, dim)?,
                xk: tensor(&p("encoder_attn.k_proj.weight"), dim, dim)?,
                xv: tensor(&p("encoder_attn.v_proj.weight"), dim, dim)?,
                xo: tensor(&p("encoder_attn.o_proj.weight"), dim, dim)?,
                ln3: vector(&p("final_layernorm.weight"), dim)?,
                fc1: tensor(&p("mlp.fc1.weight"), 2 * ff, dim)?,
                fc1_b: vector(&p("mlp.fc1.bias"), 2 * ff)?,
                fc2: tensor(&p("mlp.fc2.weight"), dim, ff)?,
                fc2_b: vector(&p("mlp.fc2.bias"), dim)?,
            });
        }
        let tokens = g
            .strings("tokenizer.ggml.tokens")
            .map_err(|_| HearError::MissingKey("tokenizer.ggml.tokens"))?;
        if tokens.len() != vocab {
            return Err(HearError::BadShape);
        }
        Ok(Hearing {
            dim,
            heads,
            head_dim,
            inv_freq,
            bos: u("moonshine.decoder_start_token_id")? as u32,
            eos: u("moonshine.eos_token_id")? as u32,
            max_tokens: u("moonshine.max_position_embeddings")?,
            conv1: tensor("encoder.conv1.weight", dim, 127)?,
            gn_w: vector("encoder.groupnorm.weight", dim)?,
            gn_b: vector("encoder.groupnorm.bias", dim)?,
            conv2: tensor("encoder.conv2.weight", 2 * dim, dim * 7)?,
            conv2_b: vector("encoder.conv2.bias", 2 * dim)?,
            conv3: tensor("encoder.conv3.weight", dim, 2 * dim * 3)?,
            conv3_b: vector("encoder.conv3.bias", dim)?,
            enc,
            enc_norm: vector("encoder.layer_norm.weight", dim)?,
            dec,
            dec_norm: vector("decoder.norm.weight", dim)?,
            embed: tensor("decoder.embed_tokens.weight", vocab, dim)?,
            tokens,
        })
    }

    /// Rotates the first `rot` dimensions of each head of every vector in
    /// `x` (`[t][dim]`), in pairs (0, 1), (2, 3), ..., at positions
    /// `pos0`, `pos0 + 1`, ...
    fn rope(&self, x: &mut [f32], pos0: usize) {
        for (t, v) in x.chunks_exact_mut(self.dim).enumerate() {
            let pos = (pos0 + t) as f32;
            for h in v.chunks_exact_mut(self.head_dim) {
                for (i, f) in self.inv_freq.iter().enumerate() {
                    let (s, c) = libm::sincosf(pos * f);
                    let (a, b) = (h[2 * i], h[2 * i + 1]);
                    h[2 * i] = a * c - b * s;
                    h[2 * i + 1] = b * c + a * s;
                }
            }
        }
    }

    /// Attention of `tq` queries over `tk` keys and values (all `[t][dim]`);
    /// with `causal`, query `i` sees keys `0..=tk - tq + i`.
    fn attend(
        &self,
        q: &[f32],
        k: &[f32],
        v: &[f32],
        tq: usize,
        tk: usize,
        causal: bool,
    ) -> Vec<f32> {
        let (d, hd) = (self.dim, self.head_dim);
        let scale = 1.0 / libm::sqrtf(hd as f32);
        let mut out = vec![0f32; tq * d];
        let mut s = vec![0f32; tk];
        for h in 0..self.heads {
            for i in 0..tq {
                let qi = &q[i * d + h * hd..i * d + (h + 1) * hd];
                let n = if causal { tk - tq + i + 1 } else { tk };
                let mut max = f32::NEG_INFINITY;
                for (j, sj) in s[..n].iter_mut().enumerate() {
                    let kj = &k[j * d + h * hd..j * d + (h + 1) * hd];
                    *sj = qi.iter().zip(kj).map(|(a, b)| a * b).sum::<f32>() * scale;
                    max = max.max(*sj);
                }
                let mut sum = 0.0;
                for sj in &mut s[..n] {
                    *sj = libm::expf(*sj - max);
                    sum += *sj;
                }
                let o = &mut out[i * d + h * hd..i * d + (h + 1) * hd];
                for (j, sj) in s[..n].iter().enumerate() {
                    let p = sj / sum;
                    for (oi, vj) in o.iter_mut().zip(&v[j * d + h * hd..j * d + (h + 1) * hd]) {
                        *oi += p * vj;
                    }
                }
            }
        }
        out
    }

    /// The encoder's output for `audio` (samples in -1..1 at 16 kHz):
    /// `[frames][dim]`, and the number of frames.
    pub fn encode(&self, audio: &[f32]) -> Result<(Vec<f32>, usize), HearError> {
        let d = self.dim;
        if audio.len() < 127 + 64 * 12 {
            return Err(HearError::TooShort);
        }
        let (mut x, t1) = conv(audio, 1, 127, 64, &self.conv1, None);
        for a in &mut x {
            *a = libm::tanhf(*a);
        }
        // One group: normalised over every channel and time.
        let n = x.len() as f32;
        let m = x.iter().sum::<f32>() / n;
        let var = x.iter().map(|a| (a - m) * (a - m)).sum::<f32>() / n;
        let s = 1.0 / libm::sqrtf(var + EPS);
        for (i, a) in x.iter_mut().enumerate() {
            let c = i % d;
            *a = (*a - m) * s * self.gn_w[c] + self.gn_b[c];
        }
        let (mut x, t2) = conv(
            &transpose(&x, t1, d),
            d,
            7,
            3,
            &self.conv2,
            Some(&self.conv2_b),
        );
        x.iter_mut().for_each(|a| *a = gelu(*a));
        let (mut x, t) = conv(
            &transpose(&x, t2, 2 * d),
            2 * d,
            3,
            2,
            &self.conv3,
            Some(&self.conv3_b),
        );
        x.iter_mut().for_each(|a| *a = gelu(*a));
        for l in &self.enc {
            let mut h = x.clone();
            layer_norm(&mut h, d, &l.ln1);
            let mut q = matmul(&l.q, None, &h, t);
            let mut k = matmul(&l.k, None, &h, t);
            let v = matmul(&l.v, None, &h, t);
            self.rope(&mut q, 0);
            self.rope(&mut k, 0);
            let a = matmul(&l.o, None, &self.attend(&q, &k, &v, t, t, false), t);
            x.iter_mut().zip(&a).for_each(|(x, a)| *x += a);
            let mut h = x.clone();
            layer_norm(&mut h, d, &l.ln2);
            let mut h = matmul(&l.fc1, Some(&l.fc1_b), &h, t);
            h.iter_mut().for_each(|a| *a = gelu(*a));
            let a = matmul(&l.fc2, Some(&l.fc2_b), &h, t);
            x.iter_mut().zip(&a).for_each(|(x, a)| *x += a);
        }
        layer_norm(&mut x, d, &self.enc_norm);
        Ok((x, t))
    }

    /// Greedy decoding over the encoder's output: the token ids (without
    /// the end), and the first step's logits (for comparing).
    pub fn decode(&self, enc: &[f32], frames: usize, max_tokens: usize) -> (Vec<u32>, Vec<f32>) {
        let d = self.dim;
        let ff = self.dec[0].fc2.cols;
        let cross: Vec<(Vec<f32>, Vec<f32>)> = self
            .dec
            .iter()
            .map(|l| {
                (
                    matmul(&l.xk, None, enc, frames),
                    matmul(&l.xv, None, enc, frames),
                )
            })
            .collect();
        let mut cache: Vec<(Vec<f32>, Vec<f32>)> =
            self.dec.iter().map(|_| (Vec::new(), Vec::new())).collect();
        let (mut ids, mut first) = (Vec::new(), Vec::new());
        let mut tok = self.bos;
        for step in 0..max_tokens.min(self.max_tokens) {
            let mut x = floats(&TensorInfo {
                data: self.embed.row(tok as usize),
                ..self.embed
            });
            for (l, ((ck, cv), (xk, xv))) in self.dec.iter().zip(cache.iter_mut().zip(&cross)) {
                let mut h = x.clone();
                layer_norm(&mut h, d, &l.ln1);
                let mut q = matmul(&l.q, None, &h, 1);
                let mut k = matmul(&l.k, None, &h, 1);
                self.rope(&mut q, step);
                self.rope(&mut k, step);
                ck.extend_from_slice(&k);
                cv.extend_from_slice(&matmul(&l.v, None, &h, 1));
                let a = matmul(&l.o, None, &self.attend(&q, ck, cv, 1, step + 1, true), 1);
                x.iter_mut().zip(&a).for_each(|(x, a)| *x += a);
                let mut h = x.clone();
                layer_norm(&mut h, d, &l.ln2);
                let q = matmul(&l.xq, None, &h, 1);
                let a = matmul(&l.xo, None, &self.attend(&q, xk, xv, 1, frames, false), 1);
                x.iter_mut().zip(&a).for_each(|(x, a)| *x += a);
                let mut h = x.clone();
                layer_norm(&mut h, d, &l.ln3);
                let h = matmul(&l.fc1, Some(&l.fc1_b), &h, 1);
                let g: Vec<f32> = (0..ff).map(|i| silu(h[ff + i]) * h[i]).collect();
                let a = matmul(&l.fc2, Some(&l.fc2_b), &g, 1);
                x.iter_mut().zip(&a).for_each(|(x, a)| *x += a);
            }
            layer_norm(&mut x, d, &self.dec_norm);
            let logits = matmul(&self.embed, None, &x, 1);
            let next = crate::argmax(&logits) as u32;
            if step == 0 {
                first = logits;
            }
            if next == self.eos {
                break;
            }
            ids.push(next);
            tok = next;
        }
        (ids, first)
    }

    /// The most tokens worth decoding for `samples` of audio.
    pub fn token_limit(samples: usize) -> usize {
        libm::ceilf(samples as f32 / RATE as f32 * TOKENS_PER_SECOND) as usize + 1
    }

    /// Speech to text: `audio` at 16 kHz in -1..1.
    pub fn transcribe(&self, audio: &[f32]) -> Result<String, HearError> {
        let (enc, frames) = self.encode(audio)?;
        let (ids, _) = self.decode(&enc, frames, Self::token_limit(audio.len()));
        Ok(self.text(&ids))
    }

    /// Token ids as text: `▁` is a space, `<0xNN>` a byte, other `<...>`
    /// tokens are dropped.
    pub fn text(&self, ids: &[u32]) -> String {
        let mut out = Vec::new();
        for &i in ids {
            let s = self.tokens.get(i as usize).copied().unwrap_or("");
            let b = s.as_bytes();
            if b.len() == 6 && s.starts_with("<0x") && s.ends_with('>') {
                if let Ok(v) = u8::from_str_radix(&s[3..5], 16) {
                    out.push(v);
                }
            } else if !(s.starts_with('<') && s.ends_with('>')) {
                out.extend_from_slice(s.replace('\u{2581}', " ").as_bytes());
            }
        }
        String::from(String::from_utf8_lossy(&out).trim())
    }
}

/// 16-bit samples as floats in -1..1.
pub fn samples_from_i16(pcm: &[u8]) -> Vec<f32> {
    pcm.chunks_exact(2)
        .map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0)
        .collect()
}

/// The 16 kHz mono 16-bit samples of a WAV file (the `data` chunk), or
/// `None` for any other kind.
pub fn wav_samples(wav: &[u8]) -> Option<&[u8]> {
    if wav.get(..4)? != b"RIFF" || wav.get(8..12)? != b"WAVE" {
        return None;
    }
    let mut at = 12;
    let mut ok = false;
    while at + 8 <= wav.len() {
        let id = &wav[at..at + 4];
        let len = u32::from_le_bytes(wav[at + 4..at + 8].try_into().ok()?) as usize;
        let body = wav.get(at + 8..(at + 8 + len).min(wav.len()))?;
        if id == b"fmt " {
            let ch = u16::from_le_bytes(body.get(2..4)?.try_into().ok()?);
            let rate = u32::from_le_bytes(body.get(4..8)?.try_into().ok()?);
            let bits = u16::from_le_bytes(body.get(14..16)?.try_into().ok()?);
            ok = ch == 1 && rate == RATE as u32 && bits == 16;
        } else if id == b"data" {
            return ok.then_some(body);
        }
        at += 8 + len + (len & 1);
    }
    None
}
