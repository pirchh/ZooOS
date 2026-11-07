//! `cargo xtask qwen35-gguf`: Qwen3.5-2B's official BF16 safetensors as
//! one GGUF file the engine reads (docs/qwen35-design.md). Qwen publishes
//! no GGUF of its own, so this is ours, from the pinned download.
//!
//! - Matrices become `Q8_0` (ggml's `quantize_row_q8_0_ref`); vectors and
//!   the convolution stay `F32`. `cargo xtask requant --bella qwen3.5-2b`
//!   then makes the 4-bit copy, as for Qwen3-1.7B.
//! - The zero-centred RMS norms (`x * (1 + w)` in transformers) are stored
//!   as `1 + w`, so the engine's norm is the one it already has. The gated
//!   DeltaNet norm is a plain `x * w` and is stored as it is.
//! - A full-attention layer's `q_proj` holds, per head, the query and then
//!   the output gate; they are stored as two matrices, `attn_q` and
//!   `attn_gate`.
//! - `A_log` is stored as `-exp(A_log)`, the decay rate the update uses.
//! - The vision tower and the multi-token-prediction layer are left out.
//!
//! The output is deterministic, so its SHA-256 is reproducible.

use crate::hear::{Json, parse_json};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

const GGUF_U32: u32 = 4;
const GGUF_I32: u32 = 5;
const GGUF_F32: u32 = 6;
const GGUF_STRING: u32 = 8;
const GGUF_ARRAY: u32 = 9;
const ALIGN: usize = 32;
const TYPE_F32: u32 = 0;
const TYPE_Q8_0: u32 = 8;

pub fn source_dir(root: &Path) -> PathBuf {
    root.join("tools").join("models").join("qwen3.5-2b")
}

pub fn gguf_path(root: &Path) -> PathBuf {
    root.join("tools")
        .join("models")
        .join("qwen3.5-2b-q8_0.gguf")
}

fn put_str(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(&(s.len() as u64).to_le_bytes());
    out.extend_from_slice(s.as_bytes());
}

struct Meta {
    kv: Vec<u8>,
    n: u64,
}

impl Meta {
    fn key(&mut self, k: &str, ty: u32) {
        put_str(&mut self.kv, k);
        self.kv.extend_from_slice(&ty.to_le_bytes());
        self.n += 1;
    }
    fn u32(&mut self, k: &str, v: u32) {
        self.key(k, GGUF_U32);
        self.kv.extend_from_slice(&v.to_le_bytes());
    }
    fn f32(&mut self, k: &str, v: f32) {
        self.key(k, GGUF_F32);
        self.kv.extend_from_slice(&v.to_le_bytes());
    }
    fn str(&mut self, k: &str, v: &str) {
        self.key(k, GGUF_STRING);
        put_str(&mut self.kv, v);
    }
    fn strings(&mut self, k: &str, v: &[String]) {
        self.key(k, GGUF_ARRAY);
        self.kv.extend_from_slice(&GGUF_STRING.to_le_bytes());
        self.kv.extend_from_slice(&(v.len() as u64).to_le_bytes());
        for s in v {
            put_str(&mut self.kv, s);
        }
    }
    fn i32s(&mut self, k: &str, v: &[i32]) {
        self.key(k, GGUF_ARRAY);
        self.kv.extend_from_slice(&GGUF_I32.to_le_bytes());
        self.kv.extend_from_slice(&(v.len() as u64).to_le_bytes());
        for x in v {
            self.kv.extend_from_slice(&x.to_le_bytes());
        }
    }
}

/// One tensor's values, read from the safetensors file as f32.
struct Source {
    file: std::fs::File,
    base: u64,
    header: Json,
}

impl Source {
    fn open(path: &Path) -> Result<Source, String> {
        let mut file = std::fs::File::open(path)
            .map_err(|e| format!("cannot open {}: {e}", path.display()))?;
        let mut n = [0u8; 8];
        file.read_exact(&mut n)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        let n = u64::from_le_bytes(n);
        let mut h = vec![0u8; n as usize];
        file.read_exact(&mut h)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        let header = parse_json(std::str::from_utf8(&h).map_err(|_| "header is not UTF-8")?)?;
        Ok(Source {
            file,
            base: 8 + n,
            header,
        })
    }

