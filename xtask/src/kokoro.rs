//! Kokoro-82M's PyTorch files as one GGUF file (docs/speech-design.md).
//!
//! A `.pth` is a zip of a pickle (`data.pkl`, naming the tensors) and one
//! raw file per storage. The pickle is read by a small machine that knows
//! only the opcodes and the three names a state dict uses
//! (`collections.OrderedDict`, `torch._utils._rebuild_tensor_v2`,
//! `torch.FloatStorage`) and refuses everything else: nothing in the file
//! can run.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::hear::{Json, parse_json};

/// Where the downloaded files are (tools/models/README.md, "Voice").
pub fn kokoro_dir(root: &Path) -> PathBuf {
    root.join("tools/models/voice/kokoro-82m")
}

/// The converted model.
pub fn gguf_path(root: &Path) -> PathBuf {
    root.join("tools/models/voice/kokoro-82m-f32.gguf")
}

fn read(p: &Path) -> Result<Vec<u8>, String> {
    std::fs::read(p).map_err(|e| format!("cannot read {}: {e}", p.display()))
}

// --- zip (stored entries only, as torch.save writes) ----------------------

fn u16_at(b: &[u8], at: usize) -> Result<usize, String> {
    b.get(at..at + 2)
        .map(|s| u16::from_le_bytes([s[0], s[1]]) as usize)
        .ok_or_else(|| "zip: truncated".to_string())
}

fn u32_at(b: &[u8], at: usize) -> Result<usize, String> {
    b.get(at..at + 4)
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]) as usize)
        .ok_or_else(|| "zip: truncated".to_string())
}

/// Each entry's name and data.
fn zip_entries(z: &[u8]) -> Result<BTreeMap<String, &[u8]>, String> {
    let eocd = (0..z.len().saturating_sub(21))
        .rev()
        .take(70_000)
        .find(|&i| z[i..].starts_with(&[0x50, 0x4b, 0x05, 0x06]))
        .ok_or("zip: no end of central directory")?;
    let count = u16_at(z, eocd + 10)?;
    let mut at = u32_at(z, eocd + 16)?;
    let mut out = BTreeMap::new();
    for _ in 0..count {
        if !z
            .get(at..)
            .is_some_and(|s| s.starts_with(&[0x50, 0x4b, 0x01, 0x02]))
        {
            return Err("zip: bad central directory entry".into());
        }
        let method = u16_at(z, at + 10)?;
        let size = u32_at(z, at + 20)?;
        let name_len = u16_at(z, at + 28)?;
        let extra_len = u16_at(z, at + 30)?;
        let comment_len = u16_at(z, at + 32)?;
        let local = u32_at(z, at + 42)?;
        let name = String::from_utf8_lossy(
            z.get(at + 46..at + 46 + name_len)
                .ok_or("zip: truncated name")?,
        )
        .into_owned();
        if method != 0 {
            return Err(format!("zip: {name} is compressed (method {method})"));
        }
        let l_name = u16_at(z, local + 26)?;
        let l_extra = u16_at(z, local + 28)?;
        let start = local + 30 + l_name + l_extra;
        let data = z.get(start..start + size).ok_or("zip: data out of range")?;
        out.insert(name, data);
        at += 46 + name_len + extra_len + comment_len;
    }
    Ok(out)
}

// --- pickle, data only ---------------------------------------------------

#[derive(Clone, Debug)]
enum V {
    None,
    Bool,
    Int(i64),
    Str(String),
    Tuple(Vec<V>),
    Dict(Vec<(V, V)>),
    Global(String),
    Storage(String),
    Tensor {
        key: String,
        offset: usize,
        shape: Vec<usize>,
        stride: Vec<usize>,
    },
    Mark,
}

fn int(v: &V) -> Result<usize, String> {
    match v {
        V::Int(i) if *i >= 0 => Ok(*i as usize),
        _ => Err(format!("pickle: expected a size, got {v:?}")),
    }
}

fn ints(v: &V) -> Result<Vec<usize>, String> {
    match v {
        V::Tuple(t) => t.iter().map(int).collect(),
        _ => Err(format!("pickle: expected a tuple, got {v:?}")),
    }
}

