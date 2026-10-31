//! Validating parser for the executables ZooOS loads (docs/m3-design.md §7):
//! ELF64 x86-64 `ET_EXEC` files whose loadable segments sit in a
//! caller-given address window, and (`parse_at` with a base) position-
//! independent `ET_DYN` files such as Linux's dynamic loader, placed at that
//! base (docs/linux8-design.md). Everything is bounds-checked; malformed
//! input returns an error, never panics.

pub const PAGE: u64 = 4096;
const MAX_PHDRS: usize = 16;
const PT_LOAD: u32 = 1;
const PT_INTERP: u32 = 3;
pub const PF_X: u32 = 1;
pub const PF_W: u32 = 2;
pub const PF_R: u32 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElfError {
    TooShort,
    BadMagic,
    NotElf64LittleEndian,
    BadVersion,
    NotX86_64,
    NotExecutable,
    BadProgramHeaders,
    TooManySegments,
    NoLoadableSegments,
    SegmentFileRange,
    SegmentSizes,
    SegmentAlignment,
    SegmentOutsideWindow,
    WritableAndExecutable,
    SegmentsOverlap,
    EntryNotExecutable,
    BadInterpreter,
}

/// One `PT_LOAD` segment, validated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Segment {
    pub vaddr: u64,
    pub mem_size: u64,
    pub file_offset: u64,
    pub file_size: u64,
    pub flags: u32,
}

impl Segment {
    pub fn writable(&self) -> bool {
        self.flags & PF_W != 0
    }
    pub fn executable(&self) -> bool {
        self.flags & PF_X != 0
    }
    /// First and one-past-last page addresses this segment touches.
    pub fn page_range(&self) -> (u64, u64) {
        let start = self.vaddr & !(PAGE - 1);
        let end = (self.vaddr + self.mem_size).div_ceil(PAGE) * PAGE;
        (start, end)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    pub entry: u64,
    pub segments: [Option<Segment>; MAX_PHDRS],
    /// File range of the `PT_INTERP` path (NUL included), if any.
    pub interp: Option<(u64, u64)>,
    /// What was added to every address (0 for `ET_EXEC`).
    pub bias: u64,
}

impl Image {
    pub fn loadable(&self) -> impl Iterator<Item = &Segment> {
        self.segments.iter().flatten()
    }
}

fn u16_at(b: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(o..o + 2)?.try_into().ok()?))
}
fn u32_at(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(o..o + 4)?.try_into().ok()?))
}
fn u64_at(b: &[u8], o: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(o..o + 8)?.try_into().ok()?))
}

/// Parses and validates an `ET_EXEC` `file`. Segments must lie inside
/// `[window_start, window_end)`.
pub fn parse(file: &[u8], window_start: u64, window_end: u64) -> Result<Image, ElfError> {
    parse_at(file, window_start, window_end, None)
}

