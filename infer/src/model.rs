//! The Qwen2, Qwen3 and Qwen3.5 transformers (docs/m5b-design.md §2,
//! docs/qwen3-design.md, docs/qwen35-design.md): one token at a time with
//! a key/value cache. Matrix-vector products run directly on the quantized
//! weights in the mapped file; only small tensors are expanded to f32.
//!
//! Qwen3.5 mixes two kinds of layer: gated full attention (Qwen3's, with
//! an output gate and rotary on part of each head) and Gated DeltaNet, a
//! linear attention whose memory is a fixed-size matrix per head updated
//! by the delta rule. That matrix can't be cut back to an earlier position
//! the way a key/value cache can, so `State` keeps snapshots of it
//! (`keep`, `rewind`).

use crate::gguf::{Gguf, GgufError, TensorInfo};
use crate::quant;
use alloc::vec;
use alloc::vec::Vec;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelError {
    Gguf(GgufError),
    NotQwen2,
    MissingTensor,
    BadShape,
    ContextFull,
    BadToken,
    /// A rewind to a position with no snapshot of the recurrent state.
    NoSnapshot,
}

impl From<GgufError> for ModelError {
    fn from(e: GgufError) -> Self {
        ModelError::Gguf(e)
    }
}

/// Gated DeltaNet sizes (Qwen3.5).
#[derive(Debug, Clone, Copy)]
pub struct Ssm {
    pub heads: usize,
    pub key_dim: usize,
    pub value_dim: usize,
    pub kernel: usize,
}

impl Ssm {
    /// Channels through the convolution: queries, keys, values.
    fn conv_dim(&self) -> usize {
        self.heads * (2 * self.key_dim + self.value_dim)
    }
    fn state_len(&self) -> usize {
        self.heads * self.key_dim * self.value_dim
    }
    fn conv_len(&self) -> usize {
        (self.kernel - 1) * self.conv_dim()
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Config {
    pub n_layer: usize,
    pub n_embd: usize,
    pub n_head: usize,
    pub n_head_kv: usize,
    pub head_dim: usize,
    /// Dimensions of each head that are rotated (all of them before
    /// Qwen3.5, which rotates the first quarter).
    pub rope_dim: usize,
    pub n_ff: usize,
    pub rope_base: f32,
    pub eps: f32,
    pub vocab: usize,
    /// Qwen3 and Qwen3.5: the chat template opens Bella's turns with an
    /// empty `<think></think>` block, so the model answers without
    /// reasoning first (docs/qwen3-design.md).
    pub qwen3: bool,
    /// Qwen3.5's linear-attention layers, if any.
    pub ssm: Option<Ssm>,
}

/// Rotary frequencies for each rotated pair: `base^(-2i / rope_dim)`.
fn rope_freqs(c: &Config) -> Vec<f32> {
    (0..c.rope_dim / 2)
        .map(|i| libm::powf(c.rope_base, -2.0 * i as f32 / c.rope_dim as f32))
        .collect()
}

struct Attn<'a> {
    q: TensorInfo<'a>,
    k: TensorInfo<'a>,
    v: TensorInfo<'a>,
    /// Qwen2 has q/k/v biases; Qwen3 has none (empty here).
    q_bias: Vec<f32>,
    k_bias: Vec<f32>,
    v_bias: Vec<f32>,
    /// Qwen3's per-head RMS norms of queries and keys (empty for Qwen2).
    q_norm: Vec<f32>,
    k_norm: Vec<f32>,
    /// Qwen3.5: the output is multiplied by `sigmoid(gate · x)`.
    gate: Option<TensorInfo<'a>>,
    o: TensorInfo<'a>,
    /// Which key/value cache this layer uses.
    slot: usize,
}

struct Delta<'a> {
    qkv: TensorInfo<'a>,
    z: TensorInfo<'a>,
    beta: TensorInfo<'a>,
    alpha: TensorInfo<'a>,
    out: TensorInfo<'a>,
    /// Per channel, `kernel` taps, oldest first.
    conv: Vec<f32>,
    /// `-exp(A_log)` per head.
    a: Vec<f32>,
    dt_bias: Vec<f32>,
    norm: Vec<f32>,
    /// Which recurrent state this layer uses.
    slot: usize,
}

enum Mixer<'a> {
    Attn(Attn<'a>),
    Delta(Delta<'a>),
}

struct Layer<'a> {
    attn_norm: Vec<f32>,
    mix: Mixer<'a>,
    ffn_norm: Vec<f32>,
    gate: TensorInfo<'a>,
    up: TensorInfo<'a>,
    down: TensorInfo<'a>,
}

pub struct Model<'a> {
    pub cfg: Config,
    embd: TensorInfo<'a>,
    output: TensorInfo<'a>,
    out_norm: Vec<f32>,
    layers: Vec<Layer<'a>>,
    freqs: Vec<f32>,
    n_attn: usize,
    n_delta: usize,
}

/// A copy of the recurrent state at one position (`State::keep`).
struct Snapshot {
    pos: usize,
    ssm: Vec<f32>,
    conv: Vec<f32>,
}

/// Snapshot slots `State::keep` takes: Bella keeps the end of the system
/// prompt and the end of the conversation.
pub const SNAPSHOTS: usize = 2;