fn unpickle(p: &[u8]) -> Result<V, String> {
    let mut stack: Vec<V> = Vec::new();
    let mut memo: BTreeMap<usize, V> = BTreeMap::new();
    let mut i = 0usize;
    let take = |i: &mut usize, n: usize| -> Result<&[u8], String> {
        let s = p.get(*i..*i + n).ok_or("pickle: truncated")?;
        *i += n;
        Ok(s)
    };
    let pop = |stack: &mut Vec<V>| stack.pop().ok_or_else(|| "pickle: empty stack".to_string());
    let pop_mark = |stack: &mut Vec<V>| -> Result<Vec<V>, String> {
        let m = stack
            .iter()
            .rposition(|v| matches!(v, V::Mark))
            .ok_or("pickle: no mark")?;
        let items = stack.split_off(m + 1);
        stack.pop();
        Ok(items)
    };
    loop {
        let op = *p.get(i).ok_or("pickle: no STOP")?;
        i += 1;
        match op {
            0x80 => {
                take(&mut i, 1)?;
            }
            b'}' => stack.push(V::Dict(Vec::new())),
            b')' => stack.push(V::Tuple(Vec::new())),
            b'(' => stack.push(V::Mark),
            b'N' => stack.push(V::None),
            0x88 | 0x89 => stack.push(V::Bool),
            b'K' => stack.push(V::Int(i64::from(take(&mut i, 1)?[0]))),
            b'M' => {
                let b = take(&mut i, 2)?;
                stack.push(V::Int(i64::from(u16::from_le_bytes([b[0], b[1]]))));
            }
            b'J' => {
                let b = take(&mut i, 4)?;
                stack.push(V::Int(i64::from(i32::from_le_bytes([
                    b[0], b[1], b[2], b[3],
                ]))));
            }
            b'X' => {
                let b = take(&mut i, 4)?;
                let n = u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize;
                let s = String::from_utf8(take(&mut i, n)?.to_vec())
                    .map_err(|_| "pickle: string not UTF-8")?;
                stack.push(V::Str(s));
            }
            b'c' => {
                let mut line = || -> Result<String, String> {
                    let end = p[i..]
                        .iter()
                        .position(|&c| c == b'\n')
                        .ok_or("pickle: GLOBAL without newline")?;
                    let s = String::from_utf8_lossy(&p[i..i + end]).into_owned();
                    i += end + 1;
                    Ok(s)
                };
                let module = line()?;
                let name = line()?;
                let g = format!("{module}.{name}");
                if !matches!(
                    g.as_str(),
                    "collections.OrderedDict"
                        | "torch._utils._rebuild_tensor_v2"
                        | "torch.FloatStorage"
                ) {
                    return Err(format!("pickle: refused {g}"));
                }
                stack.push(V::Global(g));
            }
            b'q' => {
                let k = usize::from(take(&mut i, 1)?[0]);
                memo.insert(k, stack.last().cloned().ok_or("pickle: empty stack")?);
            }
            b'r' => {
                let b = take(&mut i, 4)?;
                let k = u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize;
                memo.insert(k, stack.last().cloned().ok_or("pickle: empty stack")?);
            }
            b'h' => {
                let k = usize::from(take(&mut i, 1)?[0]);
                stack.push(memo.get(&k).cloned().ok_or("pickle: bad memo")?);
            }
            b'j' => {
                let b = take(&mut i, 4)?;
                let k = u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize;
                stack.push(memo.get(&k).cloned().ok_or("pickle: bad memo")?);
            }
            b't' => {
                let items = pop_mark(&mut stack)?;
                stack.push(V::Tuple(items));
            }
            0x85..=0x87 => {
                let n = usize::from(op - 0x84);
                if stack.len() < n {
                    return Err("pickle: short tuple".into());
                }
                let items = stack.split_off(stack.len() - n);
                stack.push(V::Tuple(items));
            }
            b'Q' => {
                // ('storage', FloatStorage, key, location, numel)
                let pid = pop(&mut stack)?;
                match pid {
                    V::Tuple(t)
                        if t.len() == 5
                            && matches!(&t[0], V::Str(s) if s == "storage")
                            && matches!(&t[1], V::Global(g) if g == "torch.FloatStorage") =>
                    {
                        let V::Str(key) = &t[2] else {
                            return Err("pickle: storage key".into());
                        };
                        stack.push(V::Storage(key.clone()));
                    }
                    other => return Err(format!("pickle: refused persistent id {other:?}")),
                }
            }
            b'R' => {
                let args = pop(&mut stack)?;
                let f = pop(&mut stack)?;
                let V::Tuple(a) = args else {
                    return Err("pickle: REDUCE without a tuple".into());
                };
                match f {
                    V::Global(g) if g == "collections.OrderedDict" && a.is_empty() => {
                        stack.push(V::Dict(Vec::new()))
                    }
                    V::Global(g) if g == "torch._utils._rebuild_tensor_v2" && a.len() >= 4 => {
                        let V::Storage(key) = &a[0] else {
                            return Err("pickle: tensor without storage".into());
                        };
                        stack.push(V::Tensor {
                            key: key.clone(),
                            offset: int(&a[1])?,
                            shape: ints(&a[2])?,
                            stride: ints(&a[3])?,
                        });
                    }
                    other => return Err(format!("pickle: refused call of {other:?}")),
                }
            }
            b's' => {
                let v = pop(&mut stack)?;
                let k = pop(&mut stack)?;
                match stack.last_mut() {
                    Some(V::Dict(d)) => d.push((k, v)),
                    _ => return Err("pickle: SETITEM without a dict".into()),
                }
            }
            b'u' => {
                let items = pop_mark(&mut stack)?;
                let Some(V::Dict(d)) = stack.last_mut() else {
                    return Err("pickle: SETITEMS without a dict".into());
                };
                for kv in items.chunks(2) {
                    if let [k, v] = kv {
                        d.push((k.clone(), v.clone()));
                    }
                }
            }
            b'b' => {
                // BUILD: an OrderedDict's `_metadata`; the state is dropped.
                pop(&mut stack)?;
            }
            b'.' => return pop(&mut stack),
            _ => return Err(format!("pickle: refused opcode 0x{op:02x} at {}", i - 1)),
        }
    }
}

