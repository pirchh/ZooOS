//! The UI font atlas (docs/u1-design.md): glyph bitmaps rendered at build
//! time by `xtask` from Inter, read here by the window server and by the
//! screenshot checks. Both place glyphs with `Face::draw`, so a check can
//! reproduce the window server's pixels exactly.
//!
//! Layout (little-endian):
//! - header: `ZFNT`, version (u16), face count (u16), then one u32 offset
//!   per face;
//! - face: pixel size (u16), ascent (i16), descent (i16, negative), line
//!   height (i16), glyph count (u32), kerning-pair count (u32), bitmap
//!   length (u32); glyph records; kerning records; bitmaps;
//! - glyph record (16 bytes): code point (u32), advance in 1/64 px (i16),
//!   left edge (i16), bottom edge above the baseline (i16), width (u8),
//!   height (u8), bitmap offset (u32); records are sorted by code point;
//! - kerning record (12 bytes): left (u32), right (u32), adjustment in 1/64
//!   px (i16), unused (i16); sorted by (left, right);
//! - a bitmap is `width * height` coverage bytes, row by row.

/// File magic.
pub const MAGIC: &[u8; 4] = b"ZFNT";
pub const VERSION: u16 = 1;
const FACE_HEADER: usize = 20;
const GLYPH: usize = 16;
const KERN: usize = 12;

/// The faces in the atlas, in order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FaceId {
    /// Body text: Inter Regular, 13 px.
    Regular13 = 0,
    /// Titles: Inter Medium, 13 px.
    Medium13 = 1,
    /// The focused application's name in the menu bar: SemiBold, 13 px.
    SemiBold13 = 2,
    /// Small labels: Regular, 11 px.
    Regular11 = 3,
}

/// Faces `xtask` puts in the atlas: (face, file, pixel size).
pub const FACES: [(FaceId, &str, u16); 4] = [
    (FaceId::Regular13, "Inter-Regular.ttf", 13),
    (FaceId::Medium13, "Inter-Medium.ttf", 13),
    (FaceId::SemiBold13, "Inter-SemiBold.ttf", 13),
    (FaceId::Regular11, "Inter-Regular.ttf", 11),
];

fn u16_at(d: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(d.get(at..at + 2)?.try_into().ok()?))
}
fn i16_at(d: &[u8], at: usize) -> Option<i16> {
    Some(i16::from_le_bytes(d.get(at..at + 2)?.try_into().ok()?))
}
fn u32_at(d: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(at..at + 4)?.try_into().ok()?))
}

#[derive(Clone, Copy)]
pub struct Atlas<'a> {
    data: &'a [u8],
}

impl<'a> Atlas<'a> {
    /// Checks the header and every face's tables.
    pub fn parse(data: &'a [u8]) -> Option<Atlas<'a>> {
        if data.get(..4)? != MAGIC || u16_at(data, 4)? != VERSION {
            return None;
        }
        let a = Atlas { data };
        for k in 0..u16_at(data, 6)? as usize {
            a.face_at(k)?;
        }
        Some(a)
    }

    fn face_at(&self, k: usize) -> Option<Face<'a>> {
        let n = u16_at(self.data, 6)? as usize;
        if k >= n {
            return None;
        }
        let at = u32_at(self.data, 8 + 4 * k)? as usize;
        let d = self.data.get(at..)?;
        let glyphs = u32_at(d, 8)? as usize;
        let kerns = u32_at(d, 12)? as usize;
        let bitmaps = u32_at(d, 16)? as usize;
        let g0 = FACE_HEADER;
        let k0 = g0 + glyphs.checked_mul(GLYPH)?;
        let b0 = k0 + kerns.checked_mul(KERN)?;
        let face = Face {
            size: u16_at(d, 0)?,
            ascent: i16_at(d, 2)?,
            descent: i16_at(d, 4)?,
            line_height: i16_at(d, 6)?,
            glyphs: d.get(g0..k0)?,
            kerns: d.get(k0..b0)?,
            bitmaps: d.get(b0..b0.checked_add(bitmaps)?)?,
        };
        // Every bitmap lies inside the bitmap area.
        for g in 0..glyphs {
            let r = &face.glyphs[g * GLYPH..];
            let (w, h) = (r[10] as usize, r[11] as usize);
            let off = u32_at(r, 12)? as usize;
            face.bitmaps.get(off..off.checked_add(w * h)?)?;
        }
        Some(face)
    }

    pub fn face(&self, id: FaceId) -> Option<Face<'a>> {
        self.face_at(id as usize)
    }
}

#[derive(Clone, Copy)]
pub struct Face<'a> {
    pub size: u16,
    pub ascent: i16,
    pub descent: i16,
    pub line_height: i16,
    glyphs: &'a [u8],
    kerns: &'a [u8],
    bitmaps: &'a [u8],
}

