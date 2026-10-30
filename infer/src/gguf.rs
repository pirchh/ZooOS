//! GGUF version 3 reader. Borrows the file bytes (in ZooOS, an `fs_map`
//! mapping), validates every offset, and exposes metadata and tensors.

use crate::quant;
use alloc::vec::Vec;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GgufError {
    Truncated,
    BadMagic,
    UnsupportedVersion,
    BadValueType,
    BadUtf8,
    Missing(&'static str),
    WrongType(&'static str),
    UnsupportedTensorType,
    TensorOutOfRange,
}

/// A metadata value. Arrays keep their raw bytes and are decoded lazily.
#[derive(Debug, Clone, Copy)]
pub enum Value<'a> {
    U32(u32),
    I32(i32),
    U64(u64),
    F32(f32),
    Bool(bool),
    Str(&'a str),
    Array { elem: u32, len: u64, raw: &'a [u8] },
    Other,
}

#[derive(Debug, Clone, Copy)]
pub struct TensorInfo<'a> {
    pub name: &'a str,
    /// Elements per row (GGUF `ne[0]`).
    pub cols: usize,
    /// Number of rows (product of the other dimensions).
    pub rows: usize,
    pub ty: u32,
    pub data: &'a [u8],
}

impl TensorInfo<'_> {
    pub fn row_bytes(&self) -> usize {
        quant::byte_size(self.ty, self.cols).unwrap_or(0)
    }

    pub fn row(&self, r: usize) -> &[u8] {
        let n = self.row_bytes();
        &self.data[r * n..(r + 1) * n]
    }
}

pub struct Gguf<'a> {
    kv: Vec<(&'a str, Value<'a>)>,
    tensors: Vec<TensorInfo<'a>>,
}

struct Cursor<'a> {
    b: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], GgufError> {
        let end = self.pos.checked_add(n).ok_or(GgufError::Truncated)?;
        let s = self.b.get(self.pos..end).ok_or(GgufError::Truncated)?;
        self.pos = end;
        Ok(s)
    }
    fn u32(&mut self) -> Result<u32, GgufError> {
        Ok(u32::from_le_bytes(
            self.take(4)?.try_into().map_err(|_| GgufError::Truncated)?,
        ))
    }
    fn u64(&mut self) -> Result<u64, GgufError> {
        Ok(u64::from_le_bytes(
            self.take(8)?.try_into().map_err(|_| GgufError::Truncated)?,
        ))
    }
    fn len(&mut self) -> Result<usize, GgufError> {
        usize::try_from(self.u64()?).map_err(|_| GgufError::Truncated)
    }
    fn str(&mut self) -> Result<&'a str, GgufError> {
        let n = self.len()?;
        core::str::from_utf8(self.take(n)?).map_err(|_| GgufError::BadUtf8)
    }
    /// Size in bytes of one fixed-size scalar of GGUF type `t`.
    fn scalar_size(t: u32) -> Option<usize> {
        match t {
            0 | 1 | 7 => Some(1),
            2 | 3 => Some(2),
            4..=6 => Some(4),
            10..=12 => Some(8),
            _ => None,
        }
    }
    fn value(&mut self, t: u32) -> Result<Value<'a>, GgufError> {
        Ok(match t {
            4 => Value::U32(self.u32()?),
            5 => Value::I32(self.u32()? as i32),
            6 => Value::F32(f32::from_bits(self.u32()?)),
            7 => Value::Bool(self.take(1)?[0] != 0),
            10 => Value::U64(self.u64()?),
            8 => Value::Str(self.str()?),
            9 => {
                let elem = self.u32()?;
                let len = self.u64()?;
                let start = self.pos;
                if elem == 8 {
                    for _ in 0..len {
                        let n = self.len()?;
                        self.take(n)?;
                    }
                } else {
                    let size = Self::scalar_size(elem).ok_or(GgufError::BadValueType)?;
                    let total = usize::try_from(len)
                        .ok()
                        .and_then(|l| l.checked_mul(size))
                        .ok_or(GgufError::Truncated)?;
                    self.take(total)?;
                }
                Value::Array {
                    elem,
                    len,
                    raw: &self.b[start..self.pos],
                }
            }
            t => {
                let size = Self::scalar_size(t).ok_or(GgufError::BadValueType)?;
                self.take(size)?;
                Value::Other
            }
        })
    }
}

impl<'a> Gguf<'a> {
    pub fn parse(file: &'a [u8]) -> Result<Gguf<'a>, GgufError> {
        let mut c = Cursor { b: file, pos: 0 };
        if c.take(4)? != b"GGUF" {
            return Err(GgufError::BadMagic);
        }
        if c.u32()? != 3 {
            return Err(GgufError::UnsupportedVersion);
        }
        let n_tensors = c.len()?;
        let n_kv = c.len()?;
        let mut kv = Vec::new();
        for _ in 0..n_kv {
            let key = c.str()?;
            let t = c.u32()?;
            kv.push((key, c.value(t)?));
        }
        let mut raw = Vec::new();
        for _ in 0..n_tensors {
            let name = c.str()?;
            let nd = c.u32()? as usize;
            if nd == 0 || nd > 4 {
                return Err(GgufError::TensorOutOfRange);
            }
            let mut dims = [1usize; 4];
            for d in dims.iter_mut().take(nd) {
                *d = c.len()?;
            }
            let ty = c.u32()?;
            let offset = c.len()?;
            raw.push((name, dims, ty, offset));
        }
        let align = match kv.iter().find(|(k, _)| *k == "general.alignment") {
            Some((_, Value::U32(a))) => *a as usize,
            _ => 32,
        };
        let data_start = c.pos.div_ceil(align) * align;
        let mut tensors = Vec::new();
        for (name, dims, ty, offset) in raw {
            let cols = dims[0];
            let rows = dims[1]
                .checked_mul(dims[2])
                .and_then(|r| r.checked_mul(dims[3]))
                .ok_or(GgufError::TensorOutOfRange)?;
            let row_bytes = quant::byte_size(ty, cols).ok_or(GgufError::UnsupportedTensorType)?;
            let len = row_bytes
                .checked_mul(rows)
                .ok_or(GgufError::TensorOutOfRange)?;
            let start = data_start
                .checked_add(offset)
                .ok_or(GgufError::TensorOutOfRange)?;
            let end = start.checked_add(len).ok_or(GgufError::TensorOutOfRange)?;
            let data = file.get(start..end).ok_or(GgufError::TensorOutOfRange)?;
            tensors.push(TensorInfo {
                name,
                cols,
                rows,
                ty,
                data,
            });
        }
        Ok(Gguf { kv, tensors })
    }

    pub fn get(&self, key: &str) -> Option<Value<'a>> {
        self.kv.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
    }