/// A tensor's dotted name, shape and values.
pub type Tensor = (String, Vec<usize>, Vec<f32>);

/// Every tensor in a `.pth` or `.pt` file, by its dotted name, as F32.
pub fn load_pth(path: &Path) -> Result<Vec<Tensor>, String> {
    let z = read(path)?;
    let entries = zip_entries(&z)?;
    let (pkl_name, pkl) = entries
        .iter()
        .find(|(n, _)| n.ends_with("data.pkl"))
        .ok_or(format!("{}: no data.pkl", path.display()))?;
    let prefix = &pkl_name[..pkl_name.len() - "data.pkl".len()];
    let root = unpickle(pkl)?;
    let mut out = Vec::new();
    let mut walk_stack = vec![(String::new(), root)];
    while let Some((name, v)) = walk_stack.pop() {
        match v {
            V::Dict(d) => {
                for (k, v) in d.into_iter().rev() {
                    let V::Str(k) = k else {
                        return Err(format!("{name}: a key that is not a string"));
                    };
                    let n = if name.is_empty() {
                        k
                    } else {
                        format!("{name}.{k}")
                    };
                    walk_stack.push((n, v));
                }
            }
            V::Tensor {
                key,
                offset,
                shape,
                stride,
            } => {
                let raw = entries
                    .get(&format!("{prefix}data/{key}"))
                    .ok_or(format!("{name}: no storage {key}"))?;
                let flat: Vec<f32> = raw
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|b| f32::from_le_bytes(*b))
                    .collect();
                let n: usize = shape.iter().product();
                let mut vals = Vec::with_capacity(n);
                let mut idx = vec![0usize; shape.len()];
                for _ in 0..n {
                    let at = offset + idx.iter().zip(&stride).map(|(i, s)| i * s).sum::<usize>();
                    vals.push(*flat.get(at).ok_or(format!("{name}: out of its storage"))?);
                    for d in (0..shape.len()).rev() {
                        idx[d] += 1;
                        if idx[d] < shape[d] {
                            break;
                        }
                        idx[d] = 0;
                    }
                }
                out.push((name, shape, vals));
            }
            other => return Err(format!("{name}: unexpected {other:?}")),
        }
    }
    Ok(out)
}

// --- GGUF -----------------------------------------------------------------

const GGUF_U32: u32 = 4;
const GGUF_STRING: u32 = 8;
const GGUF_ARRAY: u32 = 9;
const ALIGN: usize = 32;

fn put_str(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(&(s.len() as u64).to_le_bytes());
    out.extend_from_slice(s.as_bytes());
}

/// A matrix for the engine: `rows` of `cols` values.
struct Mat {
    name: String,
    rows: usize,
    cols: usize,
    vals: Vec<f32>,
}

