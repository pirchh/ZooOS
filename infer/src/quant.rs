//! Tensor element formats used by the model: F32, Q4_0 and Q8_0 (GGML
//! block quantization, 32 values per block with one f16 scale).

use core::simd::prelude::*;

/// GGML type ids.
pub const TYPE_F32: u32 = 0;
pub const TYPE_Q4_0: u32 = 2;
pub const TYPE_Q8_0: u32 = 8;

pub const BLOCK: usize = 32;
const Q4_0_BYTES: usize = 2 + 16;
const Q8_0_BYTES: usize = 2 + 32;

/// IEEE half to single precision (exact, including subnormals and inf/NaN).
pub fn f16_to_f32(h: u16) -> f32 {
    let sign = ((h >> 15) as u32) << 31;
    let exp = ((h >> 10) & 0x1F) as u32;
    let frac = (h & 0x3FF) as u32;
    let bits = match exp {
        0 if frac == 0 => sign,
        0 => {
            // Subnormal: normalise.
            let mut e: i32 = -14;
            let mut f = frac;
            while f & 0x400 == 0 {
                f <<= 1;
                e -= 1;
            }
            sign | (((e + 127) as u32) << 23) | ((f & 0x3FF) << 13)
        }
        0x1F => sign | 0x7F80_0000 | (frac << 13),
        _ => sign | ((exp + 112) << 23) | (frac << 13),
    };
    f32::from_bits(bits)
}

/// Bytes needed for `n` elements of `ty`, or `None` for an unsupported type
/// or a length that is not a whole number of blocks.
pub fn byte_size(ty: u32, n: usize) -> Option<usize> {
    match ty {
        TYPE_F32 => n.checked_mul(4),
        TYPE_Q4_0 if n.is_multiple_of(BLOCK) => (n / BLOCK).checked_mul(Q4_0_BYTES),
        TYPE_Q8_0 if n.is_multiple_of(BLOCK) => (n / BLOCK).checked_mul(Q8_0_BYTES),
        _ => None,
    }
}

fn scale(b: &[u8]) -> f32 {
    f16_to_f32(u16::from_le_bytes([b[0], b[1]]))
}

/// Dot product of one stored row (`row` bytes of type `ty`) with `x`.
pub fn dot(ty: u32, row: &[u8], x: &[f32]) -> f32 {
    match ty {
        TYPE_F32 => row
            .as_chunks::<4>()
            .0
            .iter()
            .zip(x)
            .map(|(b, &xv)| f32::from_le_bytes(*b) * xv)
            .sum(),
        TYPE_Q4_0 => {
            let mut total = f32x16::splat(0.0);
            let blocks = row.as_chunks::<Q4_0_BYTES>().0;
            for (b, xs) in blocks.iter().zip(x.as_chunks::<BLOCK>().0) {
                let q = u8x16::from_slice(&b[2..18]);
                let eight = i8x16::splat(8);
                let lo = ((q & u8x16::splat(0x0F)).cast::<i8>() - eight).cast::<f32>();
                let hi = ((q >> 4).cast::<i8>() - eight).cast::<f32>();
                let acc = lo * f32x16::from_slice(&xs[..16]) + hi * f32x16::from_slice(&xs[16..]);
                total += acc * f32x16::splat(scale(b));
            }
            total.reduce_sum()
        }
        TYPE_Q8_0 => {
            let mut total = f32x16::splat(0.0);
            let blocks = row.as_chunks::<Q8_0_BYTES>().0;
            for (b, xs) in blocks.iter().zip(x.as_chunks::<BLOCK>().0) {
                let w0 = u8x16::from_slice(&b[2..18]).cast::<i8>().cast::<f32>();
                let w1 = u8x16::from_slice(&b[18..34]).cast::<i8>().cast::<f32>();
                let acc = w0 * f32x16::from_slice(&xs[..16]) + w1 * f32x16::from_slice(&xs[16..]);
                total += acc * f32x16::splat(scale(b));
            }
            total.reduce_sum()
        }
        _ => 0.0,
    }
}

/// Expands one stored row into `out` (same length as the row's elements).
pub fn dequantize(ty: u32, row: &[u8], out: &mut [f32]) {
    match ty {
        TYPE_F32 => {
            for (o, b) in out.iter_mut().zip(row.as_chunks::<4>().0) {
                *o = f32::from_le_bytes(*b);
            }
        }
        TYPE_Q4_0 => {
            let blocks = row.as_chunks::<Q4_0_BYTES>().0;
            for (b, os) in blocks.iter().zip(out.as_chunks_mut::<BLOCK>().0) {
                let d = scale(b);
                for j in 0..16 {
                    os[j] = ((b[2 + j] & 0x0F) as i32 - 8) as f32 * d;
                    os[j + 16] = ((b[2 + j] >> 4) as i32 - 8) as f32 * d;
                }
            }
        }
        TYPE_Q8_0 => {
            let blocks = row.as_chunks::<Q8_0_BYTES>().0;
            for (b, os) in blocks.iter().zip(out.as_chunks_mut::<BLOCK>().0) {
                let d = scale(b);
                for (o, q) in os.iter_mut().zip(&b[2..]) {
                    *o = (*q as i8) as f32 * d;
                }
            }
        }
        _ => out.fill(0.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn half_precision_values() {
        assert_eq!(f16_to_f32(0x3C00), 1.0);
        assert_eq!(f16_to_f32(0xC000), -2.0);
        assert_eq!(f16_to_f32(0x3555), 0.333_251_95);
        assert_eq!(f16_to_f32(0x0001), 5.960_464_5e-8); // smallest subnormal
        assert_eq!(f16_to_f32(0x7C00), f32::INFINITY);
        assert_eq!(f16_to_f32(0x0000), 0.0);
    }

    #[test]
    fn q4_0_block_matches_hand_computation() {
        // d = 0.5; qs[j] = low nibble j%16, high nibble 15 - j%16.
        let mut b = vec![0x00, 0x38]; // 0.5 in f16
        for j in 0..16u8 {
            b.push(j | ((15 - j) << 4));
        }
        let mut out = [0.0f32; 32];
        dequantize(TYPE_Q4_0, &b, &mut out);
        for j in 0..16 {
            assert_eq!(out[j], (j as f32 - 8.0) * 0.5);
            assert_eq!(out[j + 16], ((15 - j) as f32 - 8.0) * 0.5);
        }
        let x: Vec<f32> = (0..32).map(|i| i as f32 * 0.25 - 3.0).collect();
        let want: f32 = out.iter().zip(&x).map(|(a, b)| a * b).sum();
        assert!((dot(TYPE_Q4_0, &b, &x) - want).abs() < 1e-4);
    }

    #[test]
    fn q8_0_block_matches_hand_computation() {
        let mut b = vec![0x00, 0x3C]; // 1.0
        for j in 0..32i8 {
            b.push((j - 16) as u8);
        }
        let mut out = [0.0f32; 32];
        dequantize(TYPE_Q8_0, &b, &mut out);
        assert_eq!(out[0], -16.0);
        assert_eq!(out[31], 15.0);
        let x = [1.0f32; 32];
        assert_eq!(dot(TYPE_Q8_0, &b, &x), (-16..16).sum::<i32>() as f32);
    }

    #[test]
    fn sizes() {
        assert_eq!(byte_size(TYPE_Q4_0, 896), Some(28 * 18));
        assert_eq!(byte_size(TYPE_Q8_0, 64), Some(68));
        assert_eq!(byte_size(TYPE_Q4_0, 33), None);
        assert_eq!(byte_size(99, 32), None);
    }
}
