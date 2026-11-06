//! Kokoro-82M text to speech (docs/speech-design.md): phonemes in, 24 kHz
//! audio out. The model is StyleTTS 2's inference path: an ALBERT text
//! model and a prosody predictor (durations, pitch, energy), then an
//! iSTFTNet decoder driven by a sine source at the predicted pitch.
//!
//! Activations are `[channel][time]`. Weights come prepared by
//! `cargo xtask speak-gguf`: weight norm folded, convolutions as im2col
//! matrices, transposed convolutions as `[out * k][in]`. Checked against
//! the NumPy reference (tools/reference/kokoro_ref.py) by `cargo xtask
//! speak-check`.

use alloc::collections::BTreeMap;
use alloc::vec;
use alloc::vec::Vec;

use crate::gguf::{Gguf, TensorInfo};
use crate::quant::{self, TYPE_F32};

/// Output samples per second.
pub const RATE: usize = 24_000;
/// Audio samples per predicted frame (the decoder's upsampling, 2 x 300).
pub const SAMPLES_PER_FRAME: usize = 600;
/// im2col columns per product, so a long utterance needs bounded memory.
const CHUNK: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpeakError {
    NotKokoro,
    MissingTensor,
    BadShape,
    /// No phoneme in the text was in the model's alphabet.
    Empty,
    /// More phonemes than the voice has styles for (510).
    TooLong,
}

impl SpeakError {
    pub fn name(self) -> &'static str {
        match self {
            SpeakError::NotKokoro => "not a Kokoro model",
            SpeakError::MissingTensor => "a tensor is missing",
            SpeakError::BadShape => "a tensor has the wrong shape",
            SpeakError::Empty => "nothing to say",
            SpeakError::TooLong => "too much to say at once",
        }
    }
}

/// The random numbers Kokoro's sine source draws: xorshift64* and
/// Box-Muller, the same sequence as the reference's `Noise`.
struct Noise(u64);

impl Noise {
    fn u32(&mut self) -> u32 {
        let mut s = self.0;
        s ^= s >> 12;
        s ^= s << 25;
        s ^= s >> 27;
        self.0 = s;
        (s.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 32) as u32
    }

    fn uniform(&mut self) -> f64 {
        (f64::from(self.u32()) + 0.5) / 4_294_967_296.0
    }

    fn normals(&mut self, n: usize) -> Vec<f64> {
        let mut out = vec![0f64; n];
        let mut i = 0;
        while i < n {
            let (u1, u2) = (self.uniform(), self.uniform());
            let r = libm::sqrt(-2.0 * libm::log(u1));
            let a = 2.0 * core::f64::consts::PI * u2;
            out[i] = r * libm::cos(a);
            if i + 1 < n {
                out[i + 1] = r * libm::sin(a);
            }
            i += 2;
        }
        out
    }
}

/// Called with an intermediate value's name and contents.
pub type Trace = dyn Fn(&str, &[f32]) + Sync;

pub struct Speech<'a> {
    t: BTreeMap<&'a str, TensorInfo<'a>>,
    /// Gets intermediate values by name, for `speak-check` to compare.
    pub trace: Option<&'a Trace>,
    /// Phoneme to token id.
    alphabet: Vec<(char, u32)>,
}

fn floats(t: &TensorInfo) -> Vec<f32> {
    t.data
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_le_bytes(*b))
        .collect()
}

/// `outs[t * rows + r] = W[r] · xs[t]` (+ `bias[r]`) for `n` vectors.
/// With AVX2, in tiles of four rows by two vectors (`avx2::dot_f32_4x2`).
fn matmul(w: &TensorInfo, bias: Option<&[f32]>, xs: &[f32], n: usize) -> Vec<f32> {
    let (rows, cols) = (w.rows, w.cols);
    let mut out = vec![0f32; rows * n];
    if w.ty == TYPE_F32 && rows >= 4 && n >= 2 && crate::cpu::avx2() {
        crate::par::fill_rows(
            &mut out,
            rows,
            n,
            w.row_bytes() * rows * n.min(4),
            &|a, b, put| {
                let x = |t: usize| &xs[t * cols..(t + 1) * cols];
                let b0 = |r: usize| bias.map_or(0.0, |b| b[r]);
                let mut r = a;
                while r < b {
                    if r + 4 <= b {
                        let ws = [w.row(r), w.row(r + 1), w.row(r + 2), w.row(r + 3)];
                        let mut t = 0;
                        while t + 2 <= n {
                            #[allow(unsafe_code)]
                            // SAFETY: `avx2()` is true only when CPUID reports
                            // AVX2 and FMA and the OS saves the YMM registers;
                            // each row has `cols` floats and each vector is
                            // `cols` long.
                            let v = unsafe { crate::avx2::dot_f32_4x2(ws, [x(t), x(t + 1)]) };
                            for i in 0..4 {
                                put(r + i, t, v[2 * i] + b0(r + i));
                                put(r + i, t + 1, v[2 * i + 1] + b0(r + i));
                            }
                            t += 2;
                        }
                        if t < n {
                            for i in 0..4 {
                                put(r + i, t, quant::dot(w.ty, ws[i], x(t)) + b0(r + i));
                            }
                        }
                        r += 4;
                    } else {
                        for t in 0..n {
                            put(r, t, quant::dot(w.ty, w.row(r), x(t)) + b0(r));
                        }
                        r += 1;
                    }
                }
            },
        );
        return out;
    }
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

fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + libm::expf(-x))
}