/// Per-conversation working memory: the key/value cache, the recurrent
/// state, and scratch.
pub struct State {
    pub pos: usize,
    max_ctx: usize,
    k_cache: Vec<f32>,
    v_cache: Vec<f32>,
    /// Qwen3.5: each DeltaNet layer's memory, `[heads][key][value]`.
    ssm: Vec<f32>,
    /// Qwen3.5: each DeltaNet layer's last `kernel - 1` inputs to its
    /// convolution, oldest first.
    conv: Vec<f32>,
    snapshots: [Option<Snapshot>; SNAPSHOTS],
    x: Vec<f32>,
    xb: Vec<f32>,
    /// Attention output, all query heads (`n_head * head_dim`).
    xq: Vec<f32>,
    q: Vec<f32>,
    k: Vec<f32>,
    v: Vec<f32>,
    /// Qwen3.5: the attention output gate, or the DeltaNet projections.
    g: Vec<f32>,
    dqkv: Vec<f32>,
    dz: Vec<f32>,
    db: Vec<f32>,
    da: Vec<f32>,
    dout: Vec<f32>,
    att: Vec<f32>,
    hb: Vec<f32>,
    hb2: Vec<f32>,
    pub logits: Vec<f32>,
}

impl State {
    /// Remembers the recurrent state here in `slot` (replacing what it
    /// held), so a later `rewind` to this position can bring it back. Does
    /// nothing for models without one.
    pub fn keep(&mut self, slot: usize) {
        if self.ssm.is_empty() {
            return;
        }
        let snap = &mut self.snapshots[slot.min(SNAPSHOTS - 1)];
        match snap {
            Some(s) => {
                s.pos = self.pos;
                s.ssm.copy_from_slice(&self.ssm);
                s.conv.copy_from_slice(&self.conv);
            }
            None => {
                *snap = Some(Snapshot {
                    pos: self.pos,
                    ssm: self.ssm.clone(),
                    conv: self.conv.clone(),
                })
            }
        }
    }

    /// Goes back to `pos`, so the next token fed is read there. With a
    /// recurrent state that needs a snapshot taken at `pos` (or `pos` 0,
    /// the empty state).
    pub fn rewind(&mut self, pos: usize) -> Result<(), ModelError> {
        if !self.ssm.is_empty() && pos != self.pos {
            if pos == 0 {
                self.ssm.fill(0.0);
                self.conv.fill(0.0);
            } else {
                let s = self
                    .snapshots
                    .iter()
                    .flatten()
                    .find(|s| s.pos == pos)
                    .ok_or(ModelError::NoSnapshot)?;
                self.ssm.copy_from_slice(&s.ssm);
                self.conv.copy_from_slice(&s.conv);
            }
        }
        self.pos = pos;
        Ok(())
    }

    /// Whether `rewind` must have a snapshot (the model is recurrent).
    pub fn recurrent(&self) -> bool {
        !self.ssm.is_empty()
    }
}

fn small<'a>(g: &Gguf<'a>, name: &str, len: usize) -> Result<Vec<f32>, ModelError> {
    let t = g.tensor(name).ok_or(ModelError::MissingTensor)?;
    if t.cols * t.rows != len || t.rows != 1 {
        return Err(ModelError::BadShape);
    }
    let mut out = vec![0.0; len];
    quant::dequantize(t.ty, t.data, &mut out);
    Ok(out)
}

/// `small`, or nothing when the file has no such tensor.
fn optional<'a>(g: &Gguf<'a>, name: &str, len: usize) -> Result<Vec<f32>, ModelError> {
    if g.tensor(name).is_none() {
        return Ok(Vec::new());
    }
    small(g, name, len)
}

fn matrix<'a>(
    g: &Gguf<'a>,
    name: &str,
    rows: usize,
    cols: usize,
) -> Result<TensorInfo<'a>, ModelError> {
    let t = *g.tensor(name).ok_or(ModelError::MissingTensor)?;
    if t.rows != rows || t.cols != cols {
        return Err(ModelError::BadShape);
    }
    Ok(t)
}

/// Tokens fed together by `feed_many` (docs/batch-design.md).
pub const BATCH: usize = 16;

/// `outs[t][r] = W[r] · xs[t]` for `n` vectors at once: each weight row is
/// read from memory once and used for every vector while it is in cache,
/// instead of once per token (docs/batch-design.md). The same dot
/// products as `matvec`, so the same results.
fn matmul(w: &TensorInfo, xs: &[f32], outs: &mut [f32], n: usize) {
    let (rows, cols) = (w.rows, w.cols);
    let bytes = w.row_bytes() * rows;
    // Rows split across processors (docs/smp-design.md, step 3).
    if quant::int8_for(w.ty) {
        // Each token's vector quantized once (docs/int8-design.md).
        let qs: Vec<quant::Q8x> = (0..n)
            .map(|t| quant::Q8x::from(&xs[t * cols..(t + 1) * cols]))
            .collect();
        crate::par::fill(outs, rows, n, bytes, &|r, t| {
            quant::dot_q8x(w.ty, w.row(r), &qs[t])
        });
        return;
    }
    crate::par::fill(outs, rows, n, bytes, &|r, t| {
        quant::dot(w.ty, w.row(r), &xs[t * cols..(t + 1) * cols])
    });
}