/// `g * v / |v|`, the norm over everything but dimension 0.
fn weight_norm(g: &[f32], v: &[f32]) -> Vec<f32> {
    let per = v.len() / g.len();
    let mut out = Vec::with_capacity(v.len());
    for (o, row) in v.chunks(per).enumerate() {
        let n = row
            .iter()
            .map(|a| f64::from(*a) * f64::from(*a))
            .sum::<f64>()
            .sqrt();
        let s = (f64::from(g[o]) / n) as f32;
        out.extend(row.iter().map(|a| a * s));
    }
    out
}

/// `cargo xtask speak-gguf`: Kokoro's weights and the `af_bella` voice as
/// one GGUF file of F32 matrices, prepared for the engine:
/// - weight norm folded into each weight;
/// - a convolution `[out, in, k]` as `[out, in * k]` (im2col);
/// - a transposed convolution `[in, out, k]` as `[out * k, in]`, so a
///   product gives every output's contributions, added up by stride;
/// - an LSTM's two biases as one;
/// - the voice as `voice` `[510, 256]`, a style per phoneme count.
pub fn convert(root: &Path) -> Result<PathBuf, String> {
    let dir = kokoro_dir(root);
    let mut raw: BTreeMap<String, (Vec<usize>, Vec<f32>)> = BTreeMap::new();
    for (n, shape, v) in load_pth(&dir.join("kokoro-v1_0.pth"))? {
        raw.insert(n.replace(".module.", "."), (shape, v));
    }
    let voice = load_pth(&dir.join("voices/af_bella.pt"))?;
    let cfg = parse_json(&String::from_utf8_lossy(&read(&dir.join("config.json"))?))?;

    let mut mats: Vec<Mat> = Vec::new();
    let names: Vec<String> = raw.keys().cloned().collect();
    for name in &names {
        let (shape, vals) = &raw[name];
        if name.ends_with(".weight_g") || name.contains(".bias_hh_l0") {
            continue;
        }
        if let Some(base) = name.strip_suffix(".weight_v") {
            let g = &raw[&format!("{base}.weight_g")].1;
            let w = weight_norm(g, vals);
            let out_name = format!("{base}.weight");
            let transposed = base.contains(".ups.");
            if transposed {
                // [in, out, k] -> [out * k, in]
                let (c, o, k) = (shape[0], shape[1], shape[2]);
                let mut m = vec![0f32; c * o * k];
                for ci in 0..c {
                    for oi in 0..o {
                        for j in 0..k {
                            m[(oi * k + j) * c + ci] = w[(ci * o + oi) * k + j];
                        }
                    }
                }
                mats.push(Mat {
                    name: out_name,
                    rows: o * k,
                    cols: c,
                    vals: m,
                });
            } else {
                let rows = shape[0];
                mats.push(Mat {
                    name: out_name,
                    rows,
                    cols: w.len() / rows,
                    vals: w,
                });
            }
            continue;
        }
        if let Some(at) = name.find(".bias_ih_l0") {
            let (base, rest) = (&name[..at], &name[at + ".bias_ih_l0".len()..]);
            let hh = &raw[&format!("{base}.bias_hh_l0{rest}")].1;
            mats.push(Mat {
                name: format!("{base}.bias_l0{rest}"),
                rows: 1,
                cols: vals.len(),
                vals: vals.iter().zip(hh).map(|(a, b)| a + b).collect(),
            });
            continue;
        }
        let rows = if shape.len() >= 2 && !name.contains(".alpha") {
            shape[0]
        } else {
            1
        };
        mats.push(Mat {
            name: name.clone(),
            rows,
            cols: vals.len() / rows,
            vals: vals.clone(),
        });
    }
    let (_, vshape, vvals) = voice
        .into_iter()
        .next()
        .ok_or("af_bella.pt has no tensor")?;
    mats.push(Mat {
        name: "voice".into(),
        rows: vshape[0],
        cols: vvals.len() / vshape[0],
        vals: vvals,
    });

    // The phoneme alphabet: token strings by id.
    let mut vocab: BTreeMap<usize, String> = BTreeMap::new();
    if let Some(Json::Obj(kv)) = cfg.get("vocab") {
        for (s, id) in kv {
            vocab.insert(id.num().ok_or("bad vocab id")? as usize, s.clone());
        }
    }
    let n_token = cfg.get("n_token").and_then(Json::num).ok_or("no n_token")? as usize;
    let tokens: Vec<String> = (0..n_token)
        .map(|i| vocab.get(&i).cloned().unwrap_or_default())
        .collect();

    let mut kv = Vec::new();
    let mut n_kv = 0u64;
    put_str(&mut kv, "general.architecture");
    kv.extend_from_slice(&GGUF_STRING.to_le_bytes());
    put_str(&mut kv, "kokoro");
    n_kv += 1;
    put_str(&mut kv, "kokoro.sample_rate");
    kv.extend_from_slice(&GGUF_U32.to_le_bytes());
    kv.extend_from_slice(&24000u32.to_le_bytes());
    n_kv += 1;
    put_str(&mut kv, "tokenizer.ggml.tokens");
    kv.extend_from_slice(&GGUF_ARRAY.to_le_bytes());
    kv.extend_from_slice(&GGUF_STRING.to_le_bytes());
    kv.extend_from_slice(&(tokens.len() as u64).to_le_bytes());
    for t in &tokens {
        put_str(&mut kv, t);
    }
    n_kv += 1;

    let mut infos = Vec::new();
    let mut offset = 0usize;
    for m in &mats {
        let mut info = Vec::new();
        put_str(&mut info, &m.name);
        if m.rows == 1 {
            info.extend_from_slice(&1u32.to_le_bytes());
            info.extend_from_slice(&(m.cols as u64).to_le_bytes());
        } else {
            info.extend_from_slice(&2u32.to_le_bytes());
            info.extend_from_slice(&(m.cols as u64).to_le_bytes());
            info.extend_from_slice(&(m.rows as u64).to_le_bytes());
        }
        info.extend_from_slice(&0u32.to_le_bytes()); // F32
        info.extend_from_slice(&(offset as u64).to_le_bytes());
        infos.push(info);
        offset = (offset + m.vals.len() * 4).div_ceil(ALIGN) * ALIGN;
    }
    let mut out = Vec::with_capacity(offset + (1 << 20));
    out.extend_from_slice(b"GGUF");
    out.extend_from_slice(&3u32.to_le_bytes());
    out.extend_from_slice(&(infos.len() as u64).to_le_bytes());
    out.extend_from_slice(&n_kv.to_le_bytes());
    out.extend_from_slice(&kv);
    for i in &infos {
        out.extend_from_slice(i);
    }
    for m in &mats {
        out.resize(out.len().div_ceil(ALIGN) * ALIGN, 0);
        for v in &m.vals {
            out.extend_from_slice(&v.to_le_bytes());
        }
    }
    let path = gguf_path(root);
    std::fs::write(&path, &out).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    println!(
        "speak-gguf: wrote {} ({} bytes, {} tensors, {} phoneme tokens)",
        path.display(),
        out.len(),
        mats.len(),
        tokens.len()
    );
    Ok(path)
}