fn leaky(x: &mut [f32], a: f32) {
    for v in x {
        if *v < 0.0 {
            *v *= a;
        }
    }
}

/// Layer norm over each `dim`-long vector, with optional gain and bias.
fn layer_norm(x: &mut [f32], dim: usize, g: Option<(&[f32], &[f32])>, eps: f32) {
    for v in x.chunks_exact_mut(dim) {
        let m = v.iter().sum::<f32>() / dim as f32;
        let var = v.iter().map(|a| (a - m) * (a - m)).sum::<f32>() / dim as f32;
        let s = 1.0 / libm::sqrtf(var + eps);
        for (i, a) in v.iter_mut().enumerate() {
            *a = (*a - m) * s;
            if let Some((w, b)) = g {
                *a = *a * w[i] + b[i];
            }
        }
    }
}

/// `[a][b]` to `[b][a]`.
fn transpose(x: &[f32], a: usize, b: usize) -> Vec<f32> {
    let mut o = vec![0f32; x.len()];
    for i in 0..a {
        for j in 0..b {
            o[j * a + i] = x[i * b + j];
        }
    }
    o
}

fn concat(parts: &[&[f32]]) -> Vec<f32> {
    let mut o = Vec::with_capacity(parts.iter().map(|p| p.len()).sum());
    for p in parts {
        o.extend_from_slice(p);
    }
    o
}

/// Linear interpolation along time, as `F.interpolate(mode="linear",
/// align_corners=False)` with `scale`: `x` is `[n][dim]`.
fn interp(x: &[f64], dim: usize, scale: f64, out_len: usize) -> Vec<f64> {
    let n = x.len() / dim;
    let mut o = vec![0f64; out_len * dim];
    for i in 0..out_len {
        let src = ((i as f64 + 0.5) / scale - 0.5).max(0.0);
        let i0 = (libm::floor(src) as usize).min(n - 1);
        let i1 = (i0 + 1).min(n - 1);
        let w = src - i0 as f64;
        for d in 0..dim {
            o[i * dim + d] = x[i0 * dim + d] * (1.0 - w) + x[i1 * dim + d] * w;
        }
    }
    o
}

fn hann(n: usize) -> Vec<f64> {
    (0..n)
        .map(|i| 0.5 - 0.5 * libm::cos(2.0 * core::f64::consts::PI * i as f64 / n as f64))
        .collect()
}

impl<'a> Speech<'a> {
    pub fn from_gguf(g: &Gguf<'a>) -> Result<Speech<'a>, SpeakError> {
        if g.str("general.architecture") != Ok("kokoro") {
            return Err(SpeakError::NotKokoro);
        }
        let mut t = BTreeMap::new();
        for ti in g.tensors() {
            if ti.ty != TYPE_F32 {
                return Err(SpeakError::BadShape);
            }
            t.insert(ti.name, *ti);
        }
        let tokens = g
            .strings("tokenizer.ggml.tokens")
            .map_err(|_| SpeakError::NotKokoro)?;
        let mut alphabet = Vec::new();
        for (id, s) in tokens.iter().enumerate() {
            let mut cs = s.chars();
            if let (Some(c), None) = (cs.next(), cs.next()) {
                alphabet.push((c, id as u32));
            }
        }
        let s = Speech {
            t,
            alphabet,
            trace: None,
        };
        for name in [
            "voice",
            "bert_encoder.weight",
            "decoder.generator.conv_post.weight",
        ] {
            s.get(name)?;
        }
        Ok(s)
    }

    fn tr(&self, name: &str, v: &[f32]) {
        if let Some(t) = self.trace {
            t(name, v);
        }
    }

    fn get(&self, name: &str) -> Result<&TensorInfo<'a>, SpeakError> {
        self.t.get(name).ok_or(SpeakError::MissingTensor)
    }

    fn vals(&self, name: &str) -> Result<Vec<f32>, SpeakError> {
        Ok(floats(self.get(name)?))
    }

    /// Token ids for the phonemes the model knows; others are dropped.
    pub fn ids(&self, phonemes: &str) -> Vec<u32> {
        phonemes
            .chars()
            .filter_map(|c| self.alphabet.iter().find(|(a, _)| *a == c).map(|(_, i)| *i))
            .collect()
    }

    fn linear(&self, p: &str, x: &[f32], n: usize) -> Result<Vec<f32>, SpeakError> {
        let w = self.get(&alloc::format!("{p}.weight"))?;
        let b = self.vals(&alloc::format!("{p}.bias"))?;
        if x.len() != n * w.cols {
            return Err(SpeakError::BadShape);
        }
        Ok(matmul(w, Some(&b), x, n))
    }