    /// The tensor's shape and its values (BF16 widened to f32; a few small
    /// tensors are stored as F32).
    fn read(&mut self, name: &str) -> Result<(Vec<usize>, Vec<f32>), String> {
        let v = self
            .header
            .get(name)
            .ok_or(format!("the checkpoint has no {name}"))?;
        let width = match v.get("dtype").and_then(Json::str) {
            Some("BF16") => 2,
            Some("F32") => 4,
            other => return Err(format!("{name}: {other:?}, not BF16 or F32")),
        };
        let shape: Vec<usize> = match v.get("shape") {
            Some(Json::Arr(a)) => a.iter().map(|d| d.num().unwrap_or(0.0) as usize).collect(),
            _ => return Err(format!("{name}: no shape")),
        };
        let (a, b) = match v.get("data_offsets") {
            Some(Json::Arr(o)) if o.len() == 2 => (
                o[0].num().unwrap_or(0.0) as u64,
                o[1].num().unwrap_or(0.0) as u64,
            ),
            _ => return Err(format!("{name}: no data_offsets")),
        };
        let count: usize = shape.iter().product();
        if (b - a) as usize != count * width {
            return Err(format!("{name}: size does not match its shape"));
        }
        let mut raw = vec![0u8; count * width];
        self.file
            .seek(SeekFrom::Start(self.base + a))
            .and_then(|_| self.file.read_exact(&mut raw))
            .map_err(|e| format!("{name}: {e}"))?;
        let vals = if width == 4 {
            raw.chunks_exact(4)
                .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                .collect()
        } else {
            raw.chunks_exact(2)
                .map(|c| f32::from_bits(u32::from(u16::from_le_bytes([c[0], c[1]])) << 16))
                .collect()
        };
        Ok((shape, vals))
    }
}

/// ggml's `quantize_row_q8_0_ref` for a row of whole blocks.
fn q8_0(x: &[f32], out: &mut Vec<u8>) {
    for block in x.chunks_exact(32) {
        let amax = block.iter().fold(0f32, |m, v| m.max(v.abs()));
        let d = amax / 127.0;
        let id = if d != 0.0 { 1.0 / d } else { 0.0 };
        out.extend_from_slice(&crate::requant::f32_to_f16(d).to_le_bytes());
        for &v in block {
            out.push((v * id).round() as i8 as u8);
        }
    }
}

struct Out {
    infos: Vec<u8>,
    n: u64,
    blobs: Vec<Vec<u8>>,
    offset: usize,
}

impl Out {
    fn push(&mut self, name: &str, dims: &[usize], ty: u32, blob: Vec<u8>) {
        put_str(&mut self.infos, name);
        self.infos
            .extend_from_slice(&(dims.len() as u32).to_le_bytes());
        for &d in dims {
            self.infos.extend_from_slice(&(d as u64).to_le_bytes());
        }
        self.infos.extend_from_slice(&ty.to_le_bytes());
        self.infos
            .extend_from_slice(&(self.offset as u64).to_le_bytes());
        self.offset = (self.offset + blob.len()).div_ceil(ALIGN) * ALIGN;
        self.blobs.push(blob);
        self.n += 1;
    }
    /// A `[rows, cols]` matrix as `Q8_0` (GGUF dimensions: cols, rows).
    fn matrix(&mut self, name: &str, rows: usize, cols: usize, vals: &[f32]) -> Result<(), String> {
        if vals.len() != rows * cols || !cols.is_multiple_of(32) {
            return Err(format!("{name}: {rows}x{cols} cannot be Q8_0"));
        }
        let mut blob = Vec::with_capacity(rows * cols / 32 * 34);
        q8_0(vals, &mut blob);
        self.push(name, &[cols, rows], TYPE_Q8_0, blob);
        Ok(())
    }
    fn vector(&mut self, name: &str, vals: &[f32]) {
        let blob = vals.iter().flat_map(|v| v.to_le_bytes()).collect();
        self.push(name, &[vals.len()], TYPE_F32, blob);
    }
}