// --- check ------------------------------------------------------------------

/// Sentences `speak-check` says, as phonemes (misaki's US alphabet), and
/// what speech to text must hear.
pub const CHECK: [(&str, &str); 3] = [
    ("həlˈO, ˌI æm bˈɛlə.", "Hello, I am Bella."),
    ("nˈOts fˈɪlz ðə skɹˈin.", "Notes fills the screen."),
    ("ðə dˈɛntɪst ɪz ˌɔn fɹˈIdA.", "The dentist is on Friday."),
];

/// 24 kHz to 16 kHz: up by 2, a windowed-sinc low pass at 8 kHz, down by 3.
pub fn to_16k(x: &[f32]) -> Vec<f32> {
    let half = 64isize;
    let fc = 8000.0 / 24000.0;
    let h: Vec<f64> = (-half..=half)
        .map(|n| {
            let n = n as f64;
            let s = if n == 0.0 {
                1.0
            } else {
                (std::f64::consts::PI * n * fc).sin() / (std::f64::consts::PI * n * fc)
            };
            let w = 0.5
                - 0.5 * (2.0 * std::f64::consts::PI * (n + half as f64) / (2 * half) as f64).cos();
            s * fc * w
        })
        .collect();
    let up_len = x.len() * 2;
    let out_len = up_len.div_ceil(3);
    (0..out_len)
        .map(|o| {
            let c = (o * 3) as isize;
            let mut acc = 0.0;
            for (j, hv) in h.iter().enumerate() {
                let i = c + j as isize - half;
                if i >= 0 && (i as usize) < up_len && i % 2 == 0 {
                    acc += f64::from(x[i as usize / 2]) * 2.0 * hv;
                }
            }
            acc as f32
        })
        .collect()
}