    /// A convolution: `x` `[c][t]`, weight `[o][c * k]`; `[o][t_out]`.
    #[allow(clippy::too_many_arguments)]
    fn conv(
        &self,
        x: &[f32],
        c: usize,
        w: &str,
        b: Option<&str>,
        stride: usize,
        pad: usize,
        dil: usize,
    ) -> Result<Vec<f32>, SpeakError> {
        let w = self.get(w)?;
        let bias = match b {
            Some(b) => Some(self.vals(b)?),
            None => None,
        };
        let t = x.len() / c;
        if w.cols % c != 0 {
            return Err(SpeakError::BadShape);
        }
        let k = w.cols / c;
        let span = dil * (k - 1) + 1;
        if t + 2 * pad < span {
            return Err(SpeakError::BadShape);
        }
        let t_out = (t + 2 * pad - span) / stride + 1;
        let o = w.rows;
        let mut out = vec![0f32; o * t_out];
        let mut start = 0;
        while start < t_out {
            let n = CHUNK.min(t_out - start);
            let mut cols = vec![0f32; n * c * k];
            for j in 0..n {
                let row = &mut cols[j * c * k..(j + 1) * c * k];
                let base = (start + j) * stride;
                for ci in 0..c {
                    for kk in 0..k {
                        let at = (base + kk * dil) as isize - pad as isize;
                        if at >= 0 && (at as usize) < t {
                            row[ci * k + kk] = x[ci * t + at as usize];
                        }
                    }
                }
            }
            let y = matmul(w, bias.as_deref(), &cols, n);
            for j in 0..n {
                for oi in 0..o {
                    out[oi * t_out + start + j] = y[j * o + oi];
                }
            }
            start += n;
        }
        Ok(out)
    }

    /// A transposed convolution, weight `[o * k][c]`: `[o][t_out]`.
    #[allow(clippy::too_many_arguments)]
    fn conv_t(
        &self,
        x: &[f32],
        c: usize,
        p: &str,
        o: usize,
        stride: usize,
        pad: usize,
    ) -> Result<Vec<f32>, SpeakError> {
        let w = self.get(&alloc::format!("{p}.weight"))?;
        let b = self.vals(&alloc::format!("{p}.bias"))?;
        let t = x.len() / c;
        if w.cols != c || w.rows % o != 0 {
            return Err(SpeakError::BadShape);
        }
        let k = w.rows / o;
        let xt = transpose(x, c, t);
        let m = matmul(w, None, &xt, t);
        let full = (t - 1) * stride + k;
        let t_out = full - 2 * pad;
        let mut y = vec![0f32; o * full];
        for ti in 0..t {
            for oi in 0..o {
                for j in 0..k {
                    y[oi * full + ti * stride + j] += m[ti * o * k + oi * k + j];
                }
            }
        }
        let mut out = vec![0f32; o * t_out];
        for oi in 0..o {
            for j in 0..t_out {
                out[oi * t_out + j] = y[oi * full + pad + j] + b[oi];
            }
        }
        Ok(out)
    }

    /// A bidirectional LSTM of one layer: `x` `[t][i]` -> `[t][2h]`.
    fn lstm(&self, p: &str, x: &[f32], t: usize) -> Result<Vec<f32>, SpeakError> {
        let mut dirs = Vec::new();
        for sfx in ["", "_reverse"] {
            let wi = self.get(&alloc::format!("{p}.weight_ih_l0{sfx}"))?;
            let wh = self.get(&alloc::format!("{p}.weight_hh_l0{sfx}"))?;
            let b = self.vals(&alloc::format!("{p}.bias_l0{sfx}"))?;
            let h4 = wi.rows;
            let hs = h4 / 4;
            if x.len() != t * wi.cols || wh.cols != hs {
                return Err(SpeakError::BadShape);
            }
            let xi = matmul(wi, Some(&b), x, t);
            let mut h = vec![0f32; hs];
            let mut cst = vec![0f32; hs];
            let mut out = vec![0f32; t * hs];
            for step in 0..t {
                let k = if sfx.is_empty() { step } else { t - 1 - step };
                let z = matmul(wh, None, &h, 1);
                let zi = &xi[k * h4..(k + 1) * h4];
                for j in 0..hs {
                    let i = sigmoid(zi[j] + z[j]);
                    let f = sigmoid(zi[hs + j] + z[hs + j]);
                    let g = libm::tanhf(zi[2 * hs + j] + z[2 * hs + j]);
                    let o = sigmoid(zi[3 * hs + j] + z[3 * hs + j]);
                    cst[j] = f * cst[j] + i * g;
                    h[j] = o * libm::tanhf(cst[j]);
                }
                out[k * hs..(k + 1) * hs].copy_from_slice(&h);
            }
            dirs.push((out, hs));
        }
        let hs = dirs[0].1;
        let mut out = vec![0f32; t * 2 * hs];
        for k in 0..t {
            out[k * 2 * hs..k * 2 * hs + hs].copy_from_slice(&dirs[0].0[k * hs..(k + 1) * hs]);
            out[k * 2 * hs + hs..(k + 1) * 2 * hs]
                .copy_from_slice(&dirs[1].0[k * hs..(k + 1) * hs]);
        }
        Ok(out)
    }

