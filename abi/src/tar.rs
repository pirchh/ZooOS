//! The initial filesystem: a read-only ustar archive (docs/m3-design.md §8).
//! The reader validates every header before trusting it; the writer (used by
//! xtask on the host) produces exactly what the reader accepts.
//!
//! Regular files and hard links (docs/linux10-design.md). A hard link names
//! an earlier regular file and reads as that file's very bytes, so both
//! names have one identity (Linux programs tell files apart by address).

pub const BLOCK: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TarError {
    Truncated,
    BadMagic,
    BadChecksum,
    BadSize,
    BadName,
    UnsupportedType,
    /// A hard link whose target is not an earlier regular file.
    BadLink,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry<'a> {
    pub name: &'a str,
    pub data: &'a [u8],
}

fn octal(field: &[u8]) -> Option<u64> {
    let mut v: u64 = 0;
    let mut seen = false;
    for &b in field {
        match b {
            b'0'..=b'7' => {
                v = v.checked_mul(8)?.checked_add((b - b'0') as u64)?;
                seen = true;
            }
            b' ' | 0 if seen => break,
            b' ' => {}
            _ => return None,
        }
    }
    seen.then_some(v)
}

fn checksum(header: &[u8]) -> u64 {
    header
        .iter()
        .enumerate()
        .map(|(i, &b)| {
            if (148..156).contains(&i) {
                b' ' as u64
            } else {
                b as u64
            }
        })
        .sum()
}

/// Iterates over regular files. Stops at the end-of-archive marker (a zero
/// block) or the end of the data; any malformed header is an error.
pub struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

pub fn read(data: &[u8]) -> Reader<'_> {
    Reader { data, pos: 0 }
}

impl<'a> Iterator for Reader<'a> {
    type Item = Result<Entry<'a>, TarError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.pos >= self.data.len() {
            return None;
        }
        let r = self.next_entry();
        if r.is_err() {
            self.pos = self.data.len(); // stop after the first error
        }
        r.transpose()
    }
}

impl<'a> Reader<'a> {
    fn next_entry(&mut self) -> Result<Option<Entry<'a>>, TarError> {
        let header = self
            .data
            .get(self.pos..self.pos + BLOCK)
            .ok_or(TarError::Truncated)?;
        if header.iter().all(|&b| b == 0) {
            self.pos = self.data.len();
            return Ok(None);
        }
        if &header[257..262] != b"ustar" {
            return Err(TarError::BadMagic);
        }
        if octal(&header[148..156]) != Some(checksum(header)) {
            return Err(TarError::BadChecksum);
        }
        let size = octal(&header[124..136]).ok_or(TarError::BadSize)?;
        let size = usize::try_from(size).map_err(|_| TarError::BadSize)?;
        let name_len = header[..100].iter().position(|&b| b == 0).unwrap_or(100);
        let name = core::str::from_utf8(&header[..name_len]).map_err(|_| TarError::BadName)?;
        if name.is_empty() {
            return Err(TarError::BadName);
        }
        if header[156] == b'1' {
            // A hard link: no data of its own; the earlier file's bytes.
            let target_len = header[157..257].iter().position(|&b| b == 0).unwrap_or(100);
            let target = core::str::from_utf8(&header[157..157 + target_len])
                .map_err(|_| TarError::BadLink)?;
            let earlier = &self.data[..self.pos];
            let mut found = None;
            for e in read(earlier) {
                let e = e.map_err(|_| TarError::BadLink)?;
                if e.name == target {
                    found = Some(e.data);
                }
            }
            let data = found.ok_or(TarError::BadLink)?;
            self.pos += BLOCK;
            return Ok(Some(Entry { name, data }));
        }
        if !matches!(header[156], b'0' | 0) {
            return Err(TarError::UnsupportedType);
        }
        let start = self.pos + BLOCK;
        let end = start.checked_add(size).ok_or(TarError::BadSize)?;
        let data = self.data.get(start..end).ok_or(TarError::Truncated)?;
        self.pos = start + size.div_ceil(BLOCK) * BLOCK;
        Ok(Some(Entry { name, data }))
    }
}

/// Looks up a regular file by exact name. Malformed archives are errors.
pub fn find<'a>(archive: &'a [u8], name: &str) -> Result<Option<&'a [u8]>, TarError> {
    for e in read(archive) {
        let e = e?;
        if e.name == name {
            return Ok(Some(e.data));
        }
    }
    Ok(None)
}

/// Builds the ustar header block of a hard link `name` to the earlier file
/// `target` (host-side helper).
pub fn link_header(name: &str, target: &str) -> Option<[u8; BLOCK]> {
    if target.is_empty() || target.len() >= 100 || !target.is_ascii() {
        return None;
    }
    let mut h = header(name, 0)?;
    h[156] = b'1';
    h[157..157 + target.len()].copy_from_slice(target.as_bytes());
    // The checksum covers the type and target: compute it again.
    h[148..156].copy_from_slice(b"        ");
    let sum = checksum(&h);
    let digits = [
        b'0' + ((sum >> 15) & 7) as u8,
        b'0' + ((sum >> 12) & 7) as u8,
        b'0' + ((sum >> 9) & 7) as u8,
        b'0' + ((sum >> 6) & 7) as u8,
        b'0' + ((sum >> 3) & 7) as u8,
        b'0' + (sum & 7) as u8,
    ];
    h[148..154].copy_from_slice(&digits);
    h[154] = 0;
    h[155] = b' ';
    Some(h)
}