/// `cargo xtask speak-check`: for each `CHECK` sentence, the engine's audio
/// against the NumPy reference's (same durations; samples within
/// tolerance), and the Moonshine engine must hear the sentence.
pub fn check(root: &Path) -> Result<bool, String> {
    // Relative RMS difference allowed (0.5%). The reference itself moves
    // 0.28% when its pitch curve is nudged by 4e-4 Hz, as f32 rounding
    // does (docs/speech-design.md §3); the bug found while writing this
    // check gave 5%.
    const TOLERANCE: f64 = 5e-3;
    let path = gguf_path(root);
    if !path.is_file() {
        convert(root)?;
    }
    let data = read(&path)?;
    let g = zoo_infer::Gguf::parse(&data).map_err(|e| format!("{}: {e:?}", path.display()))?;
    let mut sp = zoo_infer::speak::Speech::from_gguf(&g).map_err(|e| e.name().to_string())?;
    // `SPEAK_TRACE` set: intermediate values beside the audio, to compare with the
    // reference's dump.
    let trace_dir = root.join("build/speak");
    let tracer = move |name: &str, v: &[f32]| {
        let bytes: Vec<u8> = v.iter().flat_map(|f| f.to_le_bytes()).collect();
        let _ = std::fs::write(trace_dir.join(format!("rust.{name}")), bytes);
    };
    let tracer: &'static zoo_infer::speak::Trace = Box::leak(Box::new(tracer));
    if std::env::var_os("SPEAK_TRACE").is_some() {
        sp.trace = Some(tracer);
    }
    let hpath = crate::hear::gguf_path(root);
    if !hpath.is_file() {
        crate::hear::convert(root)?;
    }
    let hdata = read(&hpath)?;
    let hg = zoo_infer::Gguf::parse(&hdata).map_err(|e| format!("{}: {e:?}", hpath.display()))?;
    let hear = zoo_infer::hear::Hearing::from_gguf(&hg).map_err(|e| e.name().to_string())?;
    let py = crate::hear::python(root)?;
    let dir = kokoro_dir(root);
    let st = dir.join("kokoro-v1_0.safetensors");
    let vst = dir.join("af_bella.safetensors");
    let out_dir = root.join("build/speak");
    std::fs::create_dir_all(&out_dir).map_err(|e| format!("cannot create build/speak: {e}"))?;
    for (src, dst) in [("kokoro-v1_0.pth", &st), ("voices/af_bella.pt", &vst)] {
        if !dst.is_file() {
            let mut cmd = std::process::Command::new(&py);
            cmd.args(["-I"])
                .arg(root.join("tools/reference/kokoro_pth.py"))
                .arg(dir.join(src))
                .arg(dst);
            let out = crate::process::output_bounded(
                cmd,
                std::time::Duration::from_secs(600),
                "kokoro_pth",
            )?;
            if !out.status.success() {
                return Err(format!(
                    "kokoro_pth failed: {}",
                    String::from_utf8_lossy(&out.stderr)
                ));
            }
        }
    }
    let mut all_ok = true;
    for (k, (phonemes, said)) in CHECK.iter().enumerate() {
        let t0 = std::time::Instant::now();
        let (audio, frames) = sp
            .synthesize(phonemes, 1.0)
            .map_err(|e| e.name().to_string())?;
        let secs = t0.elapsed().as_secs_f64();
        let wav = out_dir.join(format!("check-{k}.wav"));
        let dump = out_dir.join(format!("check-{k}-ref.f32"));
        let mut cmd = std::process::Command::new(&py);
        cmd.args(["-I", "-X", "utf8"])
            .arg(root.join("tools/reference/kokoro_ref.py"))
            .arg(&st)
            .arg(&vst)
            .arg(dir.join("config.json"))
            .arg(phonemes)
            .arg(out_dir.join(format!("check-{k}-ref.wav")))
            .arg("--dump")
            .arg(&dump);
        let out =
            crate::process::output_bounded(cmd, std::time::Duration::from_secs(600), "kokoro_ref")?;
        if !out.status.success() {
            return Err(format!(
                "kokoro_ref failed: {}",
                String::from_utf8_lossy(&out.stderr)
            ));
        }
        let stdout = String::from_utf8_lossy(&out.stdout);
        let ref_frames: Vec<usize> = stdout
            .lines()
            .find_map(|l| l.strip_prefix("durations"))
            .unwrap_or("")
            .split_whitespace()
            .filter_map(|t| t.parse().ok())
            .collect();
        let r: Vec<f32> = read(&dump)?
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| f32::from_le_bytes(*b))
            .collect();
        let same_frames = frames == ref_frames;
        let diff = if r.len() == audio.len() {
            let e: f64 = audio
                .iter()
                .zip(&r)
                .map(|(a, b)| f64::from(a - b).powi(2))
                .sum();
            let p: f64 = r.iter().map(|b| f64::from(*b).powi(2)).sum();
            (e / p.max(1e-12)).sqrt()
        } else {
            f64::INFINITY
        };
        let pcm: Vec<u8> = audio
            .iter()
            .flat_map(|v| ((v * 32767.0).clamp(-32768.0, 32767.0) as i16).to_le_bytes())
            .collect();
        write_wav(&wav, &pcm, zoo_infer::speak::RATE as u32)?;
        let heard = hear
            .transcribe(&to_16k(&audio))
            .map_err(|e| e.name().to_string())?;
        let heard_ok = zoo_infer::hear::same_words(&heard, said);
        let ok = same_frames && diff <= TOLERANCE && heard_ok;
        all_ok &= ok;
        println!(
            "{} {said:?}: {:.2} s of audio in {secs:.2} s; {} frames{}; relative difference {diff:.2e}; heard {heard:?}",
            if ok { "OK  " } else { "FAIL" },
            audio.len() as f64 / zoo_infer::speak::RATE as f64,
            frames.iter().sum::<usize>(),
            if same_frames {
                ", as the reference"
            } else {
                ", NOT the reference's"
            },
        );
    }
    Ok(all_ok)
}