    /// AdaIN1d: instance norm over time, then `(1 + gamma) x + beta`.
    fn adain(&self, p: &str, x: &mut [f32], c: usize, s: &[f32]) -> Result<(), SpeakError> {
        let h = self.linear(&alloc::format!("{p}.fc"), s, 1)?;
        if h.len() != 2 * c {
            return Err(SpeakError::BadShape);
        }
        let t = x.len() / c;
        for ci in 0..c {
            let v = &mut x[ci * t..(ci + 1) * t];
            let m = v.iter().sum::<f32>() / t as f32;
            let var = v.iter().map(|a| (a - m) * (a - m)).sum::<f32>() / t as f32;
            let inv = 1.0 / libm::sqrtf(var + 1e-5);
            let (g, b) = (1.0 + h[ci], h[c + ci]);
            for a in v.iter_mut() {
                *a = g * (*a - m) * inv + b;
            }
        }
        Ok(())
    }

    /// AdainResBlk1d: `x` `[c][t]`; with `up`, twice as long.
    fn resblk(
        &self,
        p: &str,
        x: &[f32],
        c: usize,
        s: &[f32],
        up: bool,
    ) -> Result<(Vec<f32>, usize), SpeakError> {
        let t = x.len() / c;
        let mut r = x.to_vec();
        self.adain(&alloc::format!("{p}.norm1"), &mut r, c, s)?;
        leaky(&mut r, 0.2);
        if up {
            // Depthwise transposed convolution, kernel 3, stride 2,
            // padding 1, output padding 1: length 2t.
            let w = self.vals(&alloc::format!("{p}.pool.weight"))?;
            let b = self.vals(&alloc::format!("{p}.pool.bias"))?;
            let k = w.len() / c;
            let mut y = vec![0f32; c * 2 * t];
            for ci in 0..c {
                for ti in 0..t {
                    for j in 0..k {
                        // Output index before cropping the padding of 1.
                        let at = (ti * 2 + j) as isize - 1;
                        if at >= 0 && (at as usize) < 2 * t {
                            y[ci * 2 * t + at as usize] += r[ci * t + ti] * w[ci * k + j];
                        }
                    }
                }
                for v in &mut y[ci * 2 * t..(ci + 1) * 2 * t] {
                    *v += b[ci];
                }
            }
            r = y;
        }
        let w1 = alloc::format!("{p}.conv1.weight");
        let b1 = alloc::format!("{p}.conv1.bias");
        let mut r = self.conv(&r, c, &w1, Some(&b1), 1, 1, 1)?;
        let o = self.get(&w1)?.rows;
        self.adain(&alloc::format!("{p}.norm2"), &mut r, o, s)?;
        leaky(&mut r, 0.2);
        let w2 = alloc::format!("{p}.conv2.weight");
        let b2 = alloc::format!("{p}.conv2.bias");
        let r = self.conv(&r, o, &w2, Some(&b2), 1, 1, 1)?;
        let mut sc = if up {
            let mut u = vec![0f32; c * 2 * t];
            for ci in 0..c {
                for ti in 0..t {
                    let v = x[ci * t + ti];
                    u[ci * 2 * t + 2 * ti] = v;
                    u[ci * 2 * t + 2 * ti + 1] = v;
                }
            }
            u
        } else {
            x.to_vec()
        };
        let w11 = alloc::format!("{p}.conv1x1.weight");
        if self.t.contains_key(w11.as_str()) {
            sc = self.conv(&sc, c, &w11, None, 1, 0, 1)?;
        }
        if sc.len() != r.len() {
            return Err(SpeakError::BadShape);
        }
        let k = core::f32::consts::FRAC_1_SQRT_2;
        Ok((r.iter().zip(&sc).map(|(a, b)| (a + b) * k).collect(), o))
    }

    /// AdaINResBlock1, with Snake activations.
    fn resblock1(
        &self,
        p: &str,
        x: &mut [f32],
        c: usize,
        s: &[f32],
        k: usize,
        dil: [usize; 3],
    ) -> Result<(), SpeakError> {
        let t = x.len() / c;
        for (i, d) in dil.iter().enumerate() {
            let a1 = self.vals(&alloc::format!("{p}.alpha1.{i}"))?;
            let a2 = self.vals(&alloc::format!("{p}.alpha2.{i}"))?;
            let mut xt = x.to_vec();
            self.adain(&alloc::format!("{p}.adain1.{i}"), &mut xt, c, s)?;
            snake(&mut xt, &a1, t);
            let xt = self.conv(
                &xt,
                c,
                &alloc::format!("{p}.convs1.{i}.weight"),
                Some(&alloc::format!("{p}.convs1.{i}.bias")),
                1,
                (k * d - d) / 2,
                *d,
            )?;
            let mut xt = xt;
            self.adain(&alloc::format!("{p}.adain2.{i}"), &mut xt, c, s)?;
            snake(&mut xt, &a2, t);
            let xt = self.conv(
                &xt,
                c,
                &alloc::format!("{p}.convs2.{i}.weight"),
                Some(&alloc::format!("{p}.convs2.{i}.bias")),
                1,
                (k - 1) / 2,
                1,
            )?;
            for (a, b) in x.iter_mut().zip(&xt) {
                *a += b;
            }
        }
        Ok(())
    }

