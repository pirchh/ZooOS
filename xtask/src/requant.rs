//! `cargo xtask requant`: rewrites a GGUF model's `Q8_0` weight matrices as
//! `Q4_0` (docs/qwen3-design.md). Qwen publishes Qwen3-1.7B only as
//! `Q8_0`; our other models are `Q4_0`, which reads half the bytes per
//! weight, and reading weights is most of the time per token.
//!
//! The metadata is copied byte for byte. Each 2-D `Q8_0` tensor other than
//! the token embedding becomes `Q4_0` with ggml's reference rounding
//! (`quantize_row_q4_0_ref`). The embedding is kept: with tied embeddings
//! it is also the output layer, where precision matters most. The output
//! is deterministic, so its SHA-256 is reproducible.

use std::path::Path;

const TYPE_F32: u32 = 0;
const TYPE_Q4_0: u32 = 2;
const TYPE_Q8_0: u32 = 8;
const BLOCK: usize = 32;
const Q8_0_BYTES: usize = 34;
const Q4_0_BYTES: usize = 18;

struct Reader<'a> {
    b: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self
            .at
            .checked_add(n)
            .filter(|&e| e <= self.b.len())
            .ok_or("the file ends early")?;
        let s = &self.b[self.at..end];
        self.at = end;
        Ok(s)
    }
    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> Result<u64, String> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn string(&mut self) -> Result<&'a str, String> {
        let n = self.u64()? as usize;
        std::str::from_utf8(self.take(n)?).map_err(|_| "a name is not UTF-8".into())
    }
    /// Skips one metadata value of type `ty`, returning a `u32` value
    /// (for `general.alignment`) when it is one.
    fn skip_value(&mut self, ty: u32) -> Result<Option<u64>, String> {
        let fixed = |t: u32| match t {
            0 | 1 | 7 => Some(1),
            2 | 3 => Some(2),
            4..=6 => Some(4),
            10..=12 => Some(8),
            _ => None,
        };
        match ty {
            4 => Ok(Some(u64::from(self.u32()?))),
            8 => self.string().map(|_| None),
            9 => {
                let inner = self.u32()?;
                let n = self.u64()?;
                for _ in 0..n {
                    self.skip_value(inner)?;
                }
                Ok(None)
            }
            t => {
                let n = fixed(t).ok_or(format!("unknown metadata type {t}"))?;
                self.take(n)?;
                Ok(None)
            }
        }
    }
}

struct Tensor<'a> {
    name: &'a str,
    dims: Vec<u64>,
    ty: u32,
    offset: u64,
}

fn align(n: usize, to: usize) -> usize {
    n.div_ceil(to) * to
}

/// f32 to IEEE half, rounding to nearest even (as ggml's
/// `GGML_FP32_TO_FP16` on x86).
pub fn f32_to_f16(x: f32) -> u16 {
    let b = x.to_bits();
    let sign = ((b >> 16) & 0x8000) as u16;
    let exp = ((b >> 23) & 0xff) as i32;
    let man = b & 0x7f_ffff;
    if exp == 0xff {
        return sign | 0x7c00 | if man != 0 { 0x200 } else { 0 };
    }
    let e = exp - 127 + 15;
    if e >= 0x1f {
        return sign | 0x7c00;
    }
    if e <= 0 {
        if e < -10 {
            return sign;
        }
        let m = man | 0x80_0000;
        let shift = (14 - e) as u32;
        let half = 1u32 << (shift - 1);
        let rest = m & ((1 << shift) - 1);
        let mut h = m >> shift;
        if rest > half || (rest == half && h & 1 == 1) {
            h += 1;
        }
        return sign | h as u16;
    }
    let mut h = ((e as u32) << 10) | (man >> 13);
    let rest = man & 0x1fff;
    if rest > 0x1000 || (rest == 0x1000 && h & 1 == 1) {
        h += 1;
    }
    sign | h as u16
}

/// One `Q8_0` block (32 values) as one `Q4_0` block.
pub fn block_q8_to_q4(q8: &[u8], out: &mut [u8]) {
    let d8 = zoo_infer::quant::f16_to_f32(u16::from_le_bytes([q8[0], q8[1]]));
    let mut x = [0f32; BLOCK];
    for (v, &q) in x.iter_mut().zip(&q8[2..2 + BLOCK]) {
        *v = d8 * f32::from(q as i8);
    }
    // ggml's quantize_row_q4_0_ref.
    let (mut amax, mut max) = (0f32, 0f32);
    for &v in &x {
        if amax < v.abs() {
            amax = v.abs();
            max = v;
        }
    }
    let d = max / -8.0;
    let id = if d != 0.0 { 1.0 / d } else { 0.0 };
    out[..2].copy_from_slice(&f32_to_f16(d).to_le_bytes());
    for j in 0..BLOCK / 2 {
        let lo = ((x[j] * id + 8.5) as i8).min(15) as u8;
        let hi = ((x[j + BLOCK / 2] * id + 8.5) as i8).min(15) as u8;
        out[2 + j] = lo | (hi << 4);
    }
}

