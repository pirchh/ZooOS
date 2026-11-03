//! AVX2 and FMA dot products (docs/stage3-design.md): 256-bit versions of
//! `quant::dot_portable`, chosen at run time by `crate::cpu::avx2()`.
//! Results agree with the portable code to rounding (the sums are done in a
//! different order); `infer-check` holds both to the reference.

use crate::quant::{
    BLOCK, Q4_0_BYTES, Q6_K_BYTES, Q8_0_BYTES, QK_K, TYPE_F32, TYPE_Q4_0, TYPE_Q6_K, TYPE_Q8_0,
    q6_k_block, scale,
};
use core::arch::x86_64::*;

/// `quant::dot` with AVX2.
///
/// # Safety
/// The CPU must support AVX2 and FMA, and the OS must save YMM registers
/// (`crate::cpu::avx2()`).
#[target_feature(enable = "avx2,fma")]
pub unsafe fn dot(ty: u32, row: &[u8], x: &[f32]) -> f32 {
    match ty {
        TYPE_F32 => dot_f32(row, x),
        TYPE_Q4_0 => dot_q4_0(row, x),
        TYPE_Q8_0 => dot_q8_0(row, x),
        TYPE_Q6_K => dot_q6_k(row, x),
        _ => 0.0,
    }
}

/// Eight floats from `x[at..at + 8]` (bounds-checked).
#[target_feature(enable = "avx2,fma")]
fn load8(x: &[f32], at: usize) -> __m256 {
    let s = &x[at..at + 8];
    // SAFETY: `s` has exactly 8 readable floats; the load is unaligned.
    unsafe { _mm256_loadu_ps(s.as_ptr()) }
}

#[target_feature(enable = "avx2,fma")]
fn hsum(v: __m256) -> f32 {
    let lo = _mm256_castps256_ps128(v);
    let hi = _mm256_extractf128_ps(v, 1);
    let s = _mm_add_ps(lo, hi);
    let s = _mm_add_ps(s, _mm_movehl_ps(s, s));
    let s = _mm_add_ss(s, _mm_shuffle_ps(s, s, 1));
    _mm_cvtss_f32(s)
}

/// The low eight signed bytes of `v` as eight floats.
#[target_feature(enable = "avx2,fma")]
fn i8x8_to_f32(v: __m128i) -> __m256 {
    _mm256_cvtepi32_ps(_mm256_cvtepi8_epi32(v))
}

#[target_feature(enable = "avx2,fma")]
fn dot_f32(row: &[u8], x: &[f32]) -> f32 {
    let w = row.as_chunks::<4>().0;
    let n = w.len().min(x.len());
    let mut acc = _mm256_setzero_ps();
    let mut k = 0;
    let mut buf = [0f32; 8];
    while k + 8 <= n {
        for (j, b) in buf.iter_mut().enumerate() {
            *b = f32::from_le_bytes(w[k + j]);
        }
        acc = _mm256_fmadd_ps(load8(&buf, 0), load8(x, k), acc);
        k += 8;
    }
    let mut total = hsum(acc);
    while k < n {
        total += f32::from_le_bytes(w[k]) * x[k];
        k += 1;
    }
    total
}

#[target_feature(enable = "avx2,fma")]
fn dot_q4_0(row: &[u8], x: &[f32]) -> f32 {
    let mut acc = _mm256_setzero_ps();
    let low = _mm_set1_epi8(0x0F);
    let eight = _mm_set1_epi8(8);
    for (b, xs) in row
        .as_chunks::<Q4_0_BYTES>()
        .0
        .iter()
        .zip(x.as_chunks::<BLOCK>().0)
    {
        // SAFETY: bytes 2..18 of the block are the 16 packed nibbles.
        let q = unsafe { _mm_loadu_si128(b[2..18].as_ptr().cast()) };
        // Low nibbles are elements 0..16, high nibbles 16..32.
        let lo = _mm_sub_epi8(_mm_and_si128(q, low), eight);
        let hi = _mm_sub_epi8(_mm_and_si128(_mm_srli_epi16(q, 4), low), eight);
        let mut s = _mm256_mul_ps(i8x8_to_f32(lo), load8(xs, 0));
        s = _mm256_fmadd_ps(i8x8_to_f32(_mm_srli_si128(lo, 8)), load8(xs, 8), s);
        s = _mm256_fmadd_ps(i8x8_to_f32(hi), load8(xs, 16), s);
        s = _mm256_fmadd_ps(i8x8_to_f32(_mm_srli_si128(hi, 8)), load8(xs, 24), s);
        acc = _mm256_fmadd_ps(_mm256_set1_ps(scale(b)), s, acc);
    }
    hsum(acc)
}