    fn albert(&self, ids: &[u32]) -> Result<Vec<f32>, SpeakError> {
        let n = ids.len();
        let e = 128;
        let word = self.get("bert.embeddings.word_embeddings.weight")?;
        let pos = self.vals("bert.embeddings.position_embeddings.weight")?;
        let typ = self.vals("bert.embeddings.token_type_embeddings.weight")?;
        if n > pos.len() / e {
            return Err(SpeakError::TooLong);
        }
        let mut x = vec![0f32; n * e];
        for (i, &id) in ids.iter().enumerate() {
            let w = floats(&TensorInfo {
                data: word.row(id as usize),
                cols: e,
                rows: 1,
                ..*word
            });
            for j in 0..e {
                x[i * e + j] = w[j] + pos[i * e + j] + typ[j];
            }
        }
        let g = self.vals("bert.embeddings.LayerNorm.weight")?;
        let b = self.vals("bert.embeddings.LayerNorm.bias")?;
        layer_norm(&mut x, e, Some((&g, &b)), 1e-12);
        let mut x = self.linear("bert.encoder.embedding_hidden_mapping_in", &x, n)?;
        let l = "bert.encoder.albert_layer_groups.0.albert_layers.0";
        let (hid, heads) = (768, 12);
        let hd = hid / heads;
        let ag = self.vals(&alloc::format!("{l}.attention.LayerNorm.weight"))?;
        let ab = self.vals(&alloc::format!("{l}.attention.LayerNorm.bias"))?;
        let fg = self.vals(&alloc::format!("{l}.full_layer_layer_norm.weight"))?;
        let fb = self.vals(&alloc::format!("{l}.full_layer_layer_norm.bias"))?;
        for _ in 0..12 {
            let q = self.linear(&alloc::format!("{l}.attention.query"), &x, n)?;
            let k = self.linear(&alloc::format!("{l}.attention.key"), &x, n)?;
            let v = self.linear(&alloc::format!("{l}.attention.value"), &x, n)?;
            let mut ctx = vec![0f32; n * hid];
            let scale = 1.0 / libm::sqrtf(hd as f32);
            let mut sc = vec![0f32; n];
            for h in 0..heads {
                for i in 0..n {
                    let qi = &q[i * hid + h * hd..i * hid + (h + 1) * hd];
                    let mut mx = f32::MIN;
                    for j in 0..n {
                        let kj = &k[j * hid + h * hd..j * hid + (h + 1) * hd];
                        sc[j] = qi.iter().zip(kj).map(|(a, b)| a * b).sum::<f32>() * scale;
                        mx = mx.max(sc[j]);
                    }
                    let mut sum = 0.0;
                    for s in sc.iter_mut() {
                        *s = libm::expf(*s - mx);
                        sum += *s;
                    }
                    for j in 0..n {
                        let p = sc[j] / sum;
                        for d in 0..hd {
                            ctx[i * hid + h * hd + d] += p * v[j * hid + h * hd + d];
                        }
                    }
                }
            }
            let mut a = self.linear(&alloc::format!("{l}.attention.dense"), &ctx, n)?;
            for (ai, xi) in a.iter_mut().zip(&x) {
                *ai += xi;
            }
            layer_norm(&mut a, hid, Some((&ag, &ab)), 1e-12);
            let mut f = self.linear(&alloc::format!("{l}.ffn"), &a, n)?;
            for v in f.iter_mut() {
                let u = *v;
                *v = 0.5 * u * (1.0 + libm::tanhf(0.797_884_6 * (u + 0.044715 * u * u * u)));
            }
            let mut f = self.linear(&alloc::format!("{l}.ffn_output"), &f, n)?;
            for (fi, ai) in f.iter_mut().zip(&a) {
                *fi += ai;
            }
            layer_norm(&mut f, hid, Some((&fg, &fb)), 1e-12);
            x = f;
        }
        Ok(x)
    }