pub fn run(input: &Path, output: &Path) -> Result<(), String> {
    let b = std::fs::read(input).map_err(|e| format!("cannot read {}: {e}", input.display()))?;
    let mut r = Reader { b: &b, at: 0 };
    if r.take(4)? != b"GGUF" {
        return Err(format!("{} is not a GGUF file", input.display()));
    }
    let version = r.u32()?;
    if version != 3 {
        return Err(format!("GGUF version {version}; only 3 is handled"));
    }
    let n_tensors = r.u64()? as usize;
    let n_kv = r.u64()?;
    let mut alignment = 32usize;
    for _ in 0..n_kv {
        let key = r.string()?;
        let ty = r.u32()?;
        if let Some(v) = r.skip_value(ty)?
            && key == "general.alignment"
        {
            alignment = v as usize;
        }
    }
    let meta_end = r.at;
    let mut tensors = Vec::with_capacity(n_tensors);
    for _ in 0..n_tensors {
        let name = r.string()?;
        let n_dims = r.u32()? as usize;
        let mut dims = Vec::with_capacity(n_dims);
        for _ in 0..n_dims {
            dims.push(r.u64()?);
        }
        let ty = r.u32()?;
        let offset = r.u64()?;
        tensors.push(Tensor {
            name,
            dims,
            ty,
            offset,
        });
    }
    let data_start = align(r.at, alignment);

    // New types, sizes and offsets.
    let elems = |t: &Tensor| t.dims.iter().product::<u64>() as usize;
    let size = |ty: u32, n: usize| -> Result<usize, String> {
        match ty {
            TYPE_F32 => Ok(n * 4),
            TYPE_Q8_0 => Ok(n / BLOCK * Q8_0_BYTES),
            TYPE_Q4_0 => Ok(n / BLOCK * Q4_0_BYTES),
            t => Err(format!("tensor type {t} is not handled")),
        }
    };
    let convert =
        |t: &Tensor| t.ty == TYPE_Q8_0 && t.dims.len() == 2 && t.name != "token_embd.weight";
    let mut out_info = Vec::new();
    let mut offset = 0usize;
    let mut plan = Vec::with_capacity(tensors.len());
    let mut converted = 0;
    for t in &tensors {
        let n = elems(t);
        if !n.is_multiple_of(BLOCK) && t.ty != TYPE_F32 {
            return Err(format!("{} has {n} values, not whole blocks", t.name));
        }
        let new_ty = if convert(t) {
            converted += 1;
            TYPE_Q4_0
        } else {
            t.ty
        };
        let from = data_start + t.offset as usize;
        let len = size(t.ty, n)?;
        if from + len > b.len() {
            return Err(format!("{} lies past the end of the file", t.name));
        }
        offset = align(offset, alignment);
        out_info.extend_from_slice(&(t.name.len() as u64).to_le_bytes());
        out_info.extend_from_slice(t.name.as_bytes());
        out_info.extend_from_slice(&(t.dims.len() as u32).to_le_bytes());
        for d in &t.dims {
            out_info.extend_from_slice(&d.to_le_bytes());
        }
        out_info.extend_from_slice(&new_ty.to_le_bytes());
        out_info.extend_from_slice(&(offset as u64).to_le_bytes());
        plan.push((from, len, new_ty, offset, n));
        offset += size(new_ty, n)?;
    }

    let mut out = Vec::with_capacity(b.len());
    out.extend_from_slice(&b[..meta_end]);
    out.extend_from_slice(&out_info);
    out.resize(align(out.len(), alignment), 0);
    let base = out.len();
    for (t, &(from, len, new_ty, at, n)) in tensors.iter().zip(&plan) {
        out.resize(base + at, 0);
        let src = &b[from..from + len];
        if new_ty == t.ty {
            out.extend_from_slice(src);
        } else {
            let mut q4 = vec![0u8; n / BLOCK * Q4_0_BYTES];
            let (q4_blocks, _) = q4.as_chunks_mut::<Q4_0_BYTES>();
            for (q8, o) in src.as_chunks::<Q8_0_BYTES>().0.iter().zip(q4_blocks) {
                block_q8_to_q4(q8, o);
            }
            out.extend_from_slice(&q4);
        }
    }
    std::fs::write(output, &out).map_err(|e| format!("cannot write {}: {e}", output.display()))?;
    use sha2::{Digest, Sha256};
    let hash: String = Sha256::digest(&out)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let name = output
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    std::fs::write(
        output.with_extension("gguf.sha256"),
        format!("{hash}  {name}\n"),
    )
    .map_err(|e| format!("cannot write the .sha256 file: {e}"))?;
    println!(
        "[requant] {} -> {}: {converted} of {} tensors Q8_0 -> Q4_0, {} -> {} bytes, SHA-256 {hash}",
        input.display(),
        output.display(),
        tensors.len(),
        b.len(),
        out.len()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn half_round_trip() {
        for x in [0.0f32, 1.0, -2.5, 0.000_123, 65504.0, -0.333_333] {
            let h = f32_to_f16(x);
            let back = zoo_infer::quant::f16_to_f32(h);
            assert!((back - x).abs() <= x.abs() / 1024.0 + 1e-7, "{x} -> {back}");
        }
    }

    #[test]
    fn q4_block_close_to_q8() {
        // A Q8_0 block of a ramp, scale 0.01.
        let mut q8 = vec![0u8; Q8_0_BYTES];
        q8[..2].copy_from_slice(&f32_to_f16(0.01).to_le_bytes());
        for j in 0..BLOCK {
            q8[2 + j] = ((j as i32 * 8 - 128) as i8) as u8;
        }
        let mut q4 = vec![0u8; Q4_0_BYTES];
        block_q8_to_q4(&q8, &mut q4);
        let d4 = zoo_infer::quant::f16_to_f32(u16::from_le_bytes([q4[0], q4[1]]));
        for j in 0..BLOCK {
            let x = 0.01 * f32::from((j as i32 * 8 - 128) as i8);
            let nib = if j < 16 {
                q4[2 + j] & 15
            } else {
                q4[2 + j - 16] >> 4
            };
            let y = d4 * (f32::from(nib) - 8.0);
            // Q4_0 step is |d|; rounding is within half a step (ties included).
            assert!((x - y).abs() <= d4.abs() * 0.51 + 1e-3, "{j}: {x} vs {y}");
        }
    }
}