/// `out[r] = W[r] · x` for every row.
fn matvec(w: &TensorInfo, x: &[f32], out: &mut [f32]) {
    let rows = w.rows.min(out.len());
    let bytes = w.row_bytes() * rows;
    if quant::int8_for(w.ty) {
        // Quantized once, used for every row (docs/int8-design.md).
        let q = quant::Q8x::from(x);
        crate::par::fill(out, rows, 1, bytes, &|r, _| {
            quant::dot_q8x(w.ty, w.row(r), &q)
        });
        return;
    }
    crate::par::fill(out, rows, 1, bytes, &|r, _| quant::dot(w.ty, w.row(r), x));
}

fn rmsnorm(out: &mut [f32], x: &[f32], weight: &[f32], eps: f32) {
    let ss: f32 = x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32;
    let inv = 1.0 / libm::sqrtf(ss + eps);
    for ((o, &xv), &w) in out.iter_mut().zip(x).zip(weight) {
        *o = xv * inv * w;
    }
}

/// RMS-normalises each `weight.len()`-long head of `v` in place.
fn rmsnorm_heads(v: &mut [f32], weight: &[f32], eps: f32) {
    for head in v.chunks_exact_mut(weight.len()) {
        let ss: f32 = head.iter().map(|x| x * x).sum::<f32>() / head.len() as f32;
        let inv = 1.0 / libm::sqrtf(ss + eps);
        for (x, &w) in head.iter_mut().zip(weight) {
            *x *= inv * w;
        }
    }
}

/// Rotary position embedding, "NEOX" layout: pairs (i, i + half) within
/// the first `2 * freqs.len()` dimensions of each head; the rest pass.
fn rope(v: &mut [f32], head_dim: usize, pos: usize, freqs: &[f32]) {
    let half = freqs.len();
    for head in v.chunks_exact_mut(head_dim) {
        for (i, &freq) in freqs.iter().enumerate() {
            let angle = pos as f32 * freq;
            let (s, c) = (libm::sinf(angle), libm::cosf(angle));
            let (a, b) = (head[i], head[i + half]);
            head[i] = a * c - b * s;
            head[i + half] = a * s + b * c;
        }
    }
}

fn softmax(x: &mut [f32]) {
    let max = x.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
    let mut sum = 0.0;
    for v in x.iter_mut() {
        *v = libm::expf(*v - max);
        sum += *v;
    }
    for v in x.iter_mut() {
        *v /= sum;
    }
}

fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + libm::expf(-x))
}

fn silu(x: f32) -> f32 {
    x / (1.0 + libm::expf(-x))
}

fn softplus(x: f32) -> f32 {
    // log(1 + e^x), without overflow for large x.
    if x > 20.0 {
        x
    } else {
        libm::log1pf(libm::expf(x))
    }
}

/// One token through a DeltaNet layer's recurrence, after its four
/// projections: `qkv` (overwritten), `z`, `b` and `a` in, `out` (heads ×
/// value) out. `conv` and `ssm` are this layer's state.
#[allow(clippy::too_many_arguments)]
fn delta_token(
    d: &Delta,
    s: &Ssm,
    eps: f32,
    conv: &mut [f32],
    ssm: &mut [f32],
    qkv: &mut [f32],
    z: &[f32],
    b: &[f32],
    a: &[f32],
    out: &mut [f32],
) {
    let cd = s.conv_dim();
    let k1 = s.kernel - 1;
    // Causal depthwise convolution over the last `kernel` inputs, then
    // SiLU; the state keeps the raw inputs.
    for c in 0..cd {
        let taps = &d.conv[c * s.kernel..(c + 1) * s.kernel];
        let x = qkv[c];
        let mut y = taps[k1] * x;
        for j in 0..k1 {
            y += taps[j] * conv[j * cd + c];
        }
        for j in 0..k1.saturating_sub(1) {
            conv[j * cd + c] = conv[(j + 1) * cd + c];
        }
        if k1 > 0 {
            conv[(k1 - 1) * cd + c] = x;
        }
        qkv[c] = silu(y);
    }
    let (dk, dv) = (s.key_dim, s.value_dim);
    let kd = s.heads * dk;
    let (q_all, rest) = qkv.split_at_mut(kd);
    let (k_all, v_all) = rest.split_at_mut(kd);
    let q_scale = 1.0 / libm::sqrtf(dk as f32);
    for h in 0..s.heads {
        let q = &mut q_all[h * dk..(h + 1) * dk];
        let k = &mut k_all[h * dk..(h + 1) * dk];
        let v = &v_all[h * dv..(h + 1) * dv];
        for (x, scale) in [(&mut *q, q_scale), (&mut *k, 1.0)] {
            let ss: f32 = x.iter().map(|e| e * e).sum();
            let inv = scale / libm::sqrtf(ss + 1e-6);
            for e in x.iter_mut() {
                *e *= inv;
            }
        }
        let beta = sigmoid(b[h]);
        let decay = libm::expf(d.a[h] * softplus(a[h] + d.dt_bias[h]));
        let st = &mut ssm[h * dk * dv..(h + 1) * dk * dv];
        // S ← decay·S; mem = Sᵀk; δ = β(v − mem); S ← S + k δᵀ; o = Sᵀq.
        let o = &mut out[h * dv..(h + 1) * dv];
        o.fill(0.0);
        for e in st.iter_mut() {
            *e *= decay;
        }
        let mut delta = [0f32; 256];
        let delta = &mut delta[..dv];
        for (kk, row) in st.chunks_exact(dv).enumerate() {
            let kv = k[kk];
            for (m, &sv) in delta.iter_mut().zip(row) {
                *m += sv * kv;
            }
        }
        for (m, &vv) in delta.iter_mut().zip(v) {
            *m = beta * (vv - *m);
        }
        for (kk, row) in st.chunks_exact_mut(dv).enumerate() {
            let (kv, qv) = (k[kk], q[kk]);
            for ((sv, &dl), ov) in row.iter_mut().zip(delta.iter()).zip(o.iter_mut()) {
                *sv += kv * dl;
                *ov += *sv * qv;
            }
        }
        // Gated RMS norm: w · norm(o) · silu(z).
        let ss: f32 = o.iter().map(|e| e * e).sum::<f32>() / dv as f32;
        let inv = 1.0 / libm::sqrtf(ss + eps);
        for ((ov, &w), &zv) in o.iter_mut().zip(&d.norm).zip(&z[h * dv..(h + 1) * dv]) {
            *ov = *ov * inv * w * silu(zv);
        }
    }
}

