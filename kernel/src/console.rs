//! Framebuffer text console (docs/console-design.md §1).
//!
//! Draws text into the UEFI framebuffer the bootloader hands over. Layout
//! constants are mirrored in xtask/src/screen.rs, which re-renders text to
//! check screenshots pixel for pixel; keep the two in step.

use bootloader_api::info::{FrameBuffer, FrameBufferInfo, PixelFormat};
use core::fmt;
use core::sync::atomic::{AtomicBool, Ordering};
use noto_sans_mono_bitmap::{FontWeight, RasterHeight, get_raster, get_raster_width};
use spin::{Mutex, Once};

pub const MARGIN: usize = 8;
pub const LINE_HEIGHT: usize = 18;
const WEIGHT: FontWeight = FontWeight::Regular;
const SIZE: RasterHeight = RasterHeight::Size16;
pub const CHAR_WIDTH: usize = get_raster_width(WEIGHT, SIZE);

pub struct Console {
    buf: &'static mut [u8],
    info: FrameBufferInfo,
    cols: usize,
    rows: usize,
    col: usize,
    row: usize,
}

static CONSOLE: Mutex<Option<Console>> = Mutex::new(None);

/// True while a user process owns the display (docs/m4-design.md §2): the
/// console stops drawing; serial output continues.
static SUSPENDED: AtomicBool = AtomicBool::new(false);

/// The framebuffer as handed over by the bootloader: (virtual start, byte
/// length, info). Recorded once, for `display_acquire`.
static REGION: Once<(u64, usize, FrameBufferInfo)> = Once::new();

pub fn framebuffer_region() -> Option<(u64, usize, FrameBufferInfo)> {
    REGION.get().copied()
}

/// Stops drawing (a user process now owns the framebuffer).
pub fn suspend() {
    SUSPENDED.store(true, Ordering::Relaxed);
}

/// Takes the screen back: clears it and draws again from the top.
pub fn resume() {
    SUSPENDED.store(false, Ordering::Relaxed);
    with(|c| c.clear());
}

/// Takes ownership of the framebuffer and clears it. Returns a description
/// for the boot log, or why the screen console is disabled.
pub fn init(fb: Option<FrameBuffer>) -> Result<FrameBufferInfo, &'static str> {
    let fb = fb.ok_or("bootloader provided no framebuffer")?;
    let info = fb.info();
    match info.pixel_format {
        PixelFormat::Rgb | PixelFormat::Bgr | PixelFormat::U8 => {}
        _ => return Err("unsupported framebuffer pixel format"),
    }
    let cols = info.width.saturating_sub(2 * MARGIN) / CHAR_WIDTH;
    let rows = info.height.saturating_sub(2 * MARGIN) / LINE_HEIGHT;
    if cols == 0 || rows == 0 {
        return Err("framebuffer too small for text");
    }
    let buf = fb.into_buffer();
    REGION.call_once(|| (buf.as_ptr() as u64, buf.len(), info));
    let mut console = Console {
        buf,
        info,
        cols,
        rows,
        col: 0,
        row: 0,
    };
    console.clear();
    *CONSOLE.lock() = Some(console);
    Ok(info)
}

/// Columns and rows of the text grid, if the console is active.
pub fn size() -> Option<(usize, usize)> {
    CONSOLE.try_lock()?.as_ref().map(|c| (c.cols, c.rows))
}

/// Runs `f` on the console if it is active and not already locked.
/// `try_lock` (never `lock`) so that a fault or panic raised while the
/// console is being drawn cannot deadlock its own report.
pub fn with<R>(f: impl FnOnce(&mut Console) -> R) -> Option<R> {
    if SUSPENDED.load(Ordering::Relaxed) {
        return None;
    }
    let mut guard = CONSOLE.try_lock()?;
    guard.as_mut().map(f)
}

impl Console {
    pub fn clear(&mut self) {
        self.buf.fill(0);
        self.col = 0;
        self.row = 0;
    }

    fn put_pixel(&mut self, x: usize, y: usize, intensity: u8) {
        let bpp = self.info.bytes_per_pixel;
        let offset = (y * self.info.stride + x) * bpp;
        let Some(px) = self.buf.get_mut(offset..offset + bpp) else {
            return;
        };
        match self.info.pixel_format {
            // Grey text: R, G and B are equal, so RGB and BGR are identical.
            PixelFormat::Rgb | PixelFormat::Bgr => {
                px[..3].fill(intensity);
                if bpp > 3 {
                    px[3] = 0;
                }
            }
            _ => px[0] = intensity,
        }
    }

    fn draw_char(&mut self, c: char, col: usize, row: usize) {
        let raster = get_raster(c, WEIGHT, SIZE)
            .or_else(|| get_raster('?', WEIGHT, SIZE))
            .expect("'?' is in the font");
        let x0 = MARGIN + col * CHAR_WIDTH;
        let y0 = MARGIN + row * LINE_HEIGHT;
        for (dy, line) in raster.raster().iter().enumerate() {
            for (dx, &i) in line.iter().enumerate() {
                self.put_pixel(x0 + dx, y0 + dy, i);
            }
        }
    }

    fn newline(&mut self) {
        self.col = 0;
        if self.row + 1 < self.rows {
            self.row += 1;
        } else {
            self.scroll();
        }
    }

    /// Moves every text row up one line and clears the last one.
    fn scroll(&mut self) {
        let line_bytes = LINE_HEIGHT * self.info.stride * self.info.bytes_per_pixel;
        let top = MARGIN * self.info.stride * self.info.bytes_per_pixel;
        let end = top + self.rows * line_bytes;
        self.buf.copy_within(top + line_bytes..end, top);
        self.buf[end - line_bytes..end].fill(0);
    }

    pub fn write_char(&mut self, c: char) {
        match c {
            '\n' => self.newline(),
            '\r' => {}
            c => {
                if self.col >= self.cols {
                    self.newline();
                }
                self.draw_char(c, self.col, self.row);
                self.col += 1;
            }
        }
    }

    /// Erases the character before the cursor on the current line.
    pub fn backspace(&mut self) {
        if self.col > 0 {
            self.col -= 1;
            self.draw_char(' ', self.col, self.row);
        }
    }
}

impl fmt::Write for Console {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        s.chars().for_each(|c| self.write_char(c));
        Ok(())
    }
}

#[doc(hidden)]
pub fn _kprint(args: fmt::Arguments) {
    crate::serial::_print(args);
    with(|c| {
        use fmt::Write;
        let _ = c.write_fmt(args);
    });
}

/// Prints to COM1 and the screen.
#[macro_export]
macro_rules! kprint {
    ($($arg:tt)*) => {
        $crate::console::_kprint(format_args!($($arg)*))
    };
}

/// Prints a line to COM1 and the screen.
#[macro_export]
macro_rules! kprintln {
    ($($arg:tt)*) => {{
        $crate::console::_kprint(format_args!($($arg)*));
        $crate::console::_kprint(format_args!("\n"));
    }};
}
