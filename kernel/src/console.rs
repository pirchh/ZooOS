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
    /// The text on screen, row by row (docs/f2-design.md).
    cells: &'static mut [char],
    /// The screen no longer shows `cells` (it scrolled); `render` redraws
    /// it, at most once per timer tick or idle moment, so a burst of lines
    /// costs one redraw instead of a framebuffer copy per line.
    stale: bool,
}

/// The largest text grid kept (columns × rows); bigger screens use this much.
const MAX_COLS: usize = 256;
const MAX_ROWS: usize = 128;

/// Storage for `Console::cells`, handed out once by `init`.
static mut CELLS: [char; MAX_COLS * MAX_ROWS] = [' '; MAX_COLS * MAX_ROWS];
static CELLS_TAKEN: AtomicBool = AtomicBool::new(false);

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
    let cols = (info.width.saturating_sub(2 * MARGIN) / CHAR_WIDTH).min(MAX_COLS);
    let rows = (info.height.saturating_sub(2 * MARGIN) / LINE_HEIGHT).min(MAX_ROWS);
    if cols == 0 || rows == 0 {
        return Err("framebuffer too small for text");
    }
    let buf = fb.into_buffer();
    REGION.call_once(|| (buf.as_ptr() as u64, buf.len(), info));
    if CELLS_TAKEN.swap(true, Ordering::Relaxed) {
        return Err("console initialised twice");
    }
    // SAFETY: `CELLS_TAKEN` lets only the first call get here, so this is
    // the only reference ever made to `CELLS`.
    let cells = unsafe { &mut (&mut *core::ptr::addr_of_mut!(CELLS))[..cols * rows] };
    let mut console = Console {
        buf,
        info,
        cols,
        rows,
        col: 0,
        row: 0,
        cells,
        stale: false,
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

/// Redraws the screen if it is behind its text. Called on timer ticks,
/// when idle, and before halting.
pub fn render() {
    with(|c| c.render());
}

impl Console {
    pub fn clear(&mut self) {
        self.buf.fill(0);
        self.cells.fill(' ');
        self.stale = false;
        self.col = 0;
        self.row = 0;
    }

    /// Clears the text rows and draws every non-blank cell (writes only:
    /// reading video memory is slow on real hardware).
    fn render(&mut self) {
        if !self.stale {
            return;
        }
        self.stale = false;
        let line_bytes = LINE_HEIGHT * self.info.stride * self.info.bytes_per_pixel;
        let top = MARGIN * self.info.stride * self.info.bytes_per_pixel;
        let end = (top + self.rows * line_bytes).min(self.buf.len());
        self.buf[top..end].fill(0);
        for row in 0..self.rows {
            for col in 0..self.cols {
                let c = self.cells[row * self.cols + col];
                if c != ' ' {
                    self.draw_char(c, col, row);
                }
            }
        }
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

    /// Moves every text row up one line and clears the last one; the
    /// screen catches up in `render`.
    fn scroll(&mut self) {
        self.cells.copy_within(self.cols.., 0);
        let n = self.cells.len();
        self.cells[n - self.cols..].fill(' ');
        self.stale = true;
    }

    /// Sets one cell, drawing it now unless the screen is behind anyway.
    fn put(&mut self, c: char, col: usize, row: usize) {
        self.cells[row * self.cols + col] = c;
        if !self.stale {
            self.draw_char(c, col, row);
        }
    }

    pub fn write_char(&mut self, c: char) {
        match c {
            '\n' => self.newline(),
            '\r' => {}
            c => {
                if self.col >= self.cols {
                    self.newline();
                }
                self.put(c, self.col, self.row);
                self.col += 1;
            }
        }
    }

    /// Erases the character before the cursor on the current line.
    pub fn backspace(&mut self) {
        if self.col > 0 {
            self.col -= 1;
            self.put(' ', self.col, self.row);
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

/// A line being formatted, at most `LINE_MAX` bytes (more is cut).
struct LineBuf {
    buf: [u8; LINE_MAX],
    len: usize,
}

/// Longest line `kprintln` writes whole (a 1 KiB message and its prefix).
const LINE_MAX: usize = 1280;

impl fmt::Write for LineBuf {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for &b in s.as_bytes() {
            if self.len == LINE_MAX {
                break;
            }
            self.buf[self.len] = b;
            self.len += 1;
        }
        Ok(())
    }
}

/// Formats the whole line first, then writes it in one piece, so lines
/// from different processors never mix (docs/smp-design.md).
#[doc(hidden)]
pub fn _kprintln(args: fmt::Arguments) {
    use fmt::Write;
    let mut line = LineBuf {
        buf: [0; LINE_MAX],
        len: 0,
    };
    let _ = line.write_fmt(args);
    let _ = line.write_str("\n");
    // Cutting may split a UTF-8 sequence: keep the valid prefix.
    let text = match core::str::from_utf8(&line.buf[..line.len]) {
        Ok(t) => t,
        Err(e) => core::str::from_utf8(&line.buf[..e.valid_up_to()]).unwrap_or(""),
    };
    _kprint(format_args!("{}", text));
}

/// Prints a line to COM1 and the screen.
#[macro_export]
macro_rules! kprintln {
    ($($arg:tt)*) => {{
        $crate::console::_kprintln(format_args!($($arg)*));
    }};
}