/// 16-bit mono WAV.
pub fn write_wav(path: &Path, pcm: &[u8], rate: u32) -> Result<(), String> {
    let mut w = Vec::with_capacity(44 + pcm.len());
    w.extend_from_slice(b"RIFF");
    w.extend_from_slice(&(36 + pcm.len() as u32).to_le_bytes());
    w.extend_from_slice(b"WAVEfmt ");
    w.extend_from_slice(&16u32.to_le_bytes());
    w.extend_from_slice(&1u16.to_le_bytes());
    w.extend_from_slice(&1u16.to_le_bytes());
    w.extend_from_slice(&rate.to_le_bytes());
    w.extend_from_slice(&(rate * 2).to_le_bytes());
    w.extend_from_slice(&2u16.to_le_bytes());
    w.extend_from_slice(&16u16.to_le_bytes());
    w.extend_from_slice(b"data");
    w.extend_from_slice(&(pcm.len() as u32).to_le_bytes());
    w.extend_from_slice(pcm);
    std::fs::write(path, w).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pickle that would run a command when loaded by Python's pickle
    /// is refused at its first name.
    #[test]
    fn code_is_refused() {
        let evil = b"\x80\x02cos\nsystem\nX\x04\x00\x00\x00echo\x85R.";
        let e = unpickle(evil).unwrap_err();
        assert!(e.contains("refused os.system"), "{e}");
        let builtins = b"\x80\x02cbuiltins\neval\nX\x01\x00\x00\x001\x85R.";
        assert!(unpickle(builtins).is_err());
    }

    /// What a state dict holds: an OrderedDict of tensors.
    #[test]
    fn a_state_dict_is_read() {
        // {'w': _rebuild_tensor_v2(storage '0', 0, (2,), (1,), False, OrderedDict())}
        let mut p: Vec<u8> =
            b"\x80\x02}q\x00X\x01\x00\x00\x00wctorch._utils\n_rebuild_tensor_v2\n(".to_vec();
        p.extend_from_slice(b"(X\x07\x00\x00\x00storagectorch\nFloatStorage\nX\x01\x00\x00\x000X\x03\x00\x00\x00cpuK\x02tQ");
        p.extend_from_slice(b"K\x00K\x02\x85K\x01\x85\x89ccollections\nOrderedDict\n)RtRs.");
        match unpickle(&p).unwrap() {
            V::Dict(d) => {
                assert_eq!(d.len(), 1);
                assert!(
                    matches!(&d[0].1, V::Tensor { key, shape, .. } if key == "0" && shape == &[2])
                );
            }
            other => panic!("{other:?}"),
        }
    }
}
