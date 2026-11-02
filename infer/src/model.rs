//! The Qwen2 transformer (docs/m5b-design.md §2): one token at a time with a
//! key/value cache. Matrix-vector products run directly on the quantized
//! weights in the mapped file; only small tensors are expanded to f32.

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
}

impl From<GgufError> for ModelError {
    fn from(e: GgufError) -> Self {
        ModelError::Gguf(e)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Config {
    pub n_layer: usize,
    pub n_embd: usize,
    pub n_head: usize,
    pub n_head_kv: usize,
    pub head_dim: usize,
    pub n_ff: usize,
    pub rope_base: f32,
    pub eps: f32,
    pub vocab: usize,
}

/// Rotary frequencies for each pair in a head: `base^(-2i / head_dim)`.
fn rope_freqs(c: &Config) -> Vec<f32> {
    (0..c.head_dim / 2)
        .map(|i| libm::powf(c.rope_base, -2.0 * i as f32 / c.head_dim as f32))
        .collect()
}

struct Layer<'a> {
    attn_norm: Vec<f32>,
    q: TensorInfo<'a>,
    k: TensorInfo<'a>,
    v: TensorInfo<'a>,
    q_bias: Vec<f32>,
    k_bias: Vec<f32>,
    v_bias: Vec<f32>,
    o: TensorInfo<'a>,
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
}

/// Per-conversation working memory: the key/value cache and scratch.
pub struct State {
    pub pos: usize,
    max_ctx: usize,
    k_cache: Vec<f32>,
    v_cache: Vec<f32>,
    x: Vec<f32>,
    xb: Vec<f32>,
    q: Vec<f32>,
    k: Vec<f32>,
    v: Vec<f32>,
    att: Vec<f32>,
    hb: Vec<f32>,
    hb2: Vec<f32>,
    pub logits: Vec<f32>,
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

/// `out[r] = W[r] · x` for every row.
fn matvec(w: &TensorInfo, x: &[f32], out: &mut [f32]) {
    for (r, o) in out.iter_mut().enumerate().take(w.rows) {
        *o = quant::dot(w.ty, w.row(r), x);
    }
}

fn rmsnorm(out: &mut [f32], x: &[f32], weight: &[f32], eps: f32) {
    let ss: f32 = x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32;
    let inv = 1.0 / libm::sqrtf(ss + eps);
    for ((o, &xv), &w) in out.iter_mut().zip(x).zip(weight) {
        *o = xv * inv * w;
    }
}

/// Rotary position embedding, "NEOX" layout: pairs (i, i + half).
fn rope(v: &mut [f32], head_dim: usize, pos: usize, freqs: &[f32]) {
    let half = head_dim / 2;
    for head in v.chunks_exact_mut(head_dim) {
        for (i, &freq) in freqs.iter().enumerate().take(half) {
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

impl<'a> Model<'a> {
    pub fn from_gguf(g: &Gguf<'a>) -> Result<Model<'a>, ModelError> {
        if g.str("general.architecture")? != "qwen2" {
            return Err(ModelError::NotQwen2);
        }
        let n_embd = g.u32("qwen2.embedding_length")? as usize;
        let n_head = g.u32("qwen2.attention.head_count")? as usize;
        let n_head_kv = g.u32("qwen2.attention.head_count_kv")? as usize;
        if n_head == 0
            || n_head_kv == 0
            || !n_embd.is_multiple_of(n_head)
            || !n_head.is_multiple_of(n_head_kv)
        {
            return Err(ModelError::BadShape);
        }
        let embd = *g
            .tensor("token_embd.weight")
            .ok_or(ModelError::MissingTensor)?;
        let cfg = Config {
            n_layer: g.u32("qwen2.block_count")? as usize,
            n_embd,
            n_head,
            n_head_kv,
            head_dim: n_embd / n_head,
            n_ff: g.u32("qwen2.feed_forward_length")? as usize,
            rope_base: g.f32("qwen2.rope.freq_base")?,
            eps: g.f32("qwen2.attention.layer_norm_rms_epsilon")?,
            vocab: embd.rows,
        };
        if embd.cols != n_embd {
            return Err(ModelError::BadShape);
        }
        let kv_dim = cfg.head_dim * n_head_kv;
        let mut layers = Vec::with_capacity(cfg.n_layer);
        for l in 0..cfg.n_layer {
            let n = |s: &str| {
                let mut name = alloc::format!("blk.{l}.");
                name.push_str(s);
                name
            };
            layers.push(Layer {
                attn_norm: small(g, &n("attn_norm.weight"), n_embd)?,
                q: matrix(g, &n("attn_q.weight"), n_embd, n_embd)?,
                k: matrix(g, &n("attn_k.weight"), kv_dim, n_embd)?,
                v: matrix(g, &n("attn_v.weight"), kv_dim, n_embd)?,
                q_bias: small(g, &n("attn_q.bias"), n_embd)?,
                k_bias: small(g, &n("attn_k.bias"), kv_dim)?,
                v_bias: small(g, &n("attn_v.bias"), kv_dim)?,
                o: matrix(g, &n("attn_output.weight"), n_embd, n_embd)?,
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
        })
    }

    pub fn new_state(&self, max_ctx: usize) -> State {
        let c = &self.cfg;
        let kv_dim = c.head_dim * c.n_head_kv;
        State {
            pos: 0,
            max_ctx,
            k_cache: vec![0.0; c.n_layer * max_ctx * kv_dim],
            v_cache: vec![0.0; c.n_layer * max_ctx * kv_dim],
            x: vec![0.0; c.n_embd],
            xb: vec![0.0; c.n_embd],
            q: vec![0.0; c.n_embd],
            k: vec![0.0; kv_dim],
            v: vec![0.0; kv_dim],
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
        for (l, layer) in self.layers.iter().enumerate() {
            rmsnorm(&mut st.xb, &st.x, &layer.attn_norm, c.eps);
            matvec(&layer.q, &st.xb, &mut st.q);
            matvec(&layer.k, &st.xb, &mut st.k);
            matvec(&layer.v, &st.xb, &mut st.v);
            for (a, b) in st.q.iter_mut().zip(&layer.q_bias) {
                *a += b;
            }
            for (a, b) in st.k.iter_mut().zip(&layer.k_bias) {
                *a += b;
            }
            for (a, b) in st.v.iter_mut().zip(&layer.v_bias) {
                *a += b;
            }
            rope(&mut st.q, hd, pos, &self.freqs);
            rope(&mut st.k, hd, pos, &self.freqs);
            let base = l * st.max_ctx * kv_dim;
            st.k_cache[base + pos * kv_dim..base + (pos + 1) * kv_dim].copy_from_slice(&st.k);
            st.v_cache[base + pos * kv_dim..base + (pos + 1) * kv_dim].copy_from_slice(&st.v);

            // Attention, one query head at a time (grouped-query: `group`
            // query heads share each key/value head).
            st.xb.fill(0.0);
            for h in 0..c.n_head {
                let q = &st.q[h * hd..(h + 1) * hd];
                let kvh = h / group;
                let att = &mut st.att[..=pos];
                for (t, a) in att.iter_mut().enumerate() {
                    let k = &st.k_cache[base + t * kv_dim + kvh * hd..][..hd];
                    *a = q.iter().zip(k).map(|(x, y)| x * y).sum::<f32>() * scale;
                }
                softmax(att);
                let out = &mut st.xb[h * hd..(h + 1) * hd];
                for (t, &a) in att.iter().enumerate() {
                    let v = &st.v_cache[base + t * kv_dim + kvh * hd..][..hd];
                    for (o, &vv) in out.iter_mut().zip(v) {
                        *o += a * vv;
                    }
                }
            }
            matvec(&layer.o, &st.xb, &mut st.q);
            for (x, d) in st.x.iter_mut().zip(&st.q) {
                *x += d;
            }

            // Feed-forward (SwiGLU).
            rmsnorm(&mut st.xb, &st.x, &layer.ffn_norm, c.eps);
            matvec(&layer.gate, &st.xb, &mut st.hb);
            matvec(&layer.up, &st.xb, &mut st.hb2);
            for (g, &u) in st.hb.iter_mut().zip(&st.hb2) {
                let silu = *g / (1.0 + libm::expf(-*g));
                *g = silu * u;
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
