//! HID report descriptors (docs/u1c-design.md): just enough to find a
//! pointing device's buttons and X/Y in its input reports. Used for HID
//! pointers that have no boot protocol, such as tablets (absolute X/Y).
//!
//! Items are walked in order. Global state (usage page, logical range,
//! report size, count and ID) carries over; local state (usages) clears
//! after each main item. Input fields advance a bit offset kept per report
//! ID. Push/pop of global state, delimiters and long items are skipped.

/// One field of an input report.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct Field {
    /// First bit, counted from the start of the report after its ID byte.
    pub bit: u16,
    pub size: u8,
    pub min: i32,
    pub max: i32,
}

/// Where a pointer's state is in its input report.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pointer {
    /// The report ID (0: the device's reports carry none).
    pub id: u8,
    pub x: Field,
    pub y: Field,
    /// X and Y are positions (a tablet), not motion (a mouse).
    pub absolute: bool,
    /// The bit of buttons 1-3 (left, right, middle), if present.
    pub buttons: [Option<u16>; 3],
}

const USAGE_X: u32 = 0x0001_0030;
const USAGE_Y: u32 = 0x0001_0031;
const BUTTON_PAGE: u32 = 0x0009;
/// Usages remembered per main item; more are ignored.
const MAX_USAGES: usize = 16;

fn le(bytes: &[u8]) -> u32 {
    bytes.iter().rev().fold(0u32, |v, &b| v << 8 | u32::from(b))
}

fn sign_extend(v: u32, bytes: usize) -> i32 {
    match bytes {
        1 => v as u8 as i8 as i32,
        2 => v as u16 as i16 as i32,
        _ => v as i32,
    }
}

/// The pointer described by report descriptor `d`, if it has X and Y.
pub fn parse_pointer(d: &[u8]) -> Option<Pointer> {
    let (mut page, mut lmin, mut lmax) = (0u32, 0i32, 0i32);
    let (mut size, mut count, mut id) = (0u32, 0u32, 0u8);
    let mut usages = [0u32; MAX_USAGES];
    let mut n_usages = 0usize;
    let (mut umin, mut umax) = (None::<u32>, None::<u32>);
    // Input bits used so far, per report ID.
    let mut offsets = [0u32; 256];
    let (mut x, mut y) = (None::<(u8, Field, bool)>, None::<(u8, Field)>);
    let mut buttons = [None::<(u8, u16)>; 3];
    let mut i = 0;
    while i < d.len() {
        let b = d[i];
        if b == 0xFE {
            // A long item: data size in the next byte.
            i += 3 + usize::from(*d.get(i + 1)?);
            continue;
        }
        let n = [0, 1, 2, 4][usize::from(b & 3)];
        let data = d.get(i + 1..i + 1 + n)?;
        let raw = le(data);
        let signed = sign_extend(raw, n);
        match ((b >> 2) & 3, b >> 4) {
            (1, 0) => page = raw,
            (1, 1) => lmin = signed,
            // Unsigned unless the range starts below zero (HID 1.11 §6.2.2.7).
            (1, 2) => lmax = if lmin < 0 { signed } else { raw as i32 },
            (1, 7) => size = raw,
            (1, 8) => id = raw as u8,
            (1, 9) => count = raw,
            (2, tag @ 0..=2) => {
                let usage = if n == 4 { raw } else { page << 16 | raw };
                match tag {
                    0 if n_usages < MAX_USAGES => {
                        usages[n_usages] = usage;
                        n_usages += 1;
                    }
                    1 => umin = Some(usage),
                    2 => umax = Some(usage),
                    _ => {}
                }
            }
            (0, tag) => {
                if tag == 8 {
                    // Input: flags bit 0 constant, bit 2 relative.
                    let base = offsets[usize::from(id)];
                    for k in 0..count.min(4096) {
                        let usage = if n_usages > 0 {
                            usages[(k as usize).min(n_usages - 1)]
                        } else if let (Some(a), Some(b)) = (umin, umax) {
                            (a + k).min(b)
                        } else {
                            0
                        };
                        let bit = base + k * size;
                        if raw & 1 != 0 || bit + size > u32::from(u16::MAX) || size > 32 {
                            continue;
                        }
                        let field = Field {
                            bit: bit as u16,
                            size: size as u8,
                            min: lmin,
                            max: lmax,
                        };
                        match usage {
                            USAGE_X if x.is_none() => x = Some((id, field, raw & 4 == 0)),
                            USAGE_Y if y.is_none() => y = Some((id, field)),
                            u if u >> 16 == BUTTON_PAGE && (1..=3).contains(&(u & 0xFFFF)) => {
                                let slot = &mut buttons[(u & 0xFFFF) as usize - 1];
                                if slot.is_none() && size == 1 {
                                    *slot = Some((id, bit as u16));
                                }
                            }
                            _ => {}
                        }
                    }
                    offsets[usize::from(id)] = base.saturating_add(count.saturating_mul(size));
                }
                // Every main item ends the local state.
                n_usages = 0;
                umin = None;
                umax = None;
            }
            _ => {}
        }
        i += 1 + n;
    }
    let (id, x, absolute) = x?;
    let (yid, y) = y?;
    if yid != id || x.max <= x.min || y.max <= y.min {
        return None;
    }
    let mut out = Pointer {
        id,
        x,
        y,
        absolute,
        buttons: [None; 3],
    };
    for (k, b) in buttons.iter().enumerate() {
        if let Some((bid, bit)) = b
            && *bid == id
        {
            out.buttons[k] = Some(*bit);
        }
    }
    Some(out)
}

/// Field `f` of `report` (the bytes after any ID byte), sign-extended when
/// its range starts below zero.
pub fn read(report: &[u8], f: Field) -> Option<i32> {
    let mut v = 0u32;
    for k in 0..u32::from(f.size) {
        let bit = u32::from(f.bit) + k;
        let byte = *report.get((bit / 8) as usize)?;
        if byte >> (bit % 8) & 1 != 0 {
            v |= 1 << k;
        }
    }
    if f.min < 0 && f.size < 32 && f.size > 0 && v & (1 << (f.size - 1)) != 0 {
        v |= !0u32 << f.size;
    }
    Some(v as i32)
}

/// Bit `bit` of `report`.
pub fn bit(report: &[u8], bit: u16) -> bool {
    report
        .get(usize::from(bit / 8))
        .is_some_and(|b| b >> (bit % 8) & 1 != 0)
}