/// Builds one ustar header block for a regular file (host-side helper).
pub fn header(name: &str, size: usize) -> Option<[u8; BLOCK]> {
    if name.is_empty() || name.len() >= 100 || !name.is_ascii() {
        return None;
    }
    let mut h = [0u8; BLOCK];
    h[..name.len()].copy_from_slice(name.as_bytes());
    let put_octal = |h: &mut [u8; BLOCK], at: usize, width: usize, v: u64| {
        // width-1 octal digits, then NUL, zero-padded on the left.
        let mut v = v;
        for i in (0..width - 1).rev() {
            h[at + i] = b'0' + (v & 7) as u8;
            v >>= 3;
        }
        h[at + width - 1] = 0;
        v == 0
    };
    put_octal(&mut h, 100, 8, 0o444); // mode
    put_octal(&mut h, 108, 8, 0); // uid
    put_octal(&mut h, 116, 8, 0); // gid
    if !put_octal(&mut h, 124, 12, size as u64) {
        return None;
    }
    put_octal(&mut h, 136, 12, 0); // mtime: fixed, for reproducible archives
    h[156] = b'0';
    h[257..263].copy_from_slice(b"ustar\0");
    h[263..265].copy_from_slice(b"00");
    let sum = checksum(&h);
    put_octal(&mut h, 148, 7, sum);
    h[155] = b' ';
    Some(h)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn archive(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        for (name, data) in files {
            out.extend_from_slice(&header(name, data.len()).unwrap());
            out.extend_from_slice(data);
            out.resize(out.len().div_ceil(BLOCK) * BLOCK, 0);
        }
        out.extend_from_slice(&[0; 2 * BLOCK]);
        out
    }

    #[test]
    fn round_trip() {
        let a = archive(&[("bin/a", b"hello"), ("bin/b", &[7u8; 1000])]);
        let names: Vec<_> = read(&a).map(|e| e.unwrap().name).collect();
        assert_eq!(names, ["bin/a", "bin/b"]);
        assert_eq!(find(&a, "bin/a").unwrap(), Some(&b"hello"[..]));
        assert_eq!(find(&a, "bin/b").unwrap().unwrap().len(), 1000);
        assert_eq!(find(&a, "bin/c").unwrap(), None);
    }

    #[test]
    fn rejects_corruption() {
        let a = archive(&[("bin/a", b"hello")]);
        let mut bad = a.clone();
        bad[0] = b'X'; // name change breaks the checksum
        assert_eq!(read(&bad).next(), Some(Err(TarError::BadChecksum)));
        let mut bad = a.clone();
        bad[257] = b'x';
        assert_eq!(read(&bad).next(), Some(Err(TarError::BadMagic)));
        let mut bad = a.clone();
        // Claim a huge size, fix the checksum: data runs past the end.
        bad[124..136].copy_from_slice(b"77777777777\0");
        let sum = checksum(&bad[..BLOCK]);
        let digits = format!("{sum:06o}\0 ");
        bad[148..156].copy_from_slice(digits.as_bytes());
        assert_eq!(read(&bad).next(), Some(Err(TarError::Truncated)));
        assert_eq!(read(&a[..100]).next(), Some(Err(TarError::Truncated)));
    }

    #[test]
    fn hard_links_read_as_their_target() {
        let mut a = Vec::new();
        a.extend_from_slice(&header("lib/ld.so", 5).unwrap());
        a.extend_from_slice(b"hello");
        a.resize(a.len().div_ceil(BLOCK) * BLOCK, 0);
        a.extend_from_slice(&link_header("lib/libc.so", "lib/ld.so").unwrap());
        a.extend_from_slice(&[0; 2 * BLOCK]);
        let entries: Vec<_> = read(&a).map(|e| e.unwrap()).collect();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[1].name, "lib/libc.so");
        // The very same bytes, not a copy.
        assert_eq!(entries[1].data.as_ptr(), entries[0].data.as_ptr());
        assert_eq!(find(&a, "lib/libc.so").unwrap(), Some(&b"hello"[..]));
        // A link to a missing or later file is malformed.
        let mut bad = Vec::new();
        bad.extend_from_slice(&link_header("lib/libc.so", "lib/ld.so").unwrap());
        bad.extend_from_slice(&[0; 2 * BLOCK]);
        assert_eq!(read(&bad).next(), Some(Err(TarError::BadLink)));
    }

    #[test]
    fn never_panics_on_truncations() {
        let a = archive(&[("bin/a", b"hello"), ("bin/b", b"world")]);
        for len in 0..a.len() {
            for e in read(&a[..len]) {
                let _ = e;
            }
        }
    }
}
