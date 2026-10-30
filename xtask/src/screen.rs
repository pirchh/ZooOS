//! Checks a guest screenshot by re-rendering expected text with the same
//! font and layout as kernel/src/console.rs and comparing pixels exactly.
//! The layout constants below must match the kernel's.

use noto_sans_mono_bitmap::{FontWeight, RasterHeight, get_raster, get_raster_width};
use std::path::Path;

const MARGIN: usize = 8;
const LINE_HEIGHT: usize = 18;
const WEIGHT: FontWeight = FontWeight::Regular;
const SIZE: RasterHeight = RasterHeight::Size16;
const CHAR_WIDTH: usize = get_raster_width(WEIGHT, SIZE);

/// An RGB image, 8 bits per channel.
pub struct Image {
    pub width: usize,
    pub height: usize,
    rgb: Vec<u8>,
}

impl Image {
    #[cfg(test)]
    pub fn blank(width: usize, height: usize) -> Self {
        Image {
            width,
            height,
            rgb: vec![0; width * height * 3],
        }
    }

    fn pixel(&self, x: usize, y: usize) -> Option<[u8; 3]> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let o = (y * self.width + x) * 3;
        Some([self.rgb[o], self.rgb[o + 1], self.rgb[o + 2]])
    }

    #[cfg(test)]
    fn draw_text(&mut self, text: &str, row: usize) {
        for (col, c) in text.chars().enumerate() {
            let raster = glyph(c);
            for (dy, line) in raster.raster().iter().enumerate() {
                for (dx, &i) in line.iter().enumerate() {
                    let x = MARGIN + col * CHAR_WIDTH + dx;
                    let y = MARGIN + row * LINE_HEIGHT + dy;
                    let o = (y * self.width + x) * 3;
                    self.rgb[o..o + 3].fill(i);
                }
            }
        }
    }
}

fn glyph(c: char) -> noto_sans_mono_bitmap::RasterizedChar {
    get_raster(c, WEIGHT, SIZE)
        .or_else(|| get_raster('?', WEIGHT, SIZE))
        .expect("'?' is in the font")
}

/// Reads a binary PPM (P6, maxval 255), as written by QMP `screendump`.
pub fn read_ppm(path: &Path) -> Result<Image, String> {
    let data = std::fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    // Header: magic, width, height, maxval, separated by whitespace, with
    // optional '#' comments; then one whitespace byte and the pixel data.
    let mut fields = Vec::new();
    let mut i = 0;
    while fields.len() < 4 {
        while i < data.len() && data[i].is_ascii_whitespace() {
            i += 1;
        }
        if i < data.len() && data[i] == b'#' {
            while i < data.len() && data[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        let start = i;
        while i < data.len() && !data[i].is_ascii_whitespace() {
            i += 1;
        }
        if start == i {
            return Err("truncated PPM header".into());
        }
        fields.push(String::from_utf8_lossy(&data[start..i]).into_owned());
    }
    i += 1; // the single whitespace byte after maxval
    let num = |s: &str| {
        s.parse::<usize>()
            .map_err(|_| format!("bad PPM number `{s}`"))
    };
    if fields[0] != "P6" || num(&fields[3])? != 255 {
        return Err(format!(
            "unsupported PPM ({} maxval {})",
            fields[0], fields[3]
        ));
    }
    let (width, height) = (num(&fields[1])?, num(&fields[2])?);
    let rgb = data
        .get(i..i + width * height * 3)
        .ok_or("PPM pixel data truncated")?
        .to_vec();
    Ok(Image { width, height, rgb })
}

/// True if text row `row` starts with exactly `text`: every pixel of every
/// glyph cell matches the expected rendering (glyph intensity as grey).
pub fn row_matches(img: &Image, text: &str, row: usize) -> bool {
    for (col, c) in text.chars().enumerate() {
        let raster = glyph(c);
        for (dy, line) in raster.raster().iter().enumerate() {
            for (dx, &i) in line.iter().enumerate() {
                let x = MARGIN + col * CHAR_WIDTH + dx;
                let y = MARGIN + row * LINE_HEIGHT + dy;
                if img.pixel(x, y) != Some([i, i, i]) {
                    return false;
                }
            }
        }
    }
    true
}

/// Number of text rows the console fits on this screen.
pub fn rows(img: &Image) -> usize {
    img.height.saturating_sub(2 * MARGIN) / LINE_HEIGHT
}

/// First text row that starts with exactly `text`.
pub fn find_row(img: &Image, text: &str) -> Option<usize> {
    (0..rows(img)).find(|&r| row_matches(img, text, r))
}

/// Number of non-black pixels, as a sanity figure for reports.
pub fn lit_pixels(img: &Image) -> usize {
    img.rgb
        .chunks(3)
        .filter(|p| p.iter().any(|&v| v != 0))
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rendered_text_matches_and_wrong_text_does_not() {
        let mut img = Image::blank(640, 120);
        img.draw_text("ZOO_OS BOOT version=0.3.0", 0);
        img.draw_text("zoo> echo hi", 3);
        assert!(row_matches(&img, "ZOO_OS BOOT version=0.3.0", 0));
        assert!(!row_matches(&img, "ZOO_OS BOOT version=9.9.9", 0));
        assert_eq!(find_row(&img, "zoo> echo hi"), Some(3));
        assert_eq!(find_row(&img, "zoo> echo ho"), None);
    }

    #[test]
    fn blank_screen_matches_nothing_visible() {
        let img = Image::blank(640, 120);
        assert!(!row_matches(&img, "ZOO_OS", 0));
        assert_eq!(lit_pixels(&img), 0);
    }

    #[test]
    fn ppm_round_trip() {
        let dir = std::env::temp_dir().join(format!("zoo-os-ppm-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("t.ppm");
        let mut bytes = b"P6\n# comment\n2 1\n255\n".to_vec();
        bytes.extend_from_slice(&[1, 2, 3, 4, 5, 6]);
        std::fs::write(&p, bytes).unwrap();
        let img = read_ppm(&p).unwrap();
        assert_eq!((img.width, img.height), (2, 1));
        assert_eq!(img.pixel(1, 0), Some([4, 5, 6]));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