    /// The predictor's text encoder: `[t][512]` -> `[t][640]`.
    fn duration_encoder(&self, d_en: &[f32], n: usize, s: &[f32]) -> Result<Vec<f32>, SpeakError> {
        let cat = |x: &[f32], c: usize| -> Vec<f32> {
            let mut o = Vec::with_capacity(n * (c + s.len()));
            for i in 0..n {
                o.extend_from_slice(&x[i * c..(i + 1) * c]);
                o.extend_from_slice(s);
            }
            o
        };
        let mut x = cat(d_en, 512);
        for i in 0..6 {
            let p = alloc::format!("predictor.text_encoder.lstms.{i}");
            if i % 2 == 0 {
                x = self.lstm(&p, &x, n)?;
            } else {
                let h = self.linear(&alloc::format!("{p}.fc"), s, 1)?;
                let c = h.len() / 2;
                layer_norm(&mut x, c, None, 1e-5);
                for row in x.chunks_exact_mut(c) {
                    for j in 0..c {
                        row[j] = (1.0 + h[j]) * row[j] + h[c + j];
                    }
                }
                x = cat(&x, c);
            }
        }
        Ok(x)
    }

    fn text_encoder(&self, ids: &[u32]) -> Result<Vec<f32>, SpeakError> {
        let n = ids.len();
        let emb = self.get("text_encoder.embedding.weight")?;
        let c = emb.cols;
        let mut x = vec![0f32; c * n];
        for (i, &id) in ids.iter().enumerate() {
            let row = floats(&TensorInfo {
                data: emb.row(id as usize),
                rows: 1,
                ..*emb
            });
            for j in 0..c {
                x[j * n + i] = row[j];
            }
        }
        for i in 0..3 {
            let p = alloc::format!("text_encoder.cnn.{i}");
            let y = self.conv(
                &x,
                c,
                &alloc::format!("{p}.0.weight"),
                Some(&alloc::format!("{p}.0.bias")),
                1,
                2,
                1,
            )?;
            let mut yt = transpose(&y, c, n);
            let g = self.vals(&alloc::format!("{p}.1.gamma"))?;
            let b = self.vals(&alloc::format!("{p}.1.beta"))?;
            layer_norm(&mut yt, c, Some((&g, &b)), 1e-5);
            leaky(&mut yt, 0.2);
            x = transpose(&yt, n, c);
        }
        let xt = transpose(&x, c, n);
        let y = self.lstm("text_encoder.lstm", &xt, n)?;
        Ok(transpose(&y, n, c))
    }