#[target_feature(enable = "avx2,fma")]
fn dot_q8_0(row: &[u8], x: &[f32]) -> f32 {
    let mut acc = _mm256_setzero_ps();
    for (b, xs) in row
        .as_chunks::<Q8_0_BYTES>()
        .0
        .iter()
        .zip(x.as_chunks::<BLOCK>().0)
    {
        // SAFETY: bytes 2..34 of the block are the 32 signed weights.
        let (q0, q1) = unsafe {
            (
                _mm_loadu_si128(b[2..18].as_ptr().cast()),
                _mm_loadu_si128(b[18..34].as_ptr().cast()),
            )
        };
        let mut s = _mm256_mul_ps(i8x8_to_f32(q0), load8(xs, 0));
        s = _mm256_fmadd_ps(i8x8_to_f32(_mm_srli_si128(q0, 8)), load8(xs, 8), s);
        s = _mm256_fmadd_ps(i8x8_to_f32(q1), load8(xs, 16), s);
        s = _mm256_fmadd_ps(i8x8_to_f32(_mm_srli_si128(q1, 8)), load8(xs, 24), s);
        acc = _mm256_fmadd_ps(_mm256_set1_ps(scale(b)), s, acc);
    }
    hsum(acc)
}

#[target_feature(enable = "avx2,fma")]
fn dot_q6_k(row: &[u8], x: &[f32]) -> f32 {
    let mut acc = _mm256_setzero_ps();
    let mut vals = [0f32; QK_K];
    for (b, xs) in row
        .as_chunks::<Q6_K_BYTES>()
        .0
        .iter()
        .zip(x.as_chunks::<QK_K>().0)
    {
        q6_k_block(b, &mut vals);
        for k in (0..QK_K).step_by(8) {
            acc = _mm256_fmadd_ps(load8(&vals, k), load8(xs, k), acc);
        }
    }
    hsum(acc)
}

#[cfg(test)]
mod tests {
    use crate::quant::{TYPE_F32, TYPE_Q4_0, TYPE_Q6_K, TYPE_Q8_0, dot_portable};

    /// A deterministic pseudo-random byte stream.
    fn bytes(n: usize, seed: u32) -> Vec<u8> {
        let mut s = seed;
        (0..n)
            .map(|_| {
                s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (s >> 24) as u8
            })
            .collect()
    }

    /// Valid blocks: random bytes with each block's f16 scale set to a
    /// small finite value.
    fn row(ty: u32, n: usize) -> Vec<u8> {
        let (size, at) = match ty {
            TYPE_Q4_0 => (18, 0),
            TYPE_Q8_0 => (34, 0),
            TYPE_Q6_K => (210, 208),
            _ => unreachable!(),
        };
        let mut r = bytes(size * n, ty * 7 + 1);
        for b in r.chunks_mut(size) {
            b[at..at + 2].copy_from_slice(&0x2E66u16.to_le_bytes()); // about 0.1
        }
        r
    }

    #[test]
    fn avx2_agrees_with_portable() {
        if !crate::cpu::avx2() {
            return; // no AVX2 on this machine: nothing to compare
        }
        let x: Vec<f32> = (0..1024)
            .map(|i| ((i * 37) % 101) as f32 / 50.0 - 1.0)
            .collect();
        for (ty, n) in [(TYPE_Q4_0, 32), (TYPE_Q8_0, 32), (TYPE_Q6_K, 4)] {
            let r = row(ty, n);
            let want = dot_portable(ty, &r, &x);
            // SAFETY: `avx2()` is true.
            let got = unsafe { super::dot(ty, &r, &x) };
            assert!(
                (got - want).abs() <= 1e-4 * want.abs().max(1.0),
                "type {ty}: {got} against {want}"
            );
        }
        let f: Vec<u8> = x.iter().rev().flat_map(|v| v.to_le_bytes()).collect();
        let want = dot_portable(TYPE_F32, &f, &x);
        // SAFETY: `avx2()` is true.
        let got = unsafe { super::dot(TYPE_F32, &f, &x) };
        assert!((got - want).abs() <= 1e-4 * want.abs().max(1.0));
    }
}