/// One glyph: advance in 1/64 px, bitmap placement relative to the pen
/// (left edge, bottom edge above the baseline), size and coverage.
#[derive(Clone, Copy)]
pub struct Glyph<'a> {
    pub advance: i32,
    pub left: i32,
    pub bottom: i32,
    pub width: usize,
    pub height: usize,
    pub coverage: &'a [u8],
}

impl<'a> Face<'a> {
    fn record(&self, k: usize) -> &'a [u8] {
        &self.glyphs[k * GLYPH..(k + 1) * GLYPH]
    }

    /// The glyph for `c`, or `?`'s when the font has none.
    pub fn glyph(&self, c: char) -> Option<Glyph<'a>> {
        let n = self.glyphs.len() / GLYPH;
        let find = |cp: u32| {
            let (mut lo, mut hi) = (0, n);
            while lo < hi {
                let mid = (lo + hi) / 2;
                match u32_at(self.record(mid), 0)?.cmp(&cp) {
                    core::cmp::Ordering::Less => lo = mid + 1,
                    core::cmp::Ordering::Greater => hi = mid,
                    core::cmp::Ordering::Equal => return Some(mid),
                }
            }
            None
        };
        let k = find(c as u32).or_else(|| find('?' as u32))?;
        let r = self.record(k);
        let (w, h) = (r[10] as usize, r[11] as usize);
        let off = u32_at(r, 12)? as usize;
        Some(Glyph {
            advance: i32::from(i16_at(r, 4)?),
            left: i32::from(i16_at(r, 6)?),
            bottom: i32::from(i16_at(r, 8)?),
            width: w,
            height: h,
            coverage: self.bitmaps.get(off..off + w * h)?,
        })
    }

    /// Kerning between `l` and `r`, in 1/64 px.
    pub fn kern(&self, l: char, r: char) -> i32 {
        let n = self.kerns.len() / KERN;
        let key = (l as u32, r as u32);
        let (mut lo, mut hi) = (0, n);
        while lo < hi {
            let mid = (lo + hi) / 2;
            let rec = &self.kerns[mid * KERN..];
            let k = (u32_at(rec, 0).unwrap_or(0), u32_at(rec, 4).unwrap_or(0));
            match k.cmp(&key) {
                core::cmp::Ordering::Less => lo = mid + 1,
                core::cmp::Ordering::Greater => hi = mid,
                core::cmp::Ordering::Equal => return i32::from(i16_at(rec, 8).unwrap_or(0)),
            }
        }
        0
    }

    /// The width of `text` in pixels.
    pub fn width(&self, text: &str) -> usize {
        let mut pen = 0i32;
        let mut prev = None;
        for c in text.chars() {
            if let Some(p) = prev {
                pen += self.kern(p, c);
            }
            pen += self.glyph(c).map_or(0, |g| g.advance);
            prev = Some(c);
        }
        ((pen + 32) >> 6).max(0) as usize
    }

    /// Lays out `text` with the pen starting at `x` on baseline `y` and
    /// calls `plot(x, y, coverage)` for every covered pixel, stopping at
    /// characters whose right edge would pass `max_x`. Returns where the
    /// pen ended, in pixels.
    pub fn draw(
        &self,
        text: &str,
        x: i32,
        y: i32,
        max_x: i32,
        mut plot: impl FnMut(i32, i32, u8),
    ) -> i32 {
        let mut pen = x << 6;
        let mut prev = None;
        for c in text.chars() {
            if let Some(p) = prev {
                pen += self.kern(p, c);
            }
            prev = Some(c);
            let Some(g) = self.glyph(c) else { continue };
            let gx = ((pen + 32) >> 6) + g.left;
            if ((pen + g.advance + 32) >> 6) > max_x {
                break;
            }
            let top = y - g.bottom - g.height as i32;
            for row in 0..g.height {
                for col in 0..g.width {
                    let cov = g.coverage[row * g.width + col];
                    if cov != 0 {
                        plot(gx + col as i32, top + row as i32, cov);
                    }
                }
            }
            pen += g.advance;
        }
        (pen + 32) >> 6
    }
}

/// Blends `fg` over `bg` by `coverage` (0..=255), per channel, rounded:
/// the formula the window server and the screenshot checks share.
pub fn blend(bg: u32, fg: u32, coverage: u8) -> u32 {
    let i = u32::from(coverage);
    let ch = |shift: u32| {
        let b = (bg >> shift) & 0xFF;
        let f = (fg >> shift) & 0xFF;
        ((b * (255 - i) + f * i + 127) / 255) << shift
    };
    ch(16) | ch(8) | ch(0)
}