    fn generator(
        &self,
        x: &[f32],
        c: usize,
        s: &[f32],
        f0: &[f32],
    ) -> Result<Vec<f32>, SpeakError> {
        let g = "decoder.generator";
        let (n_fft, hop, scale) = (20usize, 5usize, 300usize);
        // The harmonic source (SineGen, SourceModuleHnNSF), in f64.
        let len = f0.len() * scale;
        let mut rad = vec![0f64; len * 9];
        for (i, &f) in f0.iter().enumerate() {
            for j in 0..scale {
                for h in 0..9 {
                    let v = f64::from(f) * (h + 1) as f64 / RATE as f64;
                    rad[(i * scale + j) * 9 + h] = v - libm::floor(v);
                }
            }
        }
        let rad_d = interp(&rad, 9, 1.0 / scale as f64, len / scale);
        let mut phase = vec![0f64; rad_d.len()];
        for h in 0..9 {
            let mut acc = 0.0;
            for i in 0..len / scale {
                acc += rad_d[i * 9 + h];
                phase[i * 9 + h] = acc * 2.0 * core::f64::consts::PI * scale as f64;
            }
        }
        let phase = interp(&phase, 9, scale as f64, len);
        let noise = Noise(0x9E37_79B9_7F4A_7C15).normals(len * 9);
        let lw = self.vals(&alloc::format!("{g}.m_source.l_linear.weight"))?;
        let lb = self.vals(&alloc::format!("{g}.m_source.l_linear.bias"))?[0];
        let mut har_src = vec![0f64; len];
        for i in 0..len {
            let voiced = f0[i / scale] > 10.0;
            let amp = if voiced { 0.003 } else { 0.1 / 3.0 };
            let mut acc = f64::from(lb);
            for h in 0..9 {
                let sine = if voiced {
                    libm::sin(phase[i * 9 + h]) * 0.1
                } else {
                    0.0
                };
                acc += (sine + amp * noise[i * 9 + h]) * f64::from(lw[h]);
            }
            har_src[i] = libm::tanh(acc);
        }
        // Its spectrum (torch.stft, centred, reflect-padded).
        let p = n_fft / 2;
        let mut xp = Vec::with_capacity(len + 2 * p);
        for i in (1..=p).rev() {
            xp.push(har_src[i]);
        }
        xp.extend_from_slice(&har_src);
        for i in 0..p {
            xp.push(har_src[len - 2 - i]);
        }
        let frames = 1 + (xp.len() - n_fft) / hop;
        let win = hann(n_fft);
        let bins = n_fft / 2 + 1;
        let mut har = vec![0f32; 2 * bins * frames];
        for f in 0..frames {
            for k in 0..bins {
                let (mut re, mut im) = (0f64, 0f64);
                for n in 0..n_fft {
                    let v = xp[f * hop + n] * win[n];
                    let a = -2.0 * core::f64::consts::PI * (k * n) as f64 / n_fft as f64;
                    re += v * libm::cos(a);
                    im += v * libm::sin(a);
                }
                if k == 0 || k == n_fft / 2 {
                    // Real, as an FFT gives it: a negative value's angle
                    // is pi, never -pi from rounding.
                    im = 0.0;
                }
                har[k * frames + f] = libm::sqrt(re * re + im * im) as f32;
                har[(bins + k) * frames + f] = libm::atan2(im, re) as f32;
            }
        }
        let hc = 2 * bins;
        self.tr("har", &har);

        let rates = [10usize, 6];
        let ks = [20usize, 12];
        let rk = [3usize, 7, 11];
        let mut x = x.to_vec();
        let mut c = c;
        for i in 0..2 {
            leaky(&mut x, 0.1);
            let (mut xs, k_res) = if i == 0 {
                let sf = rates[1];
                (
                    self.conv(
                        &har,
                        hc,
                        &alloc::format!("{g}.noise_convs.{i}.weight"),
                        Some(&alloc::format!("{g}.noise_convs.{i}.bias")),
                        sf,
                        sf.div_ceil(2),
                        1,
                    )?,
                    7,
                )
            } else {
                (
                    self.conv(
                        &har,
                        hc,
                        &alloc::format!("{g}.noise_convs.{i}.weight"),
                        Some(&alloc::format!("{g}.noise_convs.{i}.bias")),
                        1,
                        0,
                        1,
                    )?,
                    11,
                )
            };
            let o = c / 2;
            self.tr(&alloc::format!("nc{i}"), &xs);
            self.resblock1(
                &alloc::format!("{g}.noise_res.{i}"),
                &mut xs,
                o,
                s,
                k_res,
                [1, 3, 5],
            )?;
            self.tr(&alloc::format!("xs{i}"), &xs);
            let mut y = self.conv_t(
                &x,
                c,
                &alloc::format!("{g}.ups.{i}"),
                o,
                rates[i],
                (ks[i] - rates[i]) / 2,
            )?;
            if i == 1 {
                // ReflectionPad1d((1, 0)).
                let t = y.len() / o;
                let mut z = Vec::with_capacity(o * (t + 1));
                for oi in 0..o {
                    z.push(y[oi * t + 1]);
                    z.extend_from_slice(&y[oi * t..(oi + 1) * t]);
                }
                y = z;
            }
            self.tr(&alloc::format!("up{i}"), &y);
            if y.len() != xs.len() {
                return Err(SpeakError::BadShape);
            }
            for (a, b) in y.iter_mut().zip(&xs) {
                *a += b;
            }
            let mut acc = vec![0f32; y.len()];
            for (j, &k) in rk.iter().enumerate() {
                let mut r = y.clone();
                self.resblock1(
                    &alloc::format!("{g}.resblocks.{}", i * 3 + j),
                    &mut r,
                    o,
                    s,
                    k,
                    [1, 3, 5],
                )?;
                for (a, b) in acc.iter_mut().zip(&r) {
                    *a += b;
                }
            }
            for a in acc.iter_mut() {
                *a /= 3.0;
            }
            x = acc;
            c = o;
            self.tr(&alloc::format!("x{i}"), &x);
        }
        leaky(&mut x, 0.01);
        let x = self.conv(
            &x,
            c,
            &alloc::format!("{g}.conv_post.weight"),
            Some(&alloc::format!("{g}.conv_post.bias")),
            1,
            3,
            1,
        )?;
        // torch.istft, centred.
        let frames = x.len() / hc;
        let total = n_fft + hop * (frames - 1);
        let mut y = vec![0f64; total];
        let mut wsum = vec![0f64; total];
        let mut seg = vec![0f64; n_fft];
        for f in 0..frames {
            seg.fill(0.0);
            for k in 0..bins {
                let mag = libm::exp(f64::from(x[k * frames + f]));
                let ph = libm::sin(f64::from(x[(bins + k) * frames + f]));
                let (re, im) = (mag * libm::cos(ph), mag * libm::sin(ph));
                let wk = if k == 0 || k == n_fft / 2 { 1.0 } else { 2.0 };
                for (n, sv) in seg.iter_mut().enumerate() {
                    let a = 2.0 * core::f64::consts::PI * (k * n) as f64 / n_fft as f64;
                    *sv += wk * (re * libm::cos(a) - im * libm::sin(a));
                }
            }
            for n in 0..n_fft {
                y[f * hop + n] += seg[n] / n_fft as f64 * win[n];
                wsum[f * hop + n] += win[n] * win[n];
            }
        }
        Ok((p..total - p)
            .map(|i| {
                let w = if wsum[i] > 1e-11 { wsum[i] } else { 1.0 };
                (y[i] / w) as f32
            })
            .collect())
    }