impl<'a> Model<'a> {
    pub fn from_gguf(g: &Gguf<'a>) -> Result<Model<'a>, ModelError> {
        let arch = g.str("general.architecture")?;
        if arch != "qwen2" && arch != "qwen3" && arch != "qwen35" {
            return Err(ModelError::NotQwen2);
        }
        // The same keys under each architecture's prefix.
        macro_rules! key {
            ($k:literal) => {
                match arch {
                    "qwen2" => concat!("qwen2.", $k),
                    "qwen3" => concat!("qwen3.", $k),
                    _ => concat!("qwen35.", $k),
                }
            };
        }
        let n_embd = g.u32(key!("embedding_length"))? as usize;
        let n_head = g.u32(key!("attention.head_count"))? as usize;
        let n_head_kv = g.u32(key!("attention.head_count_kv"))? as usize;
        if n_head == 0 || n_head_kv == 0 || !n_head.is_multiple_of(n_head_kv) {
            return Err(ModelError::BadShape);
        }
        // Qwen3 states its head size; Qwen2's is the width over the heads.
        let head_dim = match g.u32(key!("attention.key_length")) {
            Ok(d) => d as usize,
            Err(_) if n_embd.is_multiple_of(n_head) => n_embd / n_head,
            Err(_) => return Err(ModelError::BadShape),
        };
        if head_dim == 0 || !head_dim.is_multiple_of(2) {
            return Err(ModelError::BadShape);
        }
        let rope_dim = match g.u32(key!("rope.dimension_count")) {
            Ok(d) => d as usize,
            Err(_) => head_dim,
        };
        if rope_dim == 0 || rope_dim > head_dim || !rope_dim.is_multiple_of(2) {
            return Err(ModelError::BadShape);
        }
        let (ssm, interval) = if arch == "qwen35" {
            let s = Ssm {
                heads: g.u32(key!("ssm.head_count"))? as usize,
                key_dim: g.u32(key!("ssm.key_length"))? as usize,
                value_dim: g.u32(key!("ssm.value_length"))? as usize,
                kernel: g.u32(key!("ssm.conv_kernel"))? as usize,
            };
            if s.heads == 0 || s.kernel == 0 || s.value_dim > 256 || s.key_dim == 0 {
                return Err(ModelError::BadShape);
            }
            let interval = g.u32(key!("full_attention_interval"))? as usize;
            if interval == 0 {
                return Err(ModelError::BadShape);
            }
            (Some(s), interval)
        } else {
            (None, 1)
        };
        let embd = *g
            .tensor("token_embd.weight")
            .ok_or(ModelError::MissingTensor)?;
        let cfg = Config {
            n_layer: g.u32(key!("block_count"))? as usize,
            n_embd,
            n_head,
            n_head_kv,
            head_dim,
            rope_dim,
            n_ff: g.u32(key!("feed_forward_length"))? as usize,
            rope_base: g.f32(key!("rope.freq_base"))?,
            eps: g.f32(key!("attention.layer_norm_rms_epsilon"))?,
            vocab: embd.rows,
            qwen3: arch != "qwen2",
            ssm,
        };
        if embd.cols != n_embd {
            return Err(ModelError::BadShape);
        }
        let kv_dim = cfg.head_dim * n_head_kv;
        let q_dim = cfg.head_dim * n_head;
        let mut layers = Vec::with_capacity(cfg.n_layer);
        let (mut n_attn, mut n_delta) = (0, 0);
        for l in 0..cfg.n_layer {
            let n = |s: &str| {
                let mut name = alloc::format!("blk.{l}.");
                name.push_str(s);
                name
            };
            // Qwen3.5: every `interval`-th layer is full attention.
            let mix = match ssm {
                Some(s) if !(l + 1).is_multiple_of(interval) => {
                    let vd = s.heads * s.value_dim;
                    let d = Delta {
                        qkv: matrix(g, &n("ssm_qkv.weight"), s.conv_dim(), n_embd)?,
                        z: matrix(g, &n("ssm_z.weight"), vd, n_embd)?,
                        beta: matrix(g, &n("ssm_beta.weight"), s.heads, n_embd)?,
                        alpha: matrix(g, &n("ssm_alpha.weight"), s.heads, n_embd)?,
                        out: matrix(g, &n("ssm_out.weight"), n_embd, vd)?,
                        conv: small(g, &n("ssm_conv.weight"), s.conv_dim() * s.kernel)?,
                        a: small(g, &n("ssm_a"), s.heads)?,
                        dt_bias: small(g, &n("ssm_dt_bias"), s.heads)?,
                        norm: small(g, &n("ssm_norm.weight"), s.value_dim)?,
                        slot: n_delta,
                    };
                    n_delta += 1;
                    Mixer::Delta(d)
                }
                _ => {
                    let gate = if ssm.is_some() {
                        Some(matrix(g, &n("attn_gate.weight"), q_dim, n_embd)?)
                    } else {
                        None
                    };
                    let a = Attn {
                        q: matrix(g, &n("attn_q.weight"), q_dim, n_embd)?,
                        k: matrix(g, &n("attn_k.weight"), kv_dim, n_embd)?,
                        v: matrix(g, &n("attn_v.weight"), kv_dim, n_embd)?,
                        q_bias: optional(g, &n("attn_q.bias"), q_dim)?,
                        k_bias: optional(g, &n("attn_k.bias"), kv_dim)?,
                        v_bias: optional(g, &n("attn_v.bias"), kv_dim)?,
                        q_norm: optional(g, &n("attn_q_norm.weight"), cfg.head_dim)?,
                        k_norm: optional(g, &n("attn_k_norm.weight"), cfg.head_dim)?,
                        gate,
                        o: matrix(g, &n("attn_output.weight"), n_embd, q_dim)?,
                        slot: n_attn,
                    };
                    n_attn += 1;
                    Mixer::Attn(a)
                }
            };
            layers.push(Layer {
                attn_norm: small(g, &n("attn_norm.weight"), n_embd)?,
                mix,
                ffn_norm: small(g, &n("ffn_norm.weight"), n_embd)?,
                gate: matrix(g, &n("ffn_gate.weight"), cfg.n_ff, n_embd)?,
                up: matrix(g, &n("ffn_up.weight"), cfg.n_ff, n_embd)?,
                down: matrix(g, &n("ffn_down.weight"), n_embd, cfg.n_ff)?,
            });
        }
        // Qwen2.5-0.5B stores a separate output matrix; fall back to the
        // tied embedding if a file does not.
        let output = match g.tensor("output.weight") {
            Some(t) => *t,
            None => embd,
        };
        if output.cols != n_embd || output.rows != cfg.vocab {
            return Err(ModelError::BadShape);
        }
        Ok(Model {
            cfg,
            embd,
            output,
            out_norm: small(g, "output_norm.weight", n_embd)?,
            layers,
            freqs: rope_freqs(&cfg),
            n_attn,
            n_delta,
        })
    }

    /// The hidden state after the last token fed, through the output
    /// norm: what an embedding model's vector is made from
    /// (docs/vectors-design.md).
    pub fn final_hidden(&self, st: &State) -> Vec<f32> {
        let mut out = vec![0.0; self.cfg.n_embd];
        rmsnorm(&mut out, &st.x, &self.out_norm, self.cfg.eps);
        out
    }

    pub fn new_state(&self, max_ctx: usize) -> State {
        let c = &self.cfg;
        let kv_dim = c.head_dim * c.n_head_kv;
        let (ssm_len, conv_len, cd, vd) = match c.ssm {
            Some(s) => (
                self.n_delta * s.state_len(),
                self.n_delta * s.conv_len(),
                s.conv_dim(),
                s.heads * s.value_dim,
            ),
            None => (0, 0, 0, 0),
        };
        let heads = c.ssm.map_or(0, |s| s.heads);
        State {
            pos: 0,
            max_ctx,
            k_cache: vec![0.0; self.n_attn * max_ctx * kv_dim],
            v_cache: vec![0.0; self.n_attn * max_ctx * kv_dim],
            ssm: vec![0.0; ssm_len],
            conv: vec![0.0; conv_len],
            snapshots: [const { None }; SNAPSHOTS],
            x: vec![0.0; c.n_embd],
            xb: vec![0.0; c.n_embd],
            xq: vec![0.0; c.head_dim * c.n_head],
            q: vec![0.0; c.head_dim * c.n_head],
            k: vec![0.0; kv_dim],
            v: vec![0.0; kv_dim],
            g: vec![
                0.0;
                if c.ssm.is_some() {
                    c.head_dim * c.n_head
                } else {
                    0
                }
            ],
            dqkv: vec![0.0; cd],
            dz: vec![0.0; vd],
            db: vec![0.0; heads],
            da: vec![0.0; heads],
            dout: vec![0.0; vd],
            att: vec![0.0; max_ctx],
            hb: vec![0.0; c.n_ff],
            hb2: vec![0.0; c.n_ff],
            logits: vec![0.0; c.vocab],
        }
    }

    /// Runs one token at position `st.pos`, appends it to the cache, and
    /// leaves next-token logits in `st.logits`.
    pub fn forward(&self, st: &mut State, token: u32) -> Result<(), ModelError> {
        self.step(st, token, true)
    }

    /// As `forward`, without the next-token logits (`st.logits` is left
    /// as it was): for prompt tokens whose scores are never read. The
    /// output projection is the largest matrix, so this saves about a
    /// quarter of the work (docs/a1-design.md).
    pub fn feed(&self, st: &mut State, token: u32) -> Result<(), ModelError> {
        self.step(st, token, false)
    }

    /// `feed` for each of `ids`, `BATCH` tokens at a time: what reading a
    /// prompt costs, with each weight read once per batch rather than once
    /// per token (docs/batch-design.md). Leaves `st` as feeding them one
    /// by one would.
    pub fn feed_many(&self, st: &mut State, ids: &[u32]) -> Result<(), ModelError> {
        for chunk in ids.chunks(BATCH) {
            self.batch(st, chunk, None)?;
        }
        Ok(())
    }

    /// Reads a prompt: `ids` in batches, with the last one through
    /// `forward` when `logits` (its scores are read next). `go(done)` is
    /// asked before each batch and may stop (then `Ok(false)`).
    pub fn read_prompt(
        &self,
        st: &mut State,
        ids: &[u32],
        logits: bool,
        mut go: impl FnMut(usize) -> bool,
    ) -> Result<bool, ModelError> {
        let body = if logits && !ids.is_empty() {
            &ids[..ids.len() - 1]
        } else {
            ids
        };
        let mut done = 0;
        for chunk in body.chunks(BATCH) {
            if !go(done) {
                return Ok(false);
            }
            self.batch(st, chunk, None)?;
            done += chunk.len();
        }
        if logits && let Some(&last) = ids.last() {
            if !go(done) {
                return Ok(false);
            }
            self.forward(st, last)?;
        }
        Ok(true)
    }

    /// Feeds `ids` (at most `BATCH`) together, and checks a draft's guesses
    /// (docs/draft-design.md): with `best`, the most likely next token after
    /// each of them, in order (`ids.len()` of them).
    pub fn feed_checking(
        &self,
        st: &mut State,
        ids: &[u32],
        best: &mut Vec<u32>,
    ) -> Result<(), ModelError> {
        best.clear();
        for chunk in ids.chunks(BATCH) {
            self.batch(st, chunk, Some(best))?;
        }
        Ok(())
    }

    fn batch(
        &self,
        st: &mut State,
        ids: &[u32],
        best: Option<&mut Vec<u32>>,
    ) -> Result<(), ModelError> {
        let c = &self.cfg;
        let n = ids.len();
        if n == 0 {
            return Ok(());
        }
        if n == 1 {
            return self.step(st, ids[0], false);
        }
        let (hd, kv_dim, q_dim) = (c.head_dim, c.head_dim * c.n_head_kv, c.head_dim * c.n_head);
        if ids.iter().any(|&t| t as usize >= c.vocab) {
            return Err(ModelError::BadToken);
        }
        if st.pos + n > st.max_ctx {
            return Err(ModelError::ContextFull);
        }
        let (e, ff) = (c.n_embd, c.n_ff);
        let mut x = vec![0.0f32; n * e];
        let mut xb = vec![0.0f32; n * e];
        let mut q = vec![0.0f32; n * q_dim];
        let mut k = vec![0.0f32; n * kv_dim];
        let mut v = vec![0.0f32; n * kv_dim];
        let mut xq = vec![0.0f32; n * q_dim];
        let mut hb = vec![0.0f32; n * ff];
        let mut hb2 = vec![0.0f32; n * ff];
        let mut g = vec![0.0f32; if c.ssm.is_some() { n * q_dim } else { 0 }];
        let (mut dqkv, mut dz, mut db, mut da) = match c.ssm {
            Some(s) => (
                vec![0.0f32; n * s.conv_dim()],
                vec![0.0f32; n * s.heads * s.value_dim],
                vec![0.0f32; n * s.heads],
                vec![0.0f32; n * s.heads],
            ),
            None => (Vec::new(), Vec::new(), Vec::new(), Vec::new()),
        };
        for (t, &id) in ids.iter().enumerate() {
            quant::dequantize(
                self.embd.ty,
                self.embd.row(id as usize),
                &mut x[t * e..(t + 1) * e],
            );
        }
        let group = c.n_head / c.n_head_kv;
        let scale = 1.0 / libm::sqrtf(hd as f32);
        for layer in &self.layers {
            for t in 0..n {
                rmsnorm(
                    &mut xb[t * e..(t + 1) * e],
                    &x[t * e..(t + 1) * e],
                    &layer.attn_norm,
                    c.eps,
                );
            }
            match &layer.mix {
                Mixer::Attn(at) => {
                    matmul(&at.q, &xb, &mut q, n);
                    matmul(&at.k, &xb, &mut k, n);
                    matmul(&at.v, &xb, &mut v, n);
                    if let Some(gw) = &at.gate {
                        matmul(gw, &xb, &mut g, n);
                    }
                    let base = at.slot * st.max_ctx * kv_dim;
                    for t in 0..n {
                        let (qt, kt, vt) = (
                            &mut q[t * q_dim..(t + 1) * q_dim],
                            &mut k[t * kv_dim..(t + 1) * kv_dim],
                            &mut v[t * kv_dim..(t + 1) * kv_dim],
                        );
                        for (a, b) in qt.iter_mut().zip(&at.q_bias) {
                            *a += b;
                        }
                        for (a, b) in kt.iter_mut().zip(&at.k_bias) {
                            *a += b;
                        }
                        for (a, b) in vt.iter_mut().zip(&at.v_bias) {
                            *a += b;
                        }
                        if !at.q_norm.is_empty() {
                            rmsnorm_heads(qt, &at.q_norm, c.eps);
                        }
                        if !at.k_norm.is_empty() {
                            rmsnorm_heads(kt, &at.k_norm, c.eps);
                        }
                        let pos = st.pos + t;
                        rope(qt, hd, pos, &self.freqs);
                        rope(kt, hd, pos, &self.freqs);
                        st.k_cache[base + pos * kv_dim..base + (pos + 1) * kv_dim]
                            .copy_from_slice(kt);
                        st.v_cache[base + pos * kv_dim..base + (pos + 1) * kv_dim]
                            .copy_from_slice(vt);
                    }
                    // Attention: each token sees the cache up to itself.
                    xq.fill(0.0);
                    for t in 0..n {
                        let pos = st.pos + t;
                        for h in 0..c.n_head {
                            let qh = &q[t * q_dim + h * hd..t * q_dim + (h + 1) * hd];
                            let kvh = h / group;
                            let att = &mut st.att[..=pos];
                            for (s, a) in att.iter_mut().enumerate() {
                                let kk = &st.k_cache[base + s * kv_dim + kvh * hd..][..hd];
                                *a = qh.iter().zip(kk).map(|(x, y)| x * y).sum::<f32>() * scale;
                            }
                            softmax(att);
                            let out = &mut xq[t * q_dim + h * hd..t * q_dim + (h + 1) * hd];
                            for (s, &a) in att.iter().enumerate() {
                                let vv = &st.v_cache[base + s * kv_dim + kvh * hd..][..hd];
                                for (o, &w) in out.iter_mut().zip(vv) {
                                    *o += a * w;
                                }
                            }
                        }
                    }
                    if at.gate.is_some() {
                        for (o, &gv) in xq.iter_mut().zip(&g) {
                            *o *= sigmoid(gv);
                        }
                    }
                    matmul(&at.o, &xq, &mut xb, n);
                }
                Mixer::Delta(d) => {
                    let s = c.ssm.as_ref().expect("a DeltaNet layer has sizes");
                    matmul(&d.qkv, &xb, &mut dqkv, n);
                    matmul(&d.z, &xb, &mut dz, n);
                    matmul(&d.beta, &xb, &mut db, n);
                    matmul(&d.alpha, &xb, &mut da, n);
                    let (cd, vd, hs) = (s.conv_dim(), s.heads * s.value_dim, s.heads);
                    let conv = &mut st.conv[d.slot * s.conv_len()..(d.slot + 1) * s.conv_len()];
                    let ssm = &mut st.ssm[d.slot * s.state_len()..(d.slot + 1) * s.state_len()];
                    // The recurrence is token by token.
                    let mut outs = vec![0.0f32; n * vd];
                    for t in 0..n {
                        delta_token(
                            d,
                            s,
                            c.eps,
                            conv,
                            ssm,
                            &mut dqkv[t * cd..(t + 1) * cd],
                            &dz[t * vd..(t + 1) * vd],
                            &db[t * hs..(t + 1) * hs],
                            &da[t * hs..(t + 1) * hs],
                            &mut outs[t * vd..(t + 1) * vd],
                        );
                    }
                    matmul(&d.out, &outs, &mut xb, n);
                }
            }
            for (a, d) in x.iter_mut().zip(&xb) {
                *a += d;
            }
            // Feed-forward (SwiGLU).
            for t in 0..n {
                rmsnorm(
                    &mut xb[t * e..(t + 1) * e],
                    &x[t * e..(t + 1) * e],
                    &layer.ffn_norm,
                    c.eps,
                );
            }
            matmul(&layer.gate, &xb, &mut hb, n);
            matmul(&layer.up, &xb, &mut hb2, n);
            for (g, &u) in hb.iter_mut().zip(&hb2) {
                *g = silu(*g) * u;
            }
            matmul(&layer.down, &hb, &mut xb, n);
            for (a, d) in x.iter_mut().zip(&xb) {
                *a += d;
            }
        }
        // Each position's most likely next token, for a draft's check.
        if let Some(best) = best {
            for t in 0..n {
                rmsnorm(
                    &mut xb[t * e..(t + 1) * e],
                    &x[t * e..(t + 1) * e],
                    &self.out_norm,
                    c.eps,
                );
            }
            let mut logits = vec![0.0f32; n * c.vocab];
            matmul(&self.output, &xb, &mut logits, n);
            best.extend(logits.chunks(c.vocab).map(|l| crate::argmax(l) as u32));
            // The last position's scores, as `forward` leaves them.
            st.logits.copy_from_slice(&logits[(n - 1) * c.vocab..]);
        }
        // The state as after the last token, ready for the next.
        st.x.copy_from_slice(&x[(n - 1) * e..n * e]);
        st.pos += n;
        Ok(())
    }

    fn step(&self, st: &mut State, token: u32, logits: bool) -> Result<(), ModelError> {
        let c = &self.cfg;
        let (hd, kv_dim) = (c.head_dim, c.head_dim * c.n_head_kv);
        if token as usize >= c.vocab {
            return Err(ModelError::BadToken);
        }
        if st.pos >= st.max_ctx {
            return Err(ModelError::ContextFull);
        }
        let pos = st.pos;
        quant::dequantize(self.embd.ty, self.embd.row(token as usize), &mut st.x);
        let group = c.n_head / c.n_head_kv;
        let scale = 1.0 / libm::sqrtf(hd as f32);
        for layer in &self.layers {
            rmsnorm(&mut st.xb, &st.x, &layer.attn_norm, c.eps);
            match &layer.mix {
                Mixer::Attn(at) => {
                    matvec(&at.q, &st.xb, &mut st.q);
                    matvec(&at.k, &st.xb, &mut st.k);
                    matvec(&at.v, &st.xb, &mut st.v);
                    if let Some(gw) = &at.gate {
                        matvec(gw, &st.xb, &mut st.g);
                    }
                    for (a, b) in st.q.iter_mut().zip(&at.q_bias) {
                        *a += b;
                    }
                    for (a, b) in st.k.iter_mut().zip(&at.k_bias) {
                        *a += b;
                    }
                    for (a, b) in st.v.iter_mut().zip(&at.v_bias) {
                        *a += b;
                    }
                    if !at.q_norm.is_empty() {
                        rmsnorm_heads(&mut st.q, &at.q_norm, c.eps);
                    }
                    if !at.k_norm.is_empty() {
                        rmsnorm_heads(&mut st.k, &at.k_norm, c.eps);
                    }
                    rope(&mut st.q, hd, pos, &self.freqs);
                    rope(&mut st.k, hd, pos, &self.freqs);
                    let base = at.slot * st.max_ctx * kv_dim;
                    st.k_cache[base + pos * kv_dim..base + (pos + 1) * kv_dim]
                        .copy_from_slice(&st.k);
                    st.v_cache[base + pos * kv_dim..base + (pos + 1) * kv_dim]
                        .copy_from_slice(&st.v);

                    // Attention, one query head at a time (grouped-query:
                    // `group` query heads share each key/value head).
                    st.xq.fill(0.0);
                    for h in 0..c.n_head {
                        let q = &st.q[h * hd..(h + 1) * hd];
                        let kvh = h / group;
                        let att = &mut st.att[..=pos];
                        for (t, a) in att.iter_mut().enumerate() {
                            let k = &st.k_cache[base + t * kv_dim + kvh * hd..][..hd];
                            *a = q.iter().zip(k).map(|(x, y)| x * y).sum::<f32>() * scale;
                        }
                        softmax(att);
                        let out = &mut st.xq[h * hd..(h + 1) * hd];
                        for (t, &a) in att.iter().enumerate() {
                            let v = &st.v_cache[base + t * kv_dim + kvh * hd..][..hd];
                            for (o, &vv) in out.iter_mut().zip(v) {
                                *o += a * vv;
                            }
                        }
                    }
                    if at.gate.is_some() {
                        for (o, &gv) in st.xq.iter_mut().zip(&st.g) {
                            *o *= sigmoid(gv);
                        }
                    }
                    matvec(&at.o, &st.xq, &mut st.xb);
                }
                Mixer::Delta(d) => {
                    let s = c.ssm.as_ref().expect("a DeltaNet layer has sizes");
                    matvec(&d.qkv, &st.xb, &mut st.dqkv);
                    matvec(&d.z, &st.xb, &mut st.dz);
                    matvec(&d.beta, &st.xb, &mut st.db);
                    matvec(&d.alpha, &st.xb, &mut st.da);
                    let conv = &mut st.conv[d.slot * s.conv_len()..(d.slot + 1) * s.conv_len()];
                    let ssm = &mut st.ssm[d.slot * s.state_len()..(d.slot + 1) * s.state_len()];
                    delta_token(
                        d,
                        s,
                        c.eps,
                        conv,
                        ssm,
                        &mut st.dqkv,
                        &st.dz,
                        &st.db,
                        &st.da,
                        &mut st.dout,
                    );
                    matvec(&d.out, &st.dout, &mut st.xb);
                }
            }
            for (x, d) in st.x.iter_mut().zip(&st.xb) {
                *x += d;
            }

            // Feed-forward (SwiGLU).
            rmsnorm(&mut st.xb, &st.x, &layer.ffn_norm, c.eps);
            matvec(&layer.gate, &st.xb, &mut st.hb);
            matvec(&layer.up, &st.xb, &mut st.hb2);
            for (g, &u) in st.hb.iter_mut().zip(&st.hb2) {
                *g = silu(*g) * u;
            }
            matvec(&layer.down, &st.hb, &mut st.xb);
            for (x, d) in st.x.iter_mut().zip(&st.xb) {
                *x += d;
            }
        }
        if logits {
            rmsnorm(&mut st.xb, &st.x, &self.out_norm, c.eps);
            matvec(&self.output, &st.xb, &mut st.logits);
        }
        st.pos += 1;
        Ok(())
    }
}