fn num(j: &Json, k: &str) -> Result<f64, String> {
    j.get(k)
        .and_then(Json::num)
        .ok_or(format!("config.json has no {k}"))
}

pub fn convert(root: &Path) -> Result<PathBuf, String> {
    let dir = source_dir(root);
    let read_json = |name: &str| -> Result<Json, String> {
        let p = dir.join(name);
        let s =
            std::fs::read_to_string(&p).map_err(|e| format!("cannot read {}: {e}", p.display()))?;
        parse_json(&s)
    };
    let cfg = read_json("config.json")?;
    let t = cfg
        .get("text_config")
        .ok_or("config.json has no text_config")?;
    let rope = t.get("rope_parameters").ok_or("no rope_parameters")?;
    let n_layer = num(t, "num_hidden_layers")? as usize;
    let n_embd = num(t, "hidden_size")? as usize;
    let n_ff = num(t, "intermediate_size")? as usize;
    let n_head = num(t, "num_attention_heads")? as usize;
    let n_kv = num(t, "num_key_value_heads")? as usize;
    let hd = num(t, "head_dim")? as usize;
    let lk = num(t, "linear_num_key_heads")? as usize;
    let lv = num(t, "linear_num_value_heads")? as usize;
    let dk = num(t, "linear_key_head_dim")? as usize;
    let dv = num(t, "linear_value_head_dim")? as usize;
    let ker = num(t, "linear_conv_kernel_dim")? as usize;
    let interval = num(t, "full_attention_interval")? as usize;
    let rot = (hd as f64 * num(rope, "partial_rotary_factor")?) as usize;
    let kinds: Vec<&str> = match t.get("layer_types") {
        Some(Json::Arr(a)) => a.iter().filter_map(Json::str).collect(),
        _ => return Err("config.json has no layer_types".into()),
    };
    if kinds.len() != n_layer
        || kinds
            .iter()
            .enumerate()
            .any(|(i, k)| (*k == "full_attention") != ((i + 1) % interval == 0))
    {
        return Err("layer_types is not every full_attention_interval-th layer".into());
    }
    if lk != lv {
        return Err("linear key and value heads differ (not handled)".into());
    }

    // Tokenizer.
    let tok = read_json("tokenizer.json")?;
    let model = tok.get("model").ok_or("tokenizer.json has no model")?;
    let vocab_size = num(t, "vocab_size")? as usize;
    let mut tokens = vec![String::new(); vocab_size];
    let mut types = vec![1i32; vocab_size];
    if let Some(Json::Obj(kv)) = model.get("vocab") {
        for (s, id) in kv {
            let id = id.num().ok_or("bad vocab id")? as usize;
            *tokens.get_mut(id).ok_or("vocab id past vocab_size")? = s.clone();
        }
    }
    let mut eos = None;
    if let Some(Json::Arr(added)) = tok.get("added_tokens") {
        for a in added {
            let id = a.get("id").and_then(Json::num).ok_or("bad added token")? as usize;
            let s = a
                .get("content")
                .and_then(Json::str)
                .ok_or("bad added token")?;
            *tokens.get_mut(id).ok_or("added id past vocab_size")? = s.to_string();
            types[id] = 3; // control
            if s == "<|im_end|>" {
                eos = Some(id as u32);
            }
        }
    }
    // Unused ids (the embedding is padded past the vocabulary).
    for (i, s) in tokens.iter_mut().enumerate() {
        if s.is_empty() {
            *s = format!("<|unused_{i}|>");
            types[i] = 5;
        }
    }
    let merges: Vec<String> = match model.get("merges") {
        Some(Json::Arr(m)) => m.iter().filter_map(Json::str).map(String::from).collect(),
        _ => return Err("tokenizer.json has no merges".into()),
    };
    let eos = eos.ok_or("no <|im_end|> token")?;

    let mut meta = Meta {
        kv: Vec::new(),
        n: 0,
    };
    meta.str("general.architecture", "qwen35");
    meta.str(
        "general.name",
        "Qwen3.5-2B (text), converted by cargo xtask qwen35-gguf",
    );
    meta.u32("qwen35.block_count", n_layer as u32);
    meta.u32("qwen35.embedding_length", n_embd as u32);
    meta.u32("qwen35.feed_forward_length", n_ff as u32);
    meta.u32("qwen35.attention.head_count", n_head as u32);
    meta.u32("qwen35.attention.head_count_kv", n_kv as u32);
    meta.u32("qwen35.attention.key_length", hd as u32);
    meta.f32(
        "qwen35.attention.layer_norm_rms_epsilon",
        num(t, "rms_norm_eps")? as f32,
    );
    meta.f32("qwen35.rope.freq_base", num(rope, "rope_theta")? as f32);
    meta.u32("qwen35.rope.dimension_count", rot as u32);
    meta.u32("qwen35.full_attention_interval", interval as u32);
    meta.u32("qwen35.ssm.head_count", lv as u32);
    meta.u32("qwen35.ssm.key_length", dk as u32);
    meta.u32("qwen35.ssm.value_length", dv as u32);
    meta.u32("qwen35.ssm.conv_kernel", ker as u32);
    meta.str("tokenizer.ggml.model", "gpt2");
    meta.str("tokenizer.ggml.pre", "qwen35");
    meta.strings("tokenizer.ggml.tokens", &tokens);
    meta.i32s("tokenizer.ggml.token_type", &types);
    meta.strings("tokenizer.ggml.merges", &merges);
    meta.u32("tokenizer.ggml.eos_token_id", eos);

    let mut src = Source::open(&dir.join("model.safetensors-00001-of-00001.safetensors"))?;
    let mut out = Out {
        infos: Vec::new(),
        n: 0,
        blobs: Vec::new(),
        offset: 0,
    };
    let p = "model.language_model.";
    let plus_one = |v: Vec<f32>| -> Vec<f32> { v.into_iter().map(|w| 1.0 + w).collect() };
    let (_, emb) = src.read(&format!("{p}embed_tokens.weight"))?;
    out.matrix("token_embd.weight", vocab_size, n_embd, &emb)?;
    drop(emb);
    let (_, norm) = src.read(&format!("{p}norm.weight"))?;
    out.vector("output_norm.weight", &plus_one(norm));
    let kd = lk * dk;
    let conv_dim = 2 * kd + lv * dv;
    for (l, kind) in kinds.iter().enumerate() {
        let s = |n: &str| format!("{p}layers.{l}.{n}");
        let o = |n: &str| format!("blk.{l}.{n}");
        let (_, w) = src.read(&s("input_layernorm.weight"))?;
        out.vector(&o("attn_norm.weight"), &plus_one(w));
        let (_, w) = src.read(&s("post_attention_layernorm.weight"))?;
        out.vector(&o("ffn_norm.weight"), &plus_one(w));
        for (from, to, rows, cols) in [
            ("mlp.gate_proj.weight", "ffn_gate.weight", n_ff, n_embd),
            ("mlp.up_proj.weight", "ffn_up.weight", n_ff, n_embd),
            ("mlp.down_proj.weight", "ffn_down.weight", n_embd, n_ff),
        ] {
            let (_, w) = src.read(&s(from))?;
            out.matrix(&o(to), rows, cols, &w)?;
        }
        if *kind == "full_attention" {
            let (_, qg) = src.read(&s("self_attn.q_proj.weight"))?;
            // Rows per head: hd query rows, then hd gate rows.
            let (mut q, mut g) = (Vec::new(), Vec::new());
            for (r, row) in qg.chunks_exact(n_embd).enumerate() {
                if (r / hd).is_multiple_of(2) {
                    q.extend_from_slice(row);
                } else {
                    g.extend_from_slice(row);
                }
            }
            out.matrix(&o("attn_q.weight"), n_head * hd, n_embd, &q)?;
            out.matrix(&o("attn_gate.weight"), n_head * hd, n_embd, &g)?;
            let (_, w) = src.read(&s("self_attn.k_proj.weight"))?;
            out.matrix(&o("attn_k.weight"), n_kv * hd, n_embd, &w)?;
            let (_, w) = src.read(&s("self_attn.v_proj.weight"))?;
            out.matrix(&o("attn_v.weight"), n_kv * hd, n_embd, &w)?;
            let (_, w) = src.read(&s("self_attn.o_proj.weight"))?;
            out.matrix(&o("attn_output.weight"), n_embd, n_head * hd, &w)?;
            let (_, w) = src.read(&s("self_attn.q_norm.weight"))?;
            out.vector(&o("attn_q_norm.weight"), &plus_one(w));
            let (_, w) = src.read(&s("self_attn.k_norm.weight"))?;
            out.vector(&o("attn_k_norm.weight"), &plus_one(w));
        } else {
            let a = |n: &str| s(&format!("linear_attn.{n}"));
            let (_, w) = src.read(&a("in_proj_qkv.weight"))?;
            out.matrix(&o("ssm_qkv.weight"), conv_dim, n_embd, &w)?;
            let (_, w) = src.read(&a("in_proj_z.weight"))?;
            out.matrix(&o("ssm_z.weight"), lv * dv, n_embd, &w)?;
            let (_, w) = src.read(&a("in_proj_b.weight"))?;
            out.matrix(&o("ssm_beta.weight"), lv, n_embd, &w)?;
            let (_, w) = src.read(&a("in_proj_a.weight"))?;
            out.matrix(&o("ssm_alpha.weight"), lv, n_embd, &w)?;
            let (_, w) = src.read(&a("out_proj.weight"))?;
            out.matrix(&o("ssm_out.weight"), n_embd, lv * dv, &w)?;
            let (shape, w) = src.read(&a("conv1d.weight"))?;
            if shape != [conv_dim, 1, ker] {
                return Err(format!("layer {l}: conv1d is {shape:?}"));
            }
            out.vector(&o("ssm_conv.weight"), &w);
            let (_, w) = src.read(&a("A_log"))?;
            let rate: Vec<f32> = w.iter().map(|v| -v.exp()).collect();
            out.vector(&o("ssm_a"), &rate);
            let (_, w) = src.read(&a("dt_bias"))?;
            out.vector(&o("ssm_dt_bias"), &w);
            let (_, w) = src.read(&a("norm.weight"))?;
            out.vector(&o("ssm_norm.weight"), &w);
        }
    }

    let path = gguf_path(root);
    let mut file = std::io::BufWriter::new(
        std::fs::File::create(&path)
            .map_err(|e| format!("cannot write {}: {e}", path.display()))?,
    );
    use std::io::Write;
    let mut head = Vec::new();
    head.extend_from_slice(b"GGUF");
    head.extend_from_slice(&3u32.to_le_bytes());
    head.extend_from_slice(&out.n.to_le_bytes());
    head.extend_from_slice(&meta.n.to_le_bytes());
    head.extend_from_slice(&meta.kv);
    head.extend_from_slice(&out.infos);
    let mut written = head.len();
    let w = |f: &mut std::io::BufWriter<std::fs::File>, b: &[u8]| {
        f.write_all(b)
            .map_err(|e| format!("{}: {e}", path.display()))
    };
    w(&mut file, &head)?;
    for b in &out.blobs {
        let pad = written.div_ceil(ALIGN) * ALIGN - written;
        w(&mut file, &vec![0u8; pad])?;
        w(&mut file, b)?;
        written += pad + b.len();
    }
    file.flush()
        .map_err(|e| format!("{}: {e}", path.display()))?;
    println!(
        "qwen35-gguf: wrote {} ({written} bytes, {} tensors, {} tokens, {} merges)",
        path.display(),
        out.n,
        tokens.len(),
        merges.len()
    );
    Ok(path)
}