    pub fn u32(&self, key: &'static str) -> Result<u32, GgufError> {
        match self.get(key).ok_or(GgufError::Missing(key))? {
            Value::U32(v) => Ok(v),
            Value::I32(v) if v >= 0 => Ok(v as u32),
            _ => Err(GgufError::WrongType(key)),
        }
    }

    pub fn f32(&self, key: &'static str) -> Result<f32, GgufError> {
        match self.get(key).ok_or(GgufError::Missing(key))? {
            Value::F32(v) => Ok(v),
            _ => Err(GgufError::WrongType(key)),
        }
    }

    pub fn str(&self, key: &'static str) -> Result<&'a str, GgufError> {
        match self.get(key).ok_or(GgufError::Missing(key))? {
            Value::Str(s) => Ok(s),
            _ => Err(GgufError::WrongType(key)),
        }
    }

    /// A string array, decoded.
    pub fn strings(&self, key: &'static str) -> Result<Vec<&'a str>, GgufError> {
        let Value::Array { elem: 8, len, raw } = self.get(key).ok_or(GgufError::Missing(key))?
        else {
            return Err(GgufError::WrongType(key));
        };
        let mut c = Cursor { b: raw, pos: 0 };
        (0..len).map(|_| c.str()).collect()
    }

    /// An i32 array, decoded.
    pub fn i32s(&self, key: &'static str) -> Result<Vec<i32>, GgufError> {
        let Value::Array { elem: 5, raw, .. } = self.get(key).ok_or(GgufError::Missing(key))?
        else {
            return Err(GgufError::WrongType(key));
        };
        Ok(raw
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| i32::from_le_bytes(*b))
            .collect())
    }

    pub fn tensor(&self, name: &str) -> Option<&TensorInfo<'a>> {
        self.tensors.iter().find(|t| t.name == name)
    }

    pub fn tensors(&self) -> &[TensorInfo<'a>] {
        &self.tensors
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(out: &mut Vec<u8>, x: &str) {
        out.extend_from_slice(&(x.len() as u64).to_le_bytes());
        out.extend_from_slice(x.as_bytes());
    }

    /// A tiny valid file: one u32, one string array, one 2x32 F32 tensor.
    fn tiny() -> Vec<u8> {
        let mut f = b"GGUF".to_vec();
        f.extend_from_slice(&3u32.to_le_bytes());
        f.extend_from_slice(&1u64.to_le_bytes()); // tensors
        f.extend_from_slice(&2u64.to_le_bytes()); // kv
        s(&mut f, "a.n");
        f.extend_from_slice(&4u32.to_le_bytes());
        f.extend_from_slice(&7u32.to_le_bytes());
        s(&mut f, "a.words");
        f.extend_from_slice(&9u32.to_le_bytes());
        f.extend_from_slice(&8u32.to_le_bytes());
        f.extend_from_slice(&2u64.to_le_bytes());
        s(&mut f, "hi");
        s(&mut f, "zoo");
        s(&mut f, "w");
        f.extend_from_slice(&2u32.to_le_bytes());
        f.extend_from_slice(&32u64.to_le_bytes());
        f.extend_from_slice(&2u64.to_le_bytes());
        f.extend_from_slice(&quant::TYPE_F32.to_le_bytes());
        f.extend_from_slice(&0u64.to_le_bytes());
        f.resize(f.len().div_ceil(32) * 32, 0);
        for i in 0..64 {
            f.extend_from_slice(&(i as f32).to_le_bytes());
        }
        f
    }

    #[test]
    fn parses_metadata_and_tensors() {
        let f = tiny();
        let g = Gguf::parse(&f).unwrap();
        assert_eq!(g.u32("a.n"), Ok(7));
        assert_eq!(g.strings("a.words").unwrap(), ["hi", "zoo"]);
        let t = g.tensor("w").unwrap();
        assert_eq!((t.rows, t.cols), (2, 32));
        assert_eq!(
            quant::dot(t.ty, t.row(1), &[1.0; 32]),
            (32..64).sum::<i32>() as f32
        );
        assert_eq!(g.u32("missing"), Err(GgufError::Missing("missing")));
    }

    #[test]
    fn rejects_corruption_without_panicking() {
        let f = tiny();
        let mut bad = f.clone();
        bad[0] = b'X';
        assert!(matches!(Gguf::parse(&bad), Err(GgufError::BadMagic)));
        for len in 0..f.len() {
            let _ = Gguf::parse(&f[..len]);
        }
        // Tensor data cut short: the tensor no longer fits in the file.
        assert_eq!(
            Gguf::parse(&f[..f.len() - 4]).err(),
            Some(GgufError::TensorOutOfRange)
        );
    }
}