    /// Speaks `phonemes` (misaki's alphabet) at `speed` (1 is normal):
    /// the audio at `RATE`, and each token's predicted frames.
    pub fn synthesize(
        &self,
        phonemes: &str,
        speed: f32,
    ) -> Result<(Vec<f32>, Vec<usize>), SpeakError> {
        let ph = self.ids(phonemes);
        if ph.is_empty() {
            return Err(SpeakError::Empty);
        }
        let voice = self.get("voice")?;
        if ph.len() > voice.rows {
            return Err(SpeakError::TooLong);
        }
        let style = floats(&TensorInfo {
            data: voice.row(ph.len() - 1),
            rows: 1,
            ..*voice
        });
        let (s_dec, s_pred) = style.split_at(128);
        let mut ids = vec![0u32];
        ids.extend_from_slice(&ph);
        ids.push(0);
        let n = ids.len();

        let bert = self.albert(&ids)?;
        let d_en = self.linear("bert_encoder", &bert, n)?;
        let d = self.duration_encoder(&d_en, n, s_pred)?;
        let x = self.lstm("predictor.lstm", &d, n)?;
        let dur = self.linear("predictor.duration_proj.linear_layer", &x, n)?;
        let per = dur.len() / n;
        let frames: Vec<usize> = (0..n)
            .map(|i| {
                let v = dur[i * per..(i + 1) * per]
                    .iter()
                    .map(|&a| sigmoid(a))
                    .sum::<f32>()
                    / speed;
                // torch.round: halves to even.
                let r = libm::rintf(v);
                (r as usize).max(1)
            })
            .collect();
        let idx: Vec<usize> = frames
            .iter()
            .enumerate()
            .flat_map(|(i, &f)| core::iter::repeat_n(i, f))
            .collect();
        let nf = idx.len();
        let dc = d.len() / n;
        let mut en = vec![0f32; nf * dc];
        for (j, &i) in idx.iter().enumerate() {
            en[j * dc..(j + 1) * dc].copy_from_slice(&d[i * dc..(i + 1) * dc]);
        }
        let sh = self.lstm("predictor.shared", &en, nf)?;
        let shc = sh.len() / nf;
        let sh = transpose(&sh, nf, shc);
        let mut curves = Vec::new();
        for name in ["F0", "N"] {
            let mut y = sh.clone();
            let mut c = shc;
            for i in 0..3 {
                let (z, o) = self.resblk(
                    &alloc::format!("predictor.{name}.{i}"),
                    &y,
                    c,
                    s_pred,
                    i == 1,
                )?;
                y = z;
                c = o;
            }
            curves.push(self.conv(
                &y,
                c,
                &alloc::format!("predictor.{name}_proj.weight"),
                Some(&alloc::format!("predictor.{name}_proj.bias")),
                1,
                0,
                1,
            )?);
        }
        let n_curve = curves.pop().ok_or(SpeakError::BadShape)?;
        let f0 = curves.pop().ok_or(SpeakError::BadShape)?;
        self.tr("f0", &f0);
        self.tr("n", &n_curve);
        self.tr("d", &d);

        let t_en = self.text_encoder(&ids)?;
        let tc = t_en.len() / n;
        let mut asr = vec![0f32; tc * nf];
        for ci in 0..tc {
            for (j, &i) in idx.iter().enumerate() {
                asr[ci * nf + j] = t_en[ci * n + i];
            }
        }
        // The decoder.
        let dd = "decoder";
        let f0d = self.conv(
            &f0,
            1,
            &alloc::format!("{dd}.F0_conv.weight"),
            Some(&alloc::format!("{dd}.F0_conv.bias")),
            2,
            1,
            1,
        )?;
        let nd = self.conv(
            &n_curve,
            1,
            &alloc::format!("{dd}.N_conv.weight"),
            Some(&alloc::format!("{dd}.N_conv.bias")),
            2,
            1,
            1,
        )?;
        let x = concat(&[&asr, &f0d, &nd]);
        let (mut x, mut c) =
            self.resblk(&alloc::format!("{dd}.encode"), &x, tc + 2, s_dec, false)?;
        let asr_res = self.conv(
            &asr,
            tc,
            &alloc::format!("{dd}.asr_res.0.weight"),
            Some(&alloc::format!("{dd}.asr_res.0.bias")),
            1,
            0,
            1,
        )?;
        let ar = asr_res.len() / nf;
        for i in 0..4 {
            let xin = concat(&[&x, &asr_res, &f0d, &nd]);
            let (y, o) = self.resblk(
                &alloc::format!("{dd}.decode.{i}"),
                &xin,
                c + ar + 2,
                s_dec,
                i == 3,
            )?;
            x = y;
            c = o;
        }
        self.tr("dec", &x);
        let audio = self.generator(&x, c, s_dec, &f0)?;
        Ok((audio, frames))
    }
}

fn snake(x: &mut [f32], alpha: &[f32], t: usize) {
    for (ci, a) in alpha.iter().enumerate() {
        for v in &mut x[ci * t..(ci + 1) * t] {
            let s = libm::sinf(a * *v);
            *v += s * s / a;
        }
    }
}
