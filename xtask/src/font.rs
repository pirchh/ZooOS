//! Renders the UI font atlas (docs/u1-design.md) from Inter's TTF files in
//! tools/fonts/inter-4.1/ (tools/fonts/README.md): every face in
//! `zoo_abi::font::FACES` and the characters in `chars()`, with kerning
//! from a legacy `kern` table. Inter 4 keeps its kerning in `GPOS`, which
//! fontdue does not read, so its atlas has none; Inter's spacing is made to
//! read well without it. The OS only copies the bitmaps; no font
//! engine runs in it.

use std::path::Path;
use zoo_abi::font::{FACES, MAGIC, VERSION};

/// Inter's release whose files are used.
pub const FONT_DIR: &str = "inter-4.1";

/// Characters beyond printable ASCII that the UI uses.
const EXTRA: &str = "…•·—–‘’“”✓×→←↑↓▸▾°©⌘⇧⌥⌃⏎⌫";

/// Coverage is raised to this power (by 1/100) before storing: a little
/// heavier strokes, as screens show text with gamma (1.0 = linear).
const CONTRAST: f32 = 0.82;

fn chars() -> Vec<char> {
    let mut v: Vec<char> = (' '..='~').chain(EXTRA.chars()).collect();
    v.sort_unstable();
    v.dedup();
    v
}

/// The atlas bytes, or why it cannot be made.
pub fn build_atlas(root: &Path) -> Result<Vec<u8>, String> {
    let dir = root.join("tools").join("fonts").join(FONT_DIR);
    let mut faces = Vec::new();
    for (_, file, size) in FACES {
        let path = dir.join(file);
        let bytes = std::fs::read(&path).map_err(|e| {
            format!(
                "cannot read {}: {e} (download Inter 4.1, tools/fonts/README.md)",
                path.display()
            )
        })?;
        let font = fontdue::Font::from_bytes(bytes, fontdue::FontSettings::default())
            .map_err(|e| format!("{}: {e}", path.display()))?;
        faces.push(face(&font, f32::from(size)));
    }
    let mut out = Vec::new();
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&(faces.len() as u16).to_le_bytes());
    let mut at = 8 + 4 * faces.len();
    for f in &faces {
        out.extend_from_slice(&(at as u32).to_le_bytes());
        at += f.len();
    }
    for f in faces {
        out.extend_from_slice(&f);
    }
    Ok(out)
}

fn face(font: &fontdue::Font, px: f32) -> Vec<u8> {
    let lm = font
        .horizontal_line_metrics(px)
        .expect("Inter has horizontal metrics");
    let mut glyphs = Vec::new();
    let mut bitmaps = Vec::new();
    for c in chars() {
        if c != ' ' && font.lookup_glyph_index(c) == 0 {
            continue; // not in the font
        }
        let (m, cov) = font.rasterize(c, px);
        let cov: Vec<u8> = cov
            .iter()
            .map(|&v| ((f32::from(v) / 255.0).powf(CONTRAST) * 255.0).round() as u8)
            .collect();
        let off = bitmaps.len() as u32;
        bitmaps.extend_from_slice(&cov);
        let mut r = [0u8; 16];
        r[0..4].copy_from_slice(&(c as u32).to_le_bytes());
        r[4..6].copy_from_slice(&((m.advance_width * 64.0).round() as i16).to_le_bytes());
        r[6..8].copy_from_slice(&(m.xmin as i16).to_le_bytes());
        r[8..10].copy_from_slice(&(m.ymin as i16).to_le_bytes());
        r[10] = m.width as u8;
        r[11] = m.height as u8;
        r[12..16].copy_from_slice(&off.to_le_bytes());
        glyphs.push(r);
    }
    let mut kerns = Vec::new();
    for l in ' '..='~' {
        for r in ' '..='~' {
            if let Some(k) = font.horizontal_kern(l, r, px) {
                let q = (k * 64.0).round() as i16;
                if q != 0 {
                    let mut rec = [0u8; 12];
                    rec[0..4].copy_from_slice(&(l as u32).to_le_bytes());
                    rec[4..8].copy_from_slice(&(r as u32).to_le_bytes());
                    rec[8..10].copy_from_slice(&q.to_le_bytes());
                    kerns.push(rec);
                }
            }
        }
    }
    let mut out = Vec::new();
    out.extend_from_slice(&(px as u16).to_le_bytes());
    out.extend_from_slice(&(lm.ascent.round() as i16).to_le_bytes());
    out.extend_from_slice(&(lm.descent.round() as i16).to_le_bytes());
    out.extend_from_slice(&(lm.new_line_size.round() as i16).to_le_bytes());
    out.extend_from_slice(&(glyphs.len() as u32).to_le_bytes());
    out.extend_from_slice(&(kerns.len() as u32).to_le_bytes());
    out.extend_from_slice(&(bitmaps.len() as u32).to_le_bytes());
    for g in glyphs {
        out.extend_from_slice(&g);
    }
    for k in kerns {
        out.extend_from_slice(&k);
    }
    out.extend_from_slice(&bitmaps);
    out
}

#[cfg(test)]
mod tests {
    use zoo_abi::font::{Atlas, FACES};

    /// The atlas made from tools/fonts/ parses, and every face lays out
    /// text (skipped without the fonts).
    #[test]
    fn atlas_parses_and_lays_out() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let Ok(bytes) = super::build_atlas(&root) else {
            eprintln!("no fonts in tools/fonts/; skipped");
            return;
        };
        let atlas = Atlas::parse(&bytes).expect("the atlas parses");
        for (id, _, size) in FACES {
            let face = atlas.face(id).expect("every face is present");
            assert_eq!(face.size, size);
            let w = face.width("Hello, ZooOS");
            assert!(w > 4 * size as usize && w < 12 * size as usize, "width {w}");
            let mut lit = 0;
            face.draw("Hello", 0, 20, 1000, |_, _, _| lit += 1);
            assert!(lit > 20);
        }
    }
}