/// Raw text prompts for `qwen35-check` (no chat template: the reference
/// takes token ids, and this checks the model, not the template).
const CHECK_PROMPTS: [&str; 2] = ["The capital of France is", "def fibonacci(n):"];
const CHECK_NEW: usize = 8;

/// `cargo xtask qwen35-check`: the engine against the NumPy reference
/// (tools/reference/qwen35_ref.py, `--q8`, so both have the same weights).
/// The prompt's top-5 next tokens must be the same, their logits within
/// a tolerance, and the next `CHECK_NEW` greedy tokens the same.
pub fn check(root: &Path) -> Result<bool, String> {
    // With 8-bit activations (docs/int8-design.md), as infer-check.
    let tolerance = if zoo_infer::quant::int8_for(zoo_infer::quant::TYPE_Q8_0) {
        0.5
    } else {
        0.02
    };
    let path = gguf_path(root);
    let data = std::fs::read(&path).map_err(|e| {
        format!(
            "cannot read {} (run `cargo xtask qwen35-gguf`): {e}",
            path.display()
        )
    })?;
    let g = zoo_infer::Gguf::parse(&data).map_err(|e| format!("{}: {e:?}", path.display()))?;
    let model = zoo_infer::Model::from_gguf(&g).map_err(|e| format!("model: {e:?}"))?;
    let tok = zoo_infer::Tokenizer::from_gguf(&g).map_err(|e| format!("tokenizer: {e:?}"))?;
    let python = crate::hear::python(root)?;
    let mut all_ok = true;
    for prompt in CHECK_PROMPTS {
        let ids = tok.encode(prompt);
        let mut st = model.new_state(ids.len() + CHECK_NEW + 1);
        let t0 = std::time::Instant::now();
        model
            .read_prompt(&mut st, &ids, true, |_| true)
            .map_err(|e| format!("engine: {e:?}"))?;
        let mut order: Vec<usize> = (0..st.logits.len()).collect();
        order.sort_by(|&a, &b| st.logits[b].total_cmp(&st.logits[a]));
        let top: Vec<(u32, f32)> = order[..5]
            .iter()
            .map(|&i| (i as u32, st.logits[i]))
            .collect();
        let mut out = Vec::new();
        for _ in 0..CHECK_NEW {
            let next = zoo_infer::argmax(&st.logits) as u32;
            out.push(next);
            model
                .forward(&mut st, next)
                .map_err(|e| format!("engine: {e:?}"))?;
        }
        let ours_ms = t0.elapsed().as_millis();
        let list: Vec<String> = ids.iter().map(u32::to_string).collect();
        let mut cmd = std::process::Command::new(&python);
        cmd.args(["-I", "-X", "utf8"])
            .arg(root.join("tools/reference/qwen35_ref.py"))
            .arg(source_dir(root))
            .args([
                "--ids",
                &list.join(","),
                "--gen",
                &CHECK_NEW.to_string(),
                "--q8",
            ]);
        let r =
            crate::process::output_bounded(cmd, std::time::Duration::from_secs(3600), "reference")?;
        if !r.status.success() {
            return Err(format!(
                "reference failed: {}",
                String::from_utf8_lossy(&r.stderr)
            ));
        }
        let text = String::from_utf8_lossy(&r.stdout);
        let ref_top: Vec<(u32, f32)> = text
            .lines()
            .find_map(|l| l.strip_prefix("top5 "))
            .unwrap_or("")
            .split(' ')
            .filter_map(|p| p.split_once(':'))
            .filter_map(|(a, b)| Some((a.parse().ok()?, b.parse().ok()?)))
            .collect();
        let ref_out: Vec<u32> = text
            .lines()
            .filter_map(|l| l.strip_prefix("next "))
            .filter_map(|v| v.trim().parse().ok())
            .collect();
        let max_diff = top
            .iter()
            .zip(&ref_top)
            .map(|((a, la), (b, lb))| {
                if a == b {
                    (la - lb).abs()
                } else {
                    f32::INFINITY
                }
            })
            .fold(0.0f32, f32::max);
        let same_out = out == ref_out;
        let ok = ref_top.len() == 5 && max_diff <= tolerance && same_out;
        all_ok &= ok;
        println!(
            "{:<4} {prompt:?}: {} prompt tokens; top-5 logit max diff {max_diff:.4} (tolerance {tolerance}); {} new tokens {} ({ours_ms} ms in the engine)\n     ours:      {:?} {:?}\n     reference: {:?} {:?}",
            if ok { "OK" } else { "FAIL" },
            ids.len(),
            CHECK_NEW,
            if same_out { "the same" } else { "DIFFERENT" },
            top,
            tok.decode(&out),
            ref_top,
            tok.decode(&ref_out),
        );
    }
    Ok(all_ok)
}