/// As `parse`; with `base`, an `ET_DYN` file is accepted instead and every
/// address is moved up by `base` (which must be page-aligned).
pub fn parse_at(
    file: &[u8],
    window_start: u64,
    window_end: u64,
    base: Option<u64>,
) -> Result<Image, ElfError> {
    use ElfError::*;
    if file.len() < 64 {
        return Err(TooShort);
    }
    if file[0..4] != [0x7F, b'E', b'L', b'F'] {
        return Err(BadMagic);
    }
    if file[4] != 2 || file[5] != 1 {
        return Err(NotElf64LittleEndian);
    }
    if file[6] != 1 || u32_at(file, 20) != Some(1) {
        return Err(BadVersion);
    }
    if u16_at(file, 18) != Some(0x3E) {
        return Err(NotX86_64);
    }
    let want_type = if base.is_some() { 3 } else { 2 };
    if u16_at(file, 16) != Some(want_type) {
        return Err(NotExecutable);
    }
    let bias = base.unwrap_or(0);
    if !bias.is_multiple_of(PAGE) {
        return Err(SegmentAlignment);
    }
    let entry = u64_at(file, 24)
        .ok_or(TooShort)?
        .checked_add(bias)
        .ok_or(SegmentOutsideWindow)?;
    let phoff = u64_at(file, 32).ok_or(TooShort)?;
    let phentsize = u16_at(file, 54).ok_or(TooShort)? as u64;
    let phnum = u16_at(file, 56).ok_or(TooShort)? as u64;
    if phentsize != 56 {
        return Err(BadProgramHeaders);
    }
    let table_end = phnum
        .checked_mul(56)
        .and_then(|n| n.checked_add(phoff))
        .ok_or(BadProgramHeaders)?;
    if table_end > file.len() as u64 {
        return Err(BadProgramHeaders);
    }

    let mut segments = [None; MAX_PHDRS];
    let mut count = 0;
    let mut interp = None;
    for i in 0..phnum {
        let h = (phoff + i * 56) as usize;
        if u32_at(file, h) == Some(PT_INTERP) {
            let off = u64_at(file, h + 8).ok_or(BadProgramHeaders)?;
            let size = u64_at(file, h + 32).ok_or(BadProgramHeaders)?;
            let end = off.checked_add(size).ok_or(BadInterpreter)?;
            if !(2..=4096).contains(&size) || end > file.len() as u64 || file[end as usize - 1] != 0
            {
                return Err(BadInterpreter);
            }
            interp = Some((off, size));
            continue;
        }
        if u32_at(file, h) != Some(PT_LOAD) {
            continue;
        }
        if count == MAX_PHDRS {
            return Err(TooManySegments);
        }
        let seg = Segment {
            flags: u32_at(file, h + 4).ok_or(BadProgramHeaders)?,
            file_offset: u64_at(file, h + 8).ok_or(BadProgramHeaders)?,
            vaddr: u64_at(file, h + 16)
                .ok_or(BadProgramHeaders)?
                .checked_add(bias)
                .ok_or(SegmentOutsideWindow)?,
            file_size: u64_at(file, h + 32).ok_or(BadProgramHeaders)?,
            mem_size: u64_at(file, h + 40).ok_or(BadProgramHeaders)?,
        };
        let file_end = seg
            .file_offset
            .checked_add(seg.file_size)
            .ok_or(SegmentFileRange)?;
        if file_end > file.len() as u64 {
            return Err(SegmentFileRange);
        }
        if seg.file_size > seg.mem_size || seg.mem_size == 0 {
            return Err(SegmentSizes);
        }
        if seg.vaddr % PAGE != seg.file_offset % PAGE {
            return Err(SegmentAlignment);
        }
        let mem_end = seg
            .vaddr
            .checked_add(seg.mem_size)
            .ok_or(SegmentOutsideWindow)?;
        if seg.vaddr < window_start || mem_end > window_end {
            return Err(SegmentOutsideWindow);
        }
        if seg.writable() && seg.executable() {
            return Err(WritableAndExecutable);
        }
        let (start, end) = seg.page_range();
        for other in segments.iter().flatten() {
            let (os, oe) = Segment::page_range(other);
            if start < oe && os < end {
                return Err(SegmentsOverlap);
            }
        }
        segments[count] = Some(seg);
        count += 1;
    }
    if count == 0 {
        return Err(NoLoadableSegments);
    }
    let entry_ok = segments
        .iter()
        .flatten()
        .any(|s| s.executable() && entry >= s.vaddr && entry < s.vaddr + s.mem_size);
    if !entry_ok {
        return Err(EntryNotExecutable);
    }
    Ok(Image {
        entry,
        segments,
        interp,
        bias,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    const W0: u64 = 0x20_0000;
    const W1: u64 = 0x4000_0000;

    /// Builds a minimal valid ELF: header, `n` program headers, then data.
    /// Each segment: (flags, file_offset, vaddr, file_size, mem_size).
    pub fn build(entry: u64, segs: &[(u32, u64, u64, u64, u64)], len: usize) -> Vec<u8> {
        let mut f = vec![0u8; len.max(64 + 56 * segs.len())];
        f[0..4].copy_from_slice(&[0x7F, b'E', b'L', b'F']);
        f[4] = 2;
        f[5] = 1;
        f[6] = 1;
        f[16..18].copy_from_slice(&2u16.to_le_bytes());
        f[18..20].copy_from_slice(&0x3Eu16.to_le_bytes());
        f[20..24].copy_from_slice(&1u32.to_le_bytes());
        f[24..32].copy_from_slice(&entry.to_le_bytes());
        f[32..40].copy_from_slice(&64u64.to_le_bytes());
        f[54..56].copy_from_slice(&56u16.to_le_bytes());
        f[56..58].copy_from_slice(&(segs.len() as u16).to_le_bytes());
        for (i, &(flags, off, va, fsz, msz)) in segs.iter().enumerate() {
            let h = 64 + i * 56;
            f[h..h + 4].copy_from_slice(&PT_LOAD.to_le_bytes());
            f[h + 4..h + 8].copy_from_slice(&flags.to_le_bytes());
            f[h + 8..h + 16].copy_from_slice(&off.to_le_bytes());
            f[h + 16..h + 24].copy_from_slice(&va.to_le_bytes());
            f[h + 32..h + 40].copy_from_slice(&fsz.to_le_bytes());
            f[h + 40..h + 48].copy_from_slice(&msz.to_le_bytes());
        }
        f
    }

    fn good() -> Vec<u8> {
        build(
            0x20_1000,
            &[
                (PF_R, 0, 0x20_0000, 0x100, 0x100),
                (PF_R | PF_X, 0x1000, 0x20_1000, 0x20, 0x20),
                (PF_R | PF_W, 0x2000, 0x20_2000, 0x8, 0x1000),
            ],
            0x2008,
        )
    }

    #[test]
    fn valid_image_parses() {
        let img = parse(&good(), W0, W1).unwrap();
        assert_eq!(img.entry, 0x20_1000);
        assert_eq!(img.loadable().count(), 3);
    }

    #[test]
    fn rejects_malformed_headers() {
        assert_eq!(parse(&[0; 10], W0, W1), Err(ElfError::TooShort));
        let mut f = good();
        f[0] = 0;
        assert_eq!(parse(&f, W0, W1), Err(ElfError::BadMagic));
        let mut f = good();
        f[4] = 1;
        assert_eq!(parse(&f, W0, W1), Err(ElfError::NotElf64LittleEndian));
        let mut f = good();
        f[18] = 0x28;
        assert_eq!(parse(&f, W0, W1), Err(ElfError::NotX86_64));
        let mut f = good();
        f[16] = 3; // ET_DYN
        assert_eq!(parse(&f, W0, W1), Err(ElfError::NotExecutable));
        let mut f = good();
        f[56..58].copy_from_slice(&5000u16.to_le_bytes()); // table past end
        assert_eq!(parse(&f, W0, W1), Err(ElfError::BadProgramHeaders));
        let mut f = good();
        f[32..40].copy_from_slice(&u64::MAX.to_le_bytes()); // phoff overflow
        assert_eq!(parse(&f, W0, W1), Err(ElfError::BadProgramHeaders));
    }

    #[test]
    fn rejects_bad_segments() {
        let one = |seg, entry| build(entry, &[seg], 0x3000);
        assert_eq!(
            parse(
                &one((PF_R | PF_X, 0x1000, 0x20_1000, 0x5000, 0x5000), 0x20_1000),
                W0,
                W1
            ),
            Err(ElfError::SegmentFileRange)
        );
        assert_eq!(
            parse(
                &one((PF_R | PF_X, 0x1000, 0x20_1000, 0x20, 0x10), 0x20_1000),
                W0,
                W1
            ),
            Err(ElfError::SegmentSizes)
        );
        assert_eq!(
            parse(
                &one((PF_R | PF_X, 0x1000, 0x20_1008, 0x20, 0x20), 0x20_1008),
                W0,
                W1
            ),
            Err(ElfError::SegmentAlignment)
        );
        assert_eq!(
            parse(
                &one((PF_R | PF_X, 0x1000, 0x1000, 0x20, 0x20), 0x1000),
                W0,
                W1
            ),
            Err(ElfError::SegmentOutsideWindow)
        );
        assert_eq!(
            parse(
                &one(
                    (PF_R | PF_X, 0x1000, 0xFFFF_C000_0000_1000, 0x20, 0x20),
                    0xFFFF_C000_0000_1000
                ),
                W0,
                W1
            ),
            Err(ElfError::SegmentOutsideWindow)
        );
        assert_eq!(
            parse(
                &one(
                    (PF_R | PF_X, 0x1000, u64::MAX - 0xFFF, 0x20, 0x2000),
                    0x20_1000
                ),
                W0,
                W1
            ),
            Err(ElfError::SegmentOutsideWindow)
        );
        assert_eq!(
            parse(
                &one(
                    (PF_R | PF_W | PF_X, 0x1000, 0x20_1000, 0x20, 0x20),
                    0x20_1000
                ),
                W0,
                W1
            ),
            Err(ElfError::WritableAndExecutable)
        );
        assert_eq!(
            parse(
                &one((PF_R, 0x1000, 0x20_1000, 0x20, 0x20), 0x20_1000),
                W0,
                W1
            ),
            Err(ElfError::EntryNotExecutable)
        );
        let overlapping = build(
            0x20_1000,
            &[
                (PF_R | PF_X, 0x1000, 0x20_1000, 0x20, 0x20),
                (PF_R | PF_W, 0x1100, 0x20_1100, 0x8, 0x8),
            ],
            0x2000,
        );
        assert_eq!(parse(&overlapping, W0, W1), Err(ElfError::SegmentsOverlap));
        let none = build(0x20_1000, &[], 0x100);
        assert_eq!(parse(&none, W0, W1), Err(ElfError::NoLoadableSegments));
    }

    #[test]
    fn position_independent_files_move_to_their_base() {
        let mut f = build(
            0x1000,
            &[
                (PF_R, 0, 0, 0x100, 0x100),
                (PF_R | PF_X, 0x1000, 0x1000, 0x20, 0x20),
            ],
            0x1020,
        );
        f[16] = 3; // ET_DYN
        assert_eq!(parse(&f, W0, W1), Err(ElfError::NotExecutable));
        let img = parse_at(&f, W0, W1, Some(0x300_0000)).unwrap();
        assert_eq!(img.entry, 0x300_1000);
        assert_eq!(img.bias, 0x300_0000);
        assert_eq!(img.loadable().next().unwrap().vaddr, 0x300_0000);
        assert_eq!(
            parse_at(&f, W0, W1, Some(0x123)),
            Err(ElfError::SegmentAlignment)
        );
    }

    #[test]
    fn interpreter_paths_are_checked() {
        let path = b"/lib/ld.so\0";
        let mut f = build(
            0x20_1000,
            &[(PF_R | PF_X, 0x1000, 0x20_1000, 0x20, 0x20)],
            0x1100,
        );
        // A PT_INTERP header in a second slot, pointing at the path.
        f[56..58].copy_from_slice(&2u16.to_le_bytes());
        let h = 64 + 56;
        f[h..h + 4].copy_from_slice(&PT_INTERP.to_le_bytes());
        f[h + 8..h + 16].copy_from_slice(&0x1080u64.to_le_bytes());
        f[h + 32..h + 40].copy_from_slice(&(path.len() as u64).to_le_bytes());
        f[0x1080..0x1080 + path.len()].copy_from_slice(path);
        let img = parse(&f, W0, W1).unwrap();
        assert_eq!(img.interp, Some((0x1080, path.len() as u64)));
        f[0x1080 + path.len() - 1] = b'x'; // no terminating NUL
        assert_eq!(parse(&f, W0, W1), Err(ElfError::BadInterpreter));
    }

    #[test]
    fn never_panics_on_truncations() {
        let f = good();
        for len in 0..f.len() {
            let _ = parse(&f[..len], W0, W1);
        }
    }
}
