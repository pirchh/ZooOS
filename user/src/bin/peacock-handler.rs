//! PeacockHandler: the window server (docs/m4-design.md §4; look:
//! docs/u1-design.md). Owns the display and keyboard, runs four
//! applications (Files, Crash Test, the Assistant and Notes) over channels,
//! and draws a macOS-style desktop: a translucent menu bar, rounded windows
//! with shadows and traffic lights, Inter text from the font atlas in the
//! initrd, a dock, and notification banners. Light and dark appearances
//! switch with F8 or the menu bar's switch. Keys go to the focused window;
//! the mouse focuses, moves, resizes, closes, minimises and zooms windows
//! (docs/d1-design.md, d3, d8, d9). Only the parts of the screen that
//! changed are redrawn (docs/d2-design.md). Applications that crash are
//! restarted.
//!
//! Its second start-up handle is the home directory (from Zookeeper); each
//! start of the Assistant gets a fresh copy, which it passes on to the file
//! broker.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;

use core::fmt::Write as _;
use pc_keyboard::layouts::Us104Key;
use pc_keyboard::{DecodedKey, HandleControl, KeyCode, PS2Keyboard, ScancodeSet1};
use zoo_abi::font::{Atlas, Face, FaceId, blend};
use zoo_abi::ui::{
    BASELINE, DARK, DOCK_ICON, LIGHT, LIGHT_D, LINE_H, MENU_H, Palette as Theme, RADIUS,
    RESIZE_CORNER, TEXT_X, TITLE_H,
};
use zoo_abi::{CLOCK_REALTIME, FORMAT_BGR, MAX_MESSAGE, MouseEvent};
use zoo_rt::app::{Key, MAX_LINES};
use zoo_rt::{
    Error, Line, channel_create, close, dir_open, dir_rights, display_acquire, input_read, kill,
    log, spawn, spawn_with, try_recv, wait, wait_any,
};

zoo_rt::entry2!(main);

// ---- appearance (docs/u1-design.md §3), colours as 0xRRGGBB --------------

/// Traffic lights: fill and rim, close/minimise/zoom.
const LIGHTS: [(u32, u32); 3] = [
    (0xFF5F57, 0xE0443E),
    (0xFEBC2E, 0xDEA123),
    (0x28C840, 0x1AAB29),
];
/// Dock icon colours, indexed like the applications.
const ICON: [u32; APPS] = [0x3B8EEA, 0x8E8E93, 0x7A5AF8, 0xF5C342];

/// Largest supported screen, in logical pixels (a 2560x1440 monitor is
/// used whole, at 1x; docs/u2b-design.md).
const MAX_W: usize = 2560;
const MAX_H: usize = 1600;

/// Times one application may be restarted after crashing.
const APP_RESTARTS: u32 = 3;
/// Damage rectangles kept per frame before falling back to a full repaint.
const MAX_DAMAGE: usize = 12;
/// Pointer arrow size.
/// The pointer image: its size (outline and shadow included) and where
/// its tip, the hot spot, lies in it.
const POINTER_W: usize = 18;
const POINTER_H: usize = 24;
const POINTER_HOT: usize = 2;
/// The arrow's outline in pointer units, tip at (0, 0): the left edge
/// straight down, a notch, the tail slanting down and right, the wing back
/// up to the tip (the shape of the macOS arrow).
const ARROW: [(f32, f32); 7] = [
    (0.0, 0.0),
    (0.0, 16.0),
    (3.9, 12.4),
    (6.6, 18.6),
    (9.0, 17.6),
    (6.4, 11.8),
    (11.4, 11.6),
];
/// The white outline's width, in pointer units.
const ARROW_EDGE: f32 = 1.3;

/// Whether (x, y) is inside the arrow, and its squared distance to the
/// arrow's outline.
fn arrow_at(x: f32, y: f32) -> (bool, f32) {
    let mut inside = false;
    let mut d2 = f32::MAX;
    for k in 0..ARROW.len() {
        let (ax, ay) = ARROW[k];
        let (bx, by) = ARROW[(k + 1) % ARROW.len()];
        // Even-odd crossing test.
        if (ay > y) != (by > y) && x < ax + (y - ay) * (bx - ax) / (by - ay) {
            inside = !inside;
        }
        // Distance to the segment.
        let (dx, dy) = (bx - ax, by - ay);
        let t = (((x - ax) * dx + (y - ay) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
        let (ex, ey) = (ax + t * dx - x, ay + t * dy - y);
        d2 = d2.min(ex * ex + ey * ey);
    }
    (inside, d2)
}

/// The pointer image (0xAARRGGBB, not premultiplied), `POINTER_W` x
/// `POINTER_H`: a black arrow, a white outline and a soft shadow below,
/// each pixel sampled 4 x 4 times for smooth edges.
fn pointer_image(scale: usize) -> Vec<u32> {
    let (pw, ph) = (POINTER_W * scale, POINTER_H * scale);
    let mut img = alloc::vec![0u32; pw * ph];
    let sc = scale as f32;
    let edge2 = ARROW_EDGE * ARROW_EDGE;
    for r in 0..ph {
        for c in 0..pw {
            // Summed over the 16 samples: alpha, and white (premultiplied).
            let (mut a, mut white) = (0.0f32, 0.0f32);
            for sy in 0..4 {
                for sx in 0..4 {
                    let x = (c as f32 + (sx as f32 + 0.5) / 4.0) / sc - POINTER_HOT as f32;
                    let y = (r as f32 + (sy as f32 + 0.5) / 4.0) / sc - POINTER_HOT as f32;
                    let (inside, d2) = arrow_at(x, y);
                    if inside {
                        a += 1.0;
                    } else if d2 <= edge2 {
                        a += 1.0;
                        white += 1.0;
                    } else {
                        // The shadow: the outlined arrow, 1.2 lower,
                        // fading over two units.
                        let (sin, sd2) = arrow_at(x, y - 1.2);
                        let near = |w: f32| sd2 <= (ARROW_EDGE + w) * (ARROW_EDGE + w);
                        a += if sin || near(0.0) {
                            0.32
                        } else if near(0.8) {
                            0.2
                        } else if near(1.8) {
                            0.08
                        } else {
                            0.0
                        };
                    }
                }
            }
            if a > 0.0 {
                let alpha = (a / 16.0 * 255.0 + 0.5) as u32;
                let grey = (white / a * 255.0 + 0.5) as u32;
                img[r * pw + c] = (alpha.min(255) << 24) | (grey.min(255) * 0x01_0101);
            }
        }
    }
    img
}
/// Applications, in dock order; Notes is the last.
const APPS: usize = 4;
const NOTES: usize = 3;
/// Two presses within this many ticks (and 4 px) are a double-click.
const DOUBLE_CLICK: u64 = 50;
const MIN_W: usize = 240;
const MIN_H: usize = 140;
/// Ticks an application gets to end after its window is closed.
const CLOSE_WAIT: u64 = 100;
/// Frames kept for the timing report (F11).
const FRAME_SAMPLES: usize = 64;
/// Window shadows: how far they spread, and how far down they sit.
const SHADOW: usize = 24;
const SHADOW_DROP: usize = 8;
/// What a window's redraw covers beyond its rectangle.
const MARGIN: usize = SHADOW + SHADOW_DROP;
/// Notification banners: size, and how long they stay (ticks).
const BANNER_W: usize = 360;
const BANNER_H: usize = 62;
const BANNER_TICKS: u64 = 600;
const TEXT_COLS: usize = 80;

/// Test-only fault address (docs/m4-design.md §4, the F12 key).
const KERNEL_ADDR: u64 = 0xFFFF_C000_0000_1000;

struct Text {
    buf: [u8; TEXT_COLS],
    len: usize,
}

impl Text {
    const fn new() -> Text {
        Text {
            buf: [0; TEXT_COLS],
            len: 0,
        }
    }
    fn set(&mut self, s: &[u8]) {
        let n = s.len().min(TEXT_COLS);
        self.buf[..n].copy_from_slice(&s[..n]);
        self.len = n;
    }
    fn as_str(&self) -> &str {
        core::str::from_utf8(&self.buf[..self.len]).unwrap_or("?")
    }
}

struct App {
    /// Its name in the dock and banners, e.g. "Crash Test".
    label: &'static str,
    program: &'static str,
    /// Gets a copy of the home directory handle as its second handle.
    wants_home: bool,
    chan: u64,
    process: u64,
    running: bool,
    /// Closed by the user: window hidden, not restarted.
    closed: bool,
    /// Minimised: running, window hidden until its dock icon is clicked.
    minimized: bool,
    restarts: u32,
    title: Text,
    /// The title changed since the window was last shown (its title bar
    /// must be drawn again, not just its content).
    title_changed: bool,
    /// Lines whose look changed since the window was last shown (bit k:
    /// line k), so only they are drawn again.
    changed: u32,
    lines: [Text; MAX_LINES],
    count: usize,
    selected: Option<usize>,
}

impl App {
    const fn new(label: &'static str, program: &'static str, wants_home: bool) -> App {
        App {
            label,
            program,
            wants_home,
            chan: 0,
            process: 0,
            running: false,
            closed: false,
            minimized: false,
            restarts: 0,
            title: Text::new(),
            title_changed: false,
            changed: 0,
            lines: [const { Text::new() }; MAX_LINES],
            count: 0,
            selected: None,
        }
    }

    fn visible(&self) -> bool {
        !self.closed && !self.minimized
    }

    fn start(&mut self, home: u64) -> bool {
        let Ok((mine, theirs)) = channel_create() else {
            return false;
        };
        let started = if self.wants_home && home != zoo_rt::NO_HANDLE {
            match dir_open(home, "", dir_rights::ALL) {
                Ok(copy) => {
                    let r = spawn_with(self.program, [Some(theirs), Some(copy)]);
                    if r.is_err() {
                        let _ = close(copy);
                        let _ = close(theirs);
                    }
                    r
                }
                Err(e) => Err(e),
            }
        } else {
            spawn(self.program, Some(theirs))
        };
        match started {
            Ok(p) => {
                self.chan = mine;
                self.process = p;
                self.running = true;
                self.title.set(self.label.as_bytes());
                self.count = 0;
                self.selected = None;
                true
            }
            Err(_) => {
                let _ = close(mine);
                false
            }
        }
    }

    /// Applies one message; returns true when the window should be redrawn.
    fn apply(&mut self, msg: &[u8]) -> bool {
        if let Some(t) = msg.strip_prefix(b"title:") {
            // Applications send their title with every update; only a new
            // one needs the title bar drawn again.
            let n = t.len().min(TEXT_COLS);
            if self.title.buf[..self.title.len] != t[..n] {
                self.title.set(t);
                self.title_changed = true;
            }
        } else if let Some(n) = msg.strip_prefix(b"lines:") {
            if let Some(n) = parse_num(n) {
                let n = n.min(MAX_LINES);
                // Lines that appear or disappear.
                for k in n.min(self.count)..n.max(self.count) {
                    self.changed |= 1 << k;
                }
                self.count = n;
            }
        } else if let Some(rest) = msg.strip_prefix(b"line:") {
            if let Some(colon) = rest.iter().position(|&b| b == b':')
                && let Some(i) = parse_num(&rest[..colon])
                && i < MAX_LINES
            {
                let t = &rest[colon + 1..];
                let n = t.len().min(TEXT_COLS);
                if self.lines[i].buf[..self.lines[i].len] != t[..n] {
                    self.lines[i].set(t);
                    self.changed |= 1 << i;
                }
            }
        } else if let Some(s) = msg.strip_prefix(b"sel:") {
            let new = parse_num(s);
            if new != self.selected {
                for k in [self.selected, new].into_iter().flatten() {
                    if k < MAX_LINES {
                        self.changed |= 1 << k;
                    }
                }
                self.selected = new;
            }
        } else if msg == b"show" {
            return true;
        }
        false
    }
}

fn parse_num(s: &[u8]) -> Option<usize> {
    if s.is_empty() || s.len() > 4 {
        return None;
    }
    s.iter().try_fold(0usize, |n, &b| {
        b.is_ascii_digit().then(|| n * 10 + (b - b'0') as usize)
    })
}

/// Coverage (0..=255) of a quarter circle of radius `r` at offset (dx, dy)
/// from the corner's outer edge, the corner's centre being (r, r): signed
/// distance from the squared distance (no square root needed).
fn corner_coverage(r: usize, dx: usize, dy: usize) -> u8 {
    let (fx, fy) = (r as f32 - dx as f32 - 0.5, r as f32 - dy as f32 - 0.5);
    let d2 = fx * fx + fy * fy;
    let r2 = (r * r) as f32;
    // Distance beyond the circle, ~(d^2 - r^2) / 2r.
    let sd = (d2 - r2) / (2.0 * r as f32);
    let c = 0.5 - sd;
    if c <= 0.0 {
        0
    } else if c >= 1.0 {
        255
    } else {
        (c * 255.0 + 0.5) as u8
    }
}

/// Smooth falloff over `2 * SHADOW * scale` device steps: 255 inside, 0
/// outside.
fn shadow_ramp(scale: usize) -> Vec<u8> {
    let n = 2 * SHADOW * scale;
    let mut r = alloc::vec![0u8; n];
    for (k, v) in r.iter_mut().enumerate() {
        let t = (k as f32 + 0.5) / n as f32;
        let s = 1.0 - t * t * (3.0 - 2.0 * t);
        *v = (s * 255.0 + 0.5) as u8;
    }
    r
}

struct Screen {
    /// Size in device pixels.
    w: usize,
    h: usize,
    /// Device pixels per logical pixel (1, or 2 on 4K screens,
    /// docs/u2b-design.md). Drawing calls take logical coordinates;
    /// `clip`, `back` and `present` are in device pixels.
    s: usize,
    stride: usize,
    fb: *mut u32,
    bgr: bool,
    /// Changes show only after `display_flush` (a virtio GPU).
    flush: bool,
    /// `back` is the display memory itself (with `flush`: nothing shows
    /// before the flush, so no copy is needed for whole frames).
    direct: bool,
    back: &'static mut [u32],
    /// Drawing is limited to this rectangle (x0, y0, x1, y1), exclusive,
    /// in device pixels.
    clip: (usize, usize, usize, usize),
}

impl Screen {
    /// True if logical rectangle (x, y, w, h) reaches into the clip
    /// rectangle: shapes that do not are skipped (docs/u1-design.md §4).
    fn visible(&self, x: usize, y: usize, w: usize, h: usize) -> bool {
        let s = self.s;
        self.visible_dev(x * s, y * s, w * s, h * s)
    }

    fn visible_dev(&self, x: usize, y: usize, w: usize, h: usize) -> bool {
        let (cx0, cy0, cx1, cy1) = self.clip;
        x < cx1 && y < cy1 && x + w > cx0 && y + h > cy0
    }

    fn fill(&mut self, x: usize, y: usize, w: usize, h: usize, c: u32) {
        let s = self.s;
        self.fill_dev(x * s, y * s, w * s, h * s, c);
    }

    fn fill_dev(&mut self, x: usize, y: usize, w: usize, h: usize, c: u32) {
        let (cx0, cy0, cx1, cy1) = self.clip;
        let (x0, y0, x1, y1) = (x.max(cx0), y.max(cy0), (x + w).min(cx1), (y + h).min(cy1));
        if x0 >= x1 || y0 >= y1 {
            return;
        }
        for row in y0..y1 {
            self.back[row * self.w + x0..row * self.w + x1].fill(c);
        }
    }

    /// Blends `c` over the device pixel at (x, y) by `a` (0..=255).
    fn blend_dev(&mut self, x: usize, y: usize, c: u32, a: u8) {
        let (cx0, cy0, cx1, cy1) = self.clip;
        if a != 0 && x >= cx0 && x < cx1 && y >= cy0 && y < cy1 {
            let p = &mut self.back[y * self.w + x];
            *p = if a == 255 { c } else { blend(*p, c, a) };
        }
    }

    /// Blends `c` over a device rectangle by `a`.
    fn tint_dev(&mut self, x: usize, y: usize, w: usize, h: usize, c: u32, a: u8) {
        let (cx0, cy0, cx1, cy1) = self.clip;
        let (x0, y0, x1, y1) = (x.max(cx0), y.max(cy0), (x + w).min(cx1), (y + h).min(cy1));
        if x0 >= x1 {
            return;
        }
        for row in y0..y1 {
            for p in &mut self.back[row * self.w + x0..row * self.w + x1] {
                *p = blend(*p, c, a);
            }
        }
    }

    /// A rounded rectangle, anti-aliased, blended by `a`; `corners` says
    /// which are round: top-left, top-right, bottom-left, bottom-right.
    /// Logical coordinates; corners are computed at device resolution.
    #[allow(clippy::too_many_arguments)]
    fn round_rect(
        &mut self,
        x: usize,
        y: usize,
        w: usize,
        h: usize,
        r: usize,
        c: u32,
        a: u8,
        corners: [bool; 4],
    ) {
        let s = self.s;
        let (x, y, w, h, r) = (x * s, y * s, w * s, h * s, r * s);
        if !self.visible_dev(x, y, w, h) {
            return;
        }
        let r = r.min(w / 2).min(h / 2);
        let (cx0, cy0, cx1, cy1) = self.clip;
        let rows = cy0.saturating_sub(y)..(cy1 - y).min(h);
        let cols = cx0.saturating_sub(x)..cx1.saturating_sub(x).min(w);
        for row in rows {
            let (top, bottom) = (row < r, row >= h - r);
            if !top && !bottom {
                if a == 255 {
                    self.fill_dev(x, y + row, w, 1, c);
                } else {
                    self.tint_dev(x, y + row, w, 1, c, a);
                }
                continue;
            }
            let dy = if top { row } else { h - 1 - row };
            let (left_round, right_round) = if top {
                (corners[0], corners[1])
            } else {
                (corners[2], corners[3])
            };
            for col in cols.clone() {
                let cov = if col < r && left_round {
                    corner_coverage(r, col, dy)
                } else if col >= w - r && right_round {
                    corner_coverage(r, w - 1 - col, dy)
                } else {
                    255
                };
                let cov = ((u32::from(cov) * u32::from(a) + 127) / 255) as u8;
                self.blend_dev(x + col, y + row, c, cov);
            }
        }
    }

    /// A one-device-pixel anti-aliased outline of a rounded rectangle
    /// (logical coordinates).
    fn round_outline(&mut self, x: usize, y: usize, w: usize, h: usize, r: usize, c: u32) {
        let s = self.s;
        let (x, y, w, h, r) = (x * s, y * s, w * s, h * s, r * s);
        if !self.visible_dev(x, y, w, h) {
            return;
        }
        let r = r.min(w / 2).min(h / 2);
        // Straight edges.
        self.fill_dev(x + r, y, w - 2 * r, 1, c);
        self.fill_dev(x + r, y + h - 1, w - 2 * r, 1, c);
        self.fill_dev(x, y + r, 1, h - 2 * r, c);
        self.fill_dev(x + w - 1, y + r, 1, h - 2 * r, c);
        // Corners: the ring between radius r and r - 1.
        for dy in 0..r {
            for dx in 0..r {
                let outer = corner_coverage(r, dx, dy);
                let inner = if dx >= 1 && dy >= 1 {
                    corner_coverage(r - 1, dx - 1, dy - 1)
                } else {
                    0
                };
                let cov = outer.saturating_sub(inner);
                if cov == 0 {
                    continue;
                }
                for (px, py) in [
                    (x + dx, y + dy),
                    (x + w - 1 - dx, y + dy),
                    (x + dx, y + h - 1 - dy),
                    (x + w - 1 - dx, y + h - 1 - dy),
                ] {
                    self.blend_dev(px, py, c, cov);
                }
            }
        }
    }

    /// A filled anti-aliased disc of logical diameter `d` with its top-left
    /// at (x, y), and a rim colour on its outermost device-pixel ring.
    fn disc(&mut self, x: usize, y: usize, d: usize, fill: u32, rim: u32) {
        let s = self.s;
        self.disc_part(x * s, y * s, d * s, fill, rim, 0);
    }

    /// `disc` in device pixels, drawing only columns from `from` on (the
    /// right part of the disc when `from` > 0).
    fn disc_part(&mut self, x: usize, y: usize, d: usize, fill: u32, rim: u32, from: usize) {
        if !self.visible_dev(x, y, d, d) {
            return;
        }
        let r = d / 2;
        for dy in 0..d {
            for dx in from..d {
                // Mirror into the top-left quarter.
                let (qx, qy) = (dx.min(d - 1 - dx), dy.min(d - 1 - dy));
                let outer = corner_coverage(r, qx, qy);
                if outer == 0 {
                    continue;
                }
                let inner = if qx >= 1 && qy >= 1 && r > 1 {
                    corner_coverage(r - 1, qx - 1, qy - 1)
                } else {
                    0
                };
                self.blend_dev(x + dx, y + dy, rim, outer);
                self.blend_dev(x + dx, y + dy, fill, inner);
            }
        }
    }

    /// A soft shadow of black under the logical rectangle, `a` at its
    /// strongest, drawn only outside the rectangle itself (which covers the
    /// rest). `ramp` has `2 * SHADOW * s` steps.
    fn shadow(&mut self, rect: (usize, usize, usize, usize), a: u8, ramp: &[u8]) {
        let sc = self.s;
        let (wx, wy, ww, wh) = (rect.0 * sc, rect.1 * sc, rect.2 * sc, rect.3 * sc);
        // The shadow's rectangle: the window's, moved down.
        let (sx0, sy0) = (wx as isize, (wy + SHADOW_DROP * sc) as isize);
        let (sx1, sy1) = (sx0 + ww as isize, sy0 + wh as isize);
        let s = (SHADOW * sc) as isize;
        // How strong at distance d outside an edge (negative: inside).
        let f = |d: isize| -> u32 {
            let k = d + s;
            if k < 0 {
                255
            } else if k >= 2 * s {
                0
            } else {
                u32::from(ramp[k as usize])
            }
        };
        let (cx0, cy0, cx1, cy1) = self.clip;
        let y0 = (sy0 - s).max(cy0 as isize).max(0) as usize;
        let y1 = ((sy1 + s) as usize).min(cy1);
        let x0 = (sx0 - s).max(cx0 as isize).max(0) as usize;
        let x1 = ((sx1 + s) as usize).min(cx1);
        for py in y0..y1 {
            let fy = f((sy0 - py as isize).max(py as isize - (sy1 - 1)));
            if fy == 0 {
                continue;
            }
            let inside_rows = py >= wy && py < wy + wh;
            for px in x0..x1 {
                if inside_rows && px >= wx && px < wx + ww {
                    continue;
                }
                let fx = f((sx0 - px as isize).max(px as isize - (sx1 - 1)));
                let v = fx * fy * u32::from(a) / (255 * 255);
                if v != 0 {
                    let p = &mut self.back[py * self.w + px];
                    *p = blend(*p, 0x000000, v as u8);
                }
            }
        }
    }

    /// Draws `text` in `face` (already the face for this scale) with its
    /// baseline at logical `y`, starting at `x`, clipped at `max_x`;
    /// returns where it ended (logical).
    fn text(&mut self, face: &Face, x: usize, y: usize, text: &str, c: u32, max_x: usize) -> usize {
        let s = self.s;
        let (dx, dy, dmax) = (x * s, y * s, max_x * s);
        if !self.visible_dev(
            dx,
            dy.saturating_sub(20 * s),
            dmax.saturating_sub(dx),
            28 * s,
        ) {
            return x + face.width(text).div_ceil(s);
        }
        let end = face.draw(text, dx as i32, dy as i32, dmax as i32, |px, py, cov| {
            if px >= 0 && py >= 0 {
                self.blend_dev(px as usize, py as usize, c, cov);
            }
        });
        (end.max(0) as usize).div_ceil(s)
    }

    /// Copies device rectangle (x0, y0, x1, y1) of the back buffer to the
    /// framebuffer. Called after every damaged rectangle is drawn, so the
    /// screen only shows complete frames.
    fn present(&mut self, (x0, y0, x1, y1): (usize, usize, usize, usize)) {
        if self.direct {
            return;
        }
        let (x1, y1) = (x1.min(self.w), y1.min(self.h));
        if x0 >= x1 {
            return;
        }
        for y in y0..y1 {
            let src = &self.back[y * self.w + x0..y * self.w + x1];
            // SAFETY: the framebuffer is mapped at `fb` for `stride * h`
            // pixels of 4 bytes (checked at start-up); row `y` is below
            // `h` and columns x0..x1 are within the row.
            let dst = unsafe { self.fb.add(y * self.stride + x0) };
            if self.bgr {
                // 0xRRGGBB in little-endian memory is B, G, R, X: BGR.
                // SAFETY: as above; source and destination do not overlap.
                unsafe { core::ptr::copy_nonoverlapping(src.as_ptr(), dst, src.len()) };
            } else {
                for (x, &c) in src.iter().enumerate() {
                    let rgb = ((c & 0xFF) << 16) | (c & 0xFF00) | ((c >> 16) & 0xFF);
                    // SAFETY: as above.
                    unsafe { dst.add(x).write_volatile(rgb) };
                }
            }
        }
    }
}

/// A window's cached image (docs/u2-compositor-design.md): the window and
/// its shadow, from (x - MARGIN, y - MARGIN), premultiplied by alpha, with
/// alpha in the top byte. Rebuilt only when the window's look changes;
/// moving a window only composites it again.
#[derive(Clone)]
struct Layer {
    buf: Vec<u32>,
    w: usize,
    h: usize,
    /// Per row, the fully opaque span [x0, x1): copied, not blended.
    spans: Vec<(u32, u32)>,
    dirty: bool,
    /// Only some content rows changed: redraw them in place (one pass).
    content_dirty: bool,
    /// Those rows, (y0, y1) from the window's top.
    pending: Vec<(usize, usize)>,
}

impl Layer {
    const fn new() -> Layer {
        Layer {
            buf: Vec::new(),
            w: 0,
            h: 0,
            spans: Vec::new(),
            dirty: true,
            content_dirty: false,
            pending: Vec::new(),
        }
    }
}

/// `c` (premultiplied, alpha in the top byte) over `d`.
fn over(d: u32, c: u32) -> u32 {
    // No branches, so loops over it vectorise: a = 255 gives c, and
    // a = 0 gives d (d · 255 / 255 is exact).
    let a = c >> 24;
    let inv = 255 - a;
    // d · (255 - a) / 255 per channel, red and blue together in 16-bit
    // lanes: (x + 1 + (x >> 8)) >> 8 is x / 255 rounded down for every
    // x <= 255 · 255, and no lane carries into the next. `build_layer`
    // keeps each colour channel <= a, so the sum stays <= 255.
    let rb = (d & 0xFF_00FF) * inv;
    let rb = ((rb + 0x01_0001 + ((rb >> 8) & 0xFF_00FF)) >> 8) & 0xFF_00FF;
    let g = ((d >> 8) & 0xFF) * inv;
    let g = ((g + 1 + (g >> 8)) >> 8) << 8;
    (c & 0xFF_FFFF) + rb + g
}

// ---- Animations (docs/u2d-design.md) ------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum AnimKind {
    Open,
    Close,
    Minimize,
    Restore,
}

impl AnimKind {
    fn name(self) -> &'static str {
        match self {
            AnimKind::Open => "open",
            AnimKind::Close => "close",
            AnimKind::Minimize => "minimize",
            AnimKind::Restore => "restore",
        }
    }

    /// How long it runs, in milliseconds.
    fn millis(self) -> u64 {
        match self {
            AnimKind::Open => 220,
            AnimKind::Close => 180,
            AnimKind::Minimize => 320,
            AnimKind::Restore => 280,
        }
    }

    /// Progress (0..=1) for time `t` (0..=1): things arriving slow down
    /// at the end, things leaving speed up, the dock trip does both.
    fn ease(self, t: f32) -> f32 {
        match self {
            AnimKind::Open => 1.0 - (1.0 - t) * (1.0 - t) * (1.0 - t),
            AnimKind::Close => t * t * t,
            AnimKind::Minimize | AnimKind::Restore => {
                if t < 0.5 {
                    4.0 * t * t * t
                } else {
                    let u = 2.0 - 2.0 * t;
                    1.0 - u * u * u / 2.0
                }
            }
        }
    }
}

/// A logical rectangle with fractional position and size.
type RectF = (f32, f32, f32, f32);

/// One window's animation: its rectangle and opacity move from `from`/`a0`
/// to `to`/`a1` along the kind's easing curve.
struct Anim {
    kind: AnimKind,
    /// Start, and length, in time-stamp-counter cycles.
    start: u64,
    dur: u64,
    from: RectF,
    to: RectF,
    a0: f32,
    a1: f32,
    /// The window's image for a closing or minimising window, taken when
    /// the animation starts (the window itself is gone by then); opening
    /// and restoring windows use their live layer.
    snapshot: Option<Layer>,
    /// The window's logical size when its layer was built.
    size: (usize, usize),
    /// The logical area the last frame drew (damaged by the next).
    last: (usize, usize, usize, usize),
    frames: u32,
}

impl Anim {
    /// The rectangle and opacity at time `now`, and whether it has ended.
    fn at(&self, now: u64) -> (RectF, f32, bool) {
        let t = if self.dur == 0 {
            1.0
        } else {
            (now.saturating_sub(self.start) as f32 / self.dur as f32).min(1.0)
        };
        let e = self.kind.ease(t);
        let l = |a: f32, b: f32| a + (b - a) * e;
        let (f, g) = (self.from, self.to);
        (
            (l(f.0, g.0), l(f.1, g.1), l(f.2, g.2), l(f.3, g.3)),
            l(self.a0, self.a1),
            t >= 1.0,
        )
    }
}

fn floor(x: f32) -> f32 {
    let i = x as i32 as f32;
    if i > x { i - 1.0 } else { i }
}

fn ceil(x: f32) -> f32 {
    let i = x as i32 as f32;
    if i < x { i + 1.0 } else { i }
}

/// The logical area (x, y, w, h) a window layer covers when the window
/// (of logical `size`) is drawn at `rect`.
fn layer_bounds(rect: RectF, size: (usize, usize)) -> (usize, usize, usize, usize) {
    let kx = rect.2 / size.0.max(1) as f32;
    let ky = rect.3 / size.1.max(1) as f32;
    let m = MARGIN as f32;
    let x0 = floor(rect.0 - m * kx).max(0.0);
    let y0 = floor(rect.1 - m * ky).max(0.0);
    let x1 = ceil(rect.0 + rect.2 + m * kx) + 1.0;
    let y1 = ceil(rect.1 + rect.3 + m * ky) + 1.0;
    (
        x0 as usize,
        y0 as usize,
        (x1 - x0).max(0.0) as usize,
        (y1 - y0).max(0.0) as usize,
    )
}

/// `a` to `b` by `f` (0..=255), for premultiplied pixels: red and blue,
/// and alpha and green, two channels at a time in 16-bit lanes.
fn lerp_px(a: u32, b: u32, f: u32) -> u32 {
    let g = 256 - f;
    let rb = (((a & 0x00FF_00FF) * g + (b & 0x00FF_00FF) * f) >> 8) & 0x00FF_00FF;
    let ag = ((((a >> 8) & 0x00FF_00FF) * g + ((b >> 8) & 0x00FF_00FF) * f) >> 8) & 0x00FF_00FF;
    rb | (ag << 8)
}

/// A premultiplied pixel scaled by `k` (0..=256).
fn fade_px(p: u32, k: u32) -> u32 {
    let rb = (((p & 0x00FF_00FF) * k) >> 8) & 0x00FF_00FF;
    let ag = ((((p >> 8) & 0x00FF_00FF) * k) >> 8) & 0x00FF_00FF;
    rb | (ag << 8)
}

/// Layer `l`, built for a window of logical `size`, composited over `back`
/// (device pixels, `sw` wide, within `clip`) with the window scaled into
/// logical `rect`, at drawing scale `s`, faded to `alpha` (0..=1).
/// Sampled bilinearly; outside the layer counts as transparent, so its
/// edges stay smooth while it shrinks.
#[allow(clippy::too_many_arguments)]
fn composite_scaled(
    back: &mut [u32],
    sw: usize,
    clip: (usize, usize, usize, usize),
    s: usize,
    l: &Layer,
    size: (usize, usize),
    rect: RectF,
    alpha: f32,
) {
    if l.w == 0 || rect.2 < 1.0 || rect.3 < 1.0 || alpha <= 0.0 {
        return;
    }
    let sf = s as f32;
    // Source to screen, in device pixels: the same ratio as logically.
    let fx = rect.2 / size.0.max(1) as f32;
    let fy = rect.3 / size.1.max(1) as f32;
    let ox = (rect.0 - MARGIN as f32 * fx) * sf;
    let oy = (rect.1 - MARGIN as f32 * fy) * sf;
    let (cx0, cy0, cx1, cy1) = clip;
    let x0 = (ox.max(0.0) as usize).max(cx0);
    let y0 = (oy.max(0.0) as usize).max(cy0);
    let x1 = ((ox + l.w as f32 * fx).max(0.0) as usize + 1).min(cx1);
    let y1 = ((oy + l.h as f32 * fy).max(0.0) as usize + 1).min(cy1);
    if x0 >= x1 || y0 >= y1 {
        return;
    }
    let k = (alpha.min(1.0) * 256.0) as u32;
    let (lw, lh) = (l.w as i64, l.h as i64);
    let px = |x: i64, y: i64| -> u32 {
        if x < 0 || y < 0 || x >= lw || y >= lh {
            0
        } else {
            l.buf[y as usize * l.w + x as usize]
        }
    };
    // Source coordinates in 16.16 fixed point, at pixel centres.
    let step = (65536.0 / fx) as i64;
    let u0 = (((x0 as f32 + 0.5 - ox) / fx - 0.5) * 65536.0) as i64;
    for dy in y0..y1 {
        let v = (((dy as f32 + 0.5 - oy) / fy - 0.5) * 65536.0) as i64;
        let (vi, vf) = (v >> 16, ((v >> 8) & 0xFF) as u32);
        let mut u = u0;
        for d in &mut back[dy * sw + x0..dy * sw + x1] {
            let (ui, uf) = (u >> 16, ((u >> 8) & 0xFF) as u32);
            u += step;
            let top = lerp_px(px(ui, vi), px(ui + 1, vi), uf);
            let bottom = lerp_px(px(ui, vi + 1), px(ui + 1, vi + 1), uf);
            let p = lerp_px(top, bottom, vf);
            if p != 0 {
                *d = over(*d, if k >= 256 { p } else { fade_px(p, k) });
            }
        }
    }
}

const WEEKDAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// (month 1-12, day 1-31) of day `z` since 1970-01-01 (proleptic
/// Gregorian; the inverse of the kernel's `days_from_civil`).
fn civil_from_days(z: i64) -> (usize, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (month as usize, day)
}

/// The menu-bar clock (docs/d5-design.md): the UTC date and time, or the
/// uptime when there is no clock. UTC is named: there are no time zones
/// yet.
fn write_clock(out: &mut Line) {
    match zoo_rt::clock(CLOCK_REALTIME) {
        Ok(ns) => {
            let secs = (ns / 1_000_000_000) as i64;
            let days = secs.div_euclid(86_400);
            let rem = secs.rem_euclid(86_400);
            let (month, day) = civil_from_days(days);
            let _ = write!(
                out,
                "{} {} {}  {:02}:{:02} UTC",
                WEEKDAYS[days.rem_euclid(7) as usize],
                day,
                MONTHS[month - 1],
                rem / 3600,
                rem % 3600 / 60
            );
        }
        Err(_) => {
            let _ = write!(out, "up {}s", zoo_rt::ticks() / 100);
        }
    }
}

/// The CPU's time-stamp counter.
/// Time-stamp-counter cycles per millisecond, measured over three timer
/// ticks (10 ms each) from a tick boundary.
fn calibrate_ms() -> u64 {
    let t0 = zoo_rt::ticks();
    while zoo_rt::ticks() == t0 {}
    let (c0, t1) = (cycles(), zoo_rt::ticks());
    while zoo_rt::ticks() < t1 + 3 {}
    (cycles().wrapping_sub(c0) / 30).max(1)
}

fn cycles() -> u64 {
    // SAFETY: `rdtsc` only reads the counter; user mode may run it (the
    // kernel does not set CR4.TSD).
    unsafe { core::arch::x86_64::_rdtsc() }
}

/// The font atlas from the initrd (`ui/fonts.zfnt`).
fn load_atlas() -> Option<Atlas<'static>> {
    let mut index = 0;
    while let Ok(e) = zoo_rt::fs_stat(index) {
        if e.name() == "ui/fonts.zfnt" {
            return Atlas::parse(zoo_rt::fs_map(index).ok()?);
        }
        index += 1;
    }
    None
}

/// The menu-bar labels after the application's name (they open in U4).
const MENUS: [&str; 5] = ["File", "Edit", "View", "Window", "Help"];

struct Banner {
    title: &'static str,
    body: Line,
    until: u64,
}

struct Desktop {
    screen: Screen,
    /// Each window's cached image (docs/u2-compositor-design.md).
    layers: [Layer; APPS],
    /// Scratch buffer for building layers (the render over white).
    scratch: Vec<u32>,
    /// The pointer is a hardware cursor: moving it needs no redraw
    /// (docs/gpu1-design.md).
    hw_cursor: bool,
    /// The hardware cursor has moved since it was last told.
    cursor_moved: bool,
    /// The hardware cursor is hidden: the pointer is drawn in the frame
    /// (during a drag or resize).
    cursor_hidden: bool,
    /// `cursor_hidden` changed: hide or show the hardware cursor after the
    /// next frame, so the pointer is never missing from the screen.
    cursor_switch: bool,
    /// The tick of the last frame (frame pacing).
    last_frame_tick: u64,
    /// The pointer image (`pointer_image`).
    pointer: Vec<u32>,
    /// Logical screen size: layout and the pointer use it.
    lw: usize,
    lh: usize,
    atlas: Atlas<'static>,
    dark: bool,
    /// Wallpaper colour per row, for the current appearance.
    wall: Vec<u32>,
    ramp: Vec<u8>,
    apps: [App; APPS],
    /// Home directory handle; copies go to the Assistant.
    home: u64,
    focus: usize,
    /// Window rectangles (x, y, w, h), indexed like `apps`.
    wins: [(usize, usize, usize, usize); APPS],
    /// Drawing order, back to front; the focused window is last.
    order: [usize; APPS],
    /// Pointer position.
    px: usize,
    py: usize,
    /// Left button held on the previous event.
    held: bool,
    /// Window being dragged, and the grab offset inside it.
    drag: Option<(usize, usize, usize)>,
    /// Window being resized from its bottom-right corner, and the
    /// pointer's offset from that corner (docs/d8-design.md).
    resize: Option<(usize, usize, usize)>,
    /// The last press on a title bar: (tick, x, y), for double-clicks.
    last_click: (u64, usize, usize),
    /// A zoomed window's rectangle before zooming (docs/d9-design.md).
    unzoomed: [Option<(usize, usize, usize, usize)>; APPS],
    /// Areas to redraw (x0, y0, x1, y1); `full` overrides them.
    damage: [(usize, usize, usize, usize); MAX_DAMAGE],
    damaged: usize,
    full: bool,
    /// Test mode (F9): repaint everything every frame, as before D2.
    always_full: bool,
    /// (cycles, ticks) at start-up: the time-stamp counter's rate.
    started: (u64, u64),
    /// Recent frames: (cycles, pixels drawn).
    frames: [(u64, u64); FRAME_SAMPLES],
    /// Of each frame, the cycles spent showing it (copy or flush).
    shows: [u64; FRAME_SAMPLES],
    frame_count: usize,
    /// The dock icon under the pointer (its name shows above it).
    hover: Option<usize>,
    /// Each window's running animation (docs/u2d-design.md).
    anims: [Option<Anim>; APPS],
    /// Time-stamp-counter cycles per millisecond (calibrated at start).
    per_ms: u64,
    /// The time this frame is drawn for (all damaged areas alike).
    frame_now: u64,
    banner: Option<Banner>,
}

impl Desktop {
    fn face(&self, id: FaceId) -> Face<'static> {
        self.atlas
            .face(id.scaled(self.screen.s))
            .expect("checked at start-up")
    }

    fn theme(&self) -> &'static Theme {
        if self.dark { &DARK } else { &LIGHT }
    }

    fn set_appearance(&mut self, dark: bool) {
        self.dark = dark;
        let t = self.theme();
        let h = self.screen.h.max(2);
        for (y, c) in self.wall[..h].iter_mut().enumerate() {
            *c = blend(t.wall_top, t.wall_bottom, (y * 255 / (h - 1)) as u8);
        }
        self.full = true;
        for l in &mut self.layers {
            l.dirty = true;
        }
        zoo_rt::log!("PeacockHandler: appearance {}", t.name);
    }

    /// Marks rectangle (x, y, w, h) for redrawing.
    fn damage(&mut self, x: usize, y: usize, w: usize, h: usize) {
        let sc = self.screen.s;
        let (x, y, w, h) = (x * sc, y * sc, w * sc, h * sc);
        let r = (
            x.min(self.screen.w),
            y.min(self.screen.h),
            (x + w).min(self.screen.w),
            (y + h).min(self.screen.h),
        );
        if r.0 >= r.2 || r.1 >= r.3 || self.full {
            return;
        }
        // Merge with an overlapping rectangle when the union costs no more
        // than both (a window moved a little: one area, not two).
        let area = |r: (usize, usize, usize, usize)| (r.2 - r.0) * (r.3 - r.1);
        let mut r = r;
        let mut k = 0;
        while k < self.damaged {
            let d = self.damage[k];
            let overlap = r.0 < d.2 && d.0 < r.2 && r.1 < d.3 && d.1 < r.3;
            let union = (r.0.min(d.0), r.1.min(d.1), r.2.max(d.2), r.3.max(d.3));
            if overlap && area(union) <= area(r) + area(d) {
                r = union;
                self.damaged -= 1;
                self.damage[k] = self.damage[self.damaged];
                k = 0;
                continue;
            }
            k += 1;
        }
        if self.damaged == MAX_DAMAGE {
            self.full = true;
            return;
        }
        self.damage[self.damaged] = r;
        self.damaged += 1;
    }

    /// A window, with its shadow, whose look changed: its layer is
    /// rebuilt.
    fn damage_window(&mut self, i: usize) {
        self.layers[i].dirty = true;
        self.damage_window_area(i);
    }

    /// A window's area only (it moved; its look is unchanged).
    fn damage_window_area(&mut self, i: usize) {
        let (x, y, w, h) = self.wins[i];
        self.damage(
            x.saturating_sub(MARGIN),
            y.saturating_sub(MARGIN),
            w + 2 * MARGIN,
            h + 2 * MARGIN,
        );
    }

    /// The pointer is drawn in the frame: no hardware cursor, or it is
    /// hidden during a drag.
    fn soft_pointer(&self) -> bool {
        !self.hw_cursor || self.cursor_hidden
    }

    fn damage_pointer(&mut self) {
        if self.hw_cursor {
            self.cursor_moved = true;
        }
        if self.soft_pointer() {
            self.damage(
                self.px.saturating_sub(POINTER_HOT),
                self.py.saturating_sub(POINTER_HOT),
                POINTER_W,
                POINTER_H,
            );
        }
    }

    fn damage_menu_bar(&mut self) {
        self.damage(0, 0, self.lw, MENU_H);
    }

    fn dock_rect(&self) -> (usize, usize, usize, usize) {
        zoo_abi::ui::dock_rect(self.lw, self.lh, APPS)
    }

    fn damage_dock(&mut self) {
        let (x, y, w, h) = self.dock_rect();
        // With the label above an icon.
        self.damage(x, y.saturating_sub(40), w, h + 40);
    }

    fn banner_rect(&self) -> (usize, usize, usize, usize) {
        (self.lw - 16 - BANNER_W, MENU_H + 10, BANNER_W, BANNER_H)
    }

    fn damage_banner(&mut self) {
        let (x, y, w, h) = self.banner_rect();
        self.damage(
            x - MARGIN,
            y.saturating_sub(MARGIN),
            w + 2 * MARGIN,
            h + 2 * MARGIN,
        );
    }

    fn set_focus(&mut self, i: usize) {
        if i != self.focus {
            self.damage_window(self.focus);
            self.damage_window(i);
            self.damage_menu_bar();
        } else if self.order[APPS - 1] != i {
            self.damage_window(i);
        }
        self.focus = i;
        let mut order = [0; APPS];
        let mut k = 0;
        for &j in self.order.iter().filter(|&&j| j != i) {
            order[k] = j;
            k += 1;
        }
        order[k] = i;
        self.order = order;
    }

    /// Topmost visible window under the pointer.
    fn window_at(&self, x: usize, y: usize) -> Option<usize> {
        self.order.iter().rev().copied().find(|&i| {
            let (wx, wy, ww, wh) = self.wins[i];
            self.apps[i].visible() && x >= wx && x < wx + ww && y >= wy && y < wy + wh
        })
    }

    /// The traffic light under the pointer in window `i`, if any.
    fn light_at(&self, i: usize) -> Option<usize> {
        (0..3).find(|&k| {
            let (cx, cy) = zoo_abi::ui::light_centre(self.wins[i], k);
            self.px.abs_diff(cx) <= LIGHT_D / 2 + 1 && self.py.abs_diff(cy) <= LIGHT_D / 2 + 1
        })
    }

    /// The green light or a title-bar double-click: fill the space between
    /// the menu bar and the dock, or go back to the size and place from
    /// before.
    fn toggle_zoom(&mut self, i: usize) {
        self.damage_window(i);
        self.drag = None;
        match self.unzoomed[i].take() {
            Some(r) => {
                self.wins[i] = r;
                zoo_rt::log!("PeacockHandler: restored {}", self.apps[i].label);
            }
            None => {
                self.unzoomed[i] = Some(self.wins[i]);
                self.wins[i] = zoo_abi::ui::zoomed(self.lw, self.lh);
                zoo_rt::log!("PeacockHandler: zoomed {}", self.apps[i].label);
            }
        }
        self.damage_window(i);
    }

    /// The pointer is in window `i`'s bottom-right resize corner.
    fn on_resize_corner(&self, i: usize) -> bool {
        let (x, y, w, h) = self.wins[i];
        self.px + RESIZE_CORNER >= x + w && self.py + RESIZE_CORNER >= y + h
    }

    /// The dock icon under the pointer.
    fn dock_slot(&self) -> Option<usize> {
        let (w, h) = (self.lw, self.lh);
        (0..APPS).find(|&k| {
            let (ix, iy) = zoo_abi::ui::dock_icon(w, h, APPS, k);
            self.px >= ix && self.px < ix + DOCK_ICON && self.py >= iy && self.py < iy + DOCK_ICON
        })
    }

    /// A dock icon: opens a closed application, shows a minimised one, or
    /// raises a running one.
    fn dock_click(&mut self, i: usize) {
        self.damage_dock();
        if self.apps[i].closed {
            let app = &mut self.apps[i];
            app.restarts = 0;
            if !app.start(self.home) {
                zoo_rt::log!("PeacockHandler: cannot start {}", app.program);
                return;
            }
            app.closed = false;
            zoo_rt::log!("PeacockHandler: opened {}", app.label);
            self.damage_window(i);
            self.begin(i, AnimKind::Open);
        } else if self.apps[i].minimized {
            self.apps[i].minimized = false;
            zoo_rt::log!("PeacockHandler: showed {}", self.apps[i].label);
            self.damage_window(i);
            self.begin(i, AnimKind::Restore);
        }
        self.set_focus(i);
    }

    /// The topmost other visible window gets the focus.
    fn focus_another(&mut self, i: usize) {
        if self.focus == i
            && let Some(j) = self
                .order
                .iter()
                .rev()
                .copied()
                .find(|&j| j != i && self.apps[j].visible())
        {
            self.set_focus(j);
        }
    }

    /// The red light: ends the application without restarting it and
    /// hides its window.
    fn close_app(&mut self, i: usize) {
        self.begin(i, AnimKind::Close);
        self.damage_window(i);
        self.damage_menu_bar();
        self.damage_dock();
        let app = &mut self.apps[i];
        if app.running {
            // The application sees its channel close and ends; one that
            // does not end in time is killed.
            let _ = close(app.chan);
            if wait_any(&[app.process], CLOSE_WAIT) == Err(Error::TimedOut) {
                zoo_rt::log!("PeacockHandler: {} did not end; killing it", app.label);
                let _ = kill(app.process);
            }
            let _ = wait(app.process);
            let _ = close(app.process);
        }
        app.running = false;
        app.closed = true;
        zoo_rt::log!("PeacockHandler: closed {}", app.label);
        self.focus_another(i);
    }

    /// The yellow light: hides the window; the dock shows it again.
    fn minimize(&mut self, i: usize) {
        self.begin(i, AnimKind::Minimize);
        self.damage_window(i);
        self.damage_menu_bar();
        self.apps[i].minimized = true;
        zoo_rt::log!("PeacockHandler: minimized {}", self.apps[i].label);
        self.focus_another(i);
    }

    /// Tab: the next visible window.
    fn focus_next(&mut self) {
        let n = self.apps.len();
        if let Some(j) = (1..n)
            .map(|k| (self.focus + k) % n)
            .find(|&j| self.apps[j].visible())
        {
            self.set_focus(j);
        }
    }

    /// Applies one mouse event; true if the screen changed.
    fn mouse(&mut self, e: MouseEvent) -> bool {
        let (w, h) = (self.lw, self.lh);
        self.damage_pointer();
        if e.buttons & zoo_abi::MOUSE_ABSOLUTE != 0 {
            // A tablet: a position across the screen, rounded to a pixel.
            let max = zoo_abi::MOUSE_ABS_MAX as usize;
            let at = |v: i16, n: usize| (v.max(0) as usize * (n - 1) + max / 2) / max;
            self.px = at(e.dx, w);
            self.py = at(e.dy, h);
        } else {
            // The mouse reports dy positive upwards; screen y grows downwards.
            self.px = (self.px as isize + e.dx as isize).clamp(0, w as isize - 1) as usize;
            self.py = (self.py as isize - e.dy as isize).clamp(0, h as isize - 1) as usize;
        }
        let down = e.buttons & 1 != 0;
        if down && !self.held {
            self.press();
        } else if down && let Some((i, ox, oy)) = self.resize {
            self.damage_window(i);
            let (x, y, _, _) = self.wins[i];
            // At least MIN_W x MIN_H, and inside the screen.
            let nw = (self.px + ox).saturating_sub(x).clamp(MIN_W, w - 1 - x);
            let nh = (self.py + oy).saturating_sub(y).clamp(MIN_H, h - 1 - y);
            self.wins[i] = (x, y, nw, nh);
            self.damage_window(i);
        } else if down {
            if let Some((i, ox, oy)) = self.drag {
                self.damage_window_area(i);
                let (_, _, ww, wh) = self.wins[i];
                // Keep part of the title bar reachable, below the menu bar.
                let x =
                    (self.px as isize - ox as isize).clamp(-(ww as isize) + 80, w as isize - 80);
                let y =
                    (self.py as isize - oy as isize).clamp(MENU_H as isize, (h - TITLE_H) as isize);
                self.wins[i] = (x.max(0) as usize, y as usize, ww, wh);
                self.damage_window_area(i);
            }
        } else if let Some((i, _, _)) = self.resize.take() {
            // Back to its layer.
            self.damage_window(i);
            let (_, _, nw, nh) = self.wins[i];
            zoo_rt::log!(
                "PeacockHandler: resized {} to {}x{}",
                self.apps[i].label,
                nw,
                nh
            );
        } else if let Some((i, _, _)) = self.drag.take() {
            let (x, y, _, _) = self.wins[i];
            zoo_rt::log!(
                "PeacockHandler: moved {} to ({}, {})",
                self.apps[i].label,
                x,
                y
            );
        }
        // While a window is dragged or resized, the pointer is drawn in
        // the same frame as the window, so it stays exactly where the
        // window was grabbed; the host's cursor, which would run ahead of
        // the window by a display refresh or two, is hidden meanwhile
        // (docs/u2c-design.md).
        let grabbing = self.drag.is_some() || self.resize.is_some();
        if self.hw_cursor && grabbing != self.cursor_hidden {
            self.damage_pointer();
            self.cursor_hidden = grabbing;
            self.cursor_switch = true;
        }
        self.held = down;
        self.damage_pointer();
        let hover = self.dock_slot();
        if hover != self.hover {
            self.hover = hover;
            self.damage_dock();
        }
        true
    }

    /// The left button went down.
    fn press(&mut self) {
        if let Some(i) = self.dock_slot() {
            self.dock_click(i);
            return;
        }
        if self.py < MENU_H {
            let mut clock = Line::new();
            write_clock(&mut clock);
            let cw = self
                .face(FaceId::Regular13)
                .width(clock.as_str())
                .div_ceil(self.screen.s);
            let (sx, sy) = zoo_abi::ui::appearance_switch(self.lw, cw);
            if self.px.abs_diff(sx) <= 9 && self.py.abs_diff(sy) <= 9 {
                self.set_appearance(!self.dark);
            }
            return;
        }
        let Some(i) = self.window_at(self.px, self.py) else {
            return;
        };
        match self.light_at(i) {
            Some(0) => return self.close_app(i),
            Some(1) => return self.minimize(i),
            Some(_) => {
                self.set_focus(i);
                return self.toggle_zoom(i);
            }
            None => {}
        }
        if i != self.focus {
            zoo_rt::log!("PeacockHandler: click focuses {}", self.apps[i].label);
        }
        self.set_focus(i);
        let (wx, wy, ww, wh) = self.wins[i];
        if self.py < wy + TITLE_H {
            let now = zoo_rt::ticks();
            let (t, lx, ly) = self.last_click;
            if now.saturating_sub(t) <= DOUBLE_CLICK
                && self.px.abs_diff(lx) <= 4
                && self.py.abs_diff(ly) <= 4
            {
                self.toggle_zoom(i);
                self.last_click = (0, 0, 0);
            } else {
                self.last_click = (now, self.px, self.py);
                self.drag = Some((i, self.px - wx, self.py - wy));
            }
        } else if self.on_resize_corner(i) {
            self.resize = Some((i, wx + ww - self.px, wy + wh - self.py));
        }
    }

    /// Redraws the damaged areas and shows them; records the frame's cost.
    fn render(&mut self) {
        if self.always_full {
            self.full = true;
        }
        self.frame_now = cycles();
        self.step_anims();
        let (w, h) = (self.screen.w, self.screen.h);
        let screen = (0, 0, w, h);
        let mut rects = self.damage;
        let mut n = self.damaged;
        // Many or large areas: one full repaint is simpler and no slower.
        let area: usize = rects[..n].iter().map(|r| (r.2 - r.0) * (r.3 - r.1)).sum();
        if self.full || area > w * h / 2 {
            rects[0] = screen;
            n = 1;
        }
        self.full = false;
        self.damaged = 0;
        if n == 0 {
            return;
        }
        let start = cycles();
        let resizing = self.resize.map(|r| r.0);
        for i in 0..APPS {
            if !self.apps[i].visible() || resizing == Some(i) {
                continue;
            }
            if self.layers[i].dirty {
                self.build_layer(i);
            } else if self.layers[i].content_dirty {
                self.refresh_content(i);
            }
        }
        for &r in &rects[..n] {
            self.screen.clip = r;
            self.draw();
        }
        let drawn = cycles();
        for &r in &rects[..n] {
            self.screen.present(r);
            if self.screen.flush {
                let _ = zoo_rt::display_flush(r.0, r.1, r.2 - r.0, r.3 - r.1);
            }
        }
        self.screen.clip = screen;
        let spent = cycles().wrapping_sub(start);
        let pixels = rects[..n]
            .iter()
            .map(|r| ((r.2 - r.0) * (r.3 - r.1)) as u64)
            .sum();
        self.frames[self.frame_count % FRAME_SAMPLES] = (spent, pixels);
        self.shows[self.frame_count % FRAME_SAMPLES] = cycles().wrapping_sub(drawn);
        self.frame_count += 1;
    }

    /// Logs the median cost of the recent frames (F11), then starts over.
    fn report_frames(&mut self) {
        let n = self.frame_count.min(FRAME_SAMPLES);
        let mut c = [0u64; FRAME_SAMPLES];
        let mut p = [0u64; FRAME_SAMPLES];
        for (k, &(cy, px)) in self.frames[..n].iter().enumerate() {
            c[k] = cy;
            p[k] = px;
        }
        let mut sh = self.shows;
        c[..n].sort_unstable();
        p[..n].sort_unstable();
        sh[..n].sort_unstable();
        zoo_rt::log!(
            "PeacockHandler: frames {} mode {} median {} cycles {} px (showing {}, longest {})",
            self.frame_count,
            if self.always_full { "full" } else { "damage" },
            if n > 0 { c[n / 2] } else { 0 },
            if n > 0 { p[n / 2] } else { 0 },
            if n > 0 { sh[n / 2] } else { 0 },
            if n > 0 { c[n - 1] } else { 0 }
        );
        self.frame_count = 0;
        if let Ok([n, sum, max]) = zoo_rt::input_latency() {
            let mean = sum.checked_div(n).unwrap_or(0);
            let ticks = zoo_rt::ticks().saturating_sub(self.started.1).max(1);
            let per_tick = (cycles().wrapping_sub(self.started.0) / ticks).max(1);
            zoo_rt::log!(
                "PeacockHandler: input wait {} events mean {} max {} cycles; per mille of a tick: mean {} longest {}",
                n,
                mean,
                max,
                mean * 1000 / per_tick,
                max * 1000 / per_tick
            );
        }
    }

    /// Draws everything, limited to the screen's clip rectangle.
    fn draw(&mut self) {
        let (cx0, cy0, cx1, cy1) = self.screen.clip;
        for y in cy0..cy1 {
            let c = self.wall[y];
            self.screen.back[y * self.screen.w + cx0..y * self.screen.w + cx1].fill(c);
        }
        for k in 0..APPS {
            let i = self.order[k];
            if let Some(a) = &self.anims[i] {
                let (rect, alpha, _) = a.at(self.frame_now);
                let l = a.snapshot.as_ref().unwrap_or(&self.layers[i]);
                let (sw, clip, s) = (self.screen.w, self.screen.clip, self.screen.s);
                composite_scaled(self.screen.back, sw, clip, s, l, a.size, rect, alpha);
                continue;
            }
            if !self.apps[i].visible() {
                continue;
            }
            // A window being resized changes every frame: drawn directly,
            // its layer rebuilt when the resize ends.
            if self.resize.is_some_and(|r| r.0 == i) {
                self.draw_window(i);
            } else {
                self.composite(i);
            }
        }
        self.draw_menu_bar();
        self.draw_banner();
        self.draw_dock();
        if self.soft_pointer() {
            self.draw_pointer();
        }
    }

    /// Draws window `i` into its layer: once over black and once over
    /// white, which gives each pixel's colour (premultiplied) and alpha.
    fn build_layer(&mut self, i: usize) {
        let (x, y, w, h) = self.wins[i];
        let sc = self.screen.s;
        let (lw, lh) = ((w + 2 * MARGIN) * sc, (h + 2 * MARGIN) * sc);
        let n = lw * lh;
        let mut black = core::mem::take(&mut self.layers[i].buf);
        let mut white = core::mem::take(&mut self.scratch);
        black.clear();
        black.resize(n, 0);
        white.clear();
        white.resize(n, 0xFF_FFFF);
        self.wins[i] = (MARGIN, MARGIN, w, h);
        self.draw_window_into(i, &mut black, lw, lh);
        self.draw_window_into(i, &mut white, lw, lh);
        self.wins[i] = (x, y, w, h);
        let mut spans = core::mem::take(&mut self.layers[i].spans);
        spans.clear();
        for row in 0..lh {
            // The first run of opaque pixels in the row.
            let (mut s0, mut s1) = (None::<u32>, 0u32);
            for col in 0..lw {
                let k = row * lw + col;
                let (b, wt) = (black[k], white[k]);
                // Over black: a·c; over white: a·c + (1 - a)·255. The
                // difference gives the alpha (green channel).
                let a = 255 - (((wt >> 8) & 0xFF).saturating_sub((b >> 8) & 0xFF));
                // Each channel at most `a` (true up to rounding), so `over`
                // never carries between channels.
                let ch = |shift: u32| ((b >> shift) & 0xFF).min(a) << shift;
                black[k] = a << 24 | ch(16) | ch(8) | ch(0);
                if a == 255 {
                    if s0.is_none() {
                        (s0, s1) = (Some(col as u32), col as u32 + 1);
                    } else if s1 == col as u32 {
                        s1 += 1;
                    }
                }
            }
            spans.push(s0.map_or((0, 0), |s0| (s0, s1)));
        }
        self.scratch = white;
        let l = &mut self.layers[i];
        l.buf = black;
        l.spans = spans;
        (l.w, l.h) = (lw, lh);
        l.dirty = false;
        l.content_dirty = false;
        l.pending.clear();
    }

    /// Starts animation `kind` for window `i` (docs/u2d-design.md). A
    /// closing or minimising window's image is kept for it.
    fn begin(&mut self, i: usize, kind: AnimKind) {
        let (x, y, w, h) = self.wins[i];
        let win: RectF = (x as f32, y as f32, w as f32, h as f32);
        // Slightly smaller, about the same centre.
        let shrink = |k: f32| -> RectF {
            (
                win.0 + win.2 * (1.0 - k) / 2.0,
                win.1 + win.3 * (1.0 - k) / 2.0,
                win.2 * k,
                win.3 * k,
            )
        };
        // The dock icon, with the window's proportions.
        let icon: RectF = {
            let (ix, iy) = zoo_abi::ui::dock_icon(self.lw, self.lh, APPS, i);
            let d = DOCK_ICON as f32;
            let ih = d * win.3 / win.2.max(1.0);
            (ix as f32, iy as f32 + (d - ih) / 2.0, d, ih)
        };
        let (from, to, a0, a1) = match kind {
            AnimKind::Open => (shrink(0.92), win, 0.0, 1.0),
            AnimKind::Close => (win, shrink(0.92), 1.0, 0.0),
            AnimKind::Minimize => (win, icon, 1.0, 0.25),
            AnimKind::Restore => (icon, win, 0.25, 1.0),
        };
        let snapshot = match kind {
            AnimKind::Close | AnimKind::Minimize => {
                if self.layers[i].dirty {
                    self.build_layer(i);
                }
                Some(self.layers[i].clone())
            }
            _ => None,
        };
        // An animation it replaces still has an area on screen to clean.
        let last = self.anims[i]
            .as_ref()
            .map_or(layer_bounds(from, (w, h)), |a| a.last);
        self.anims[i] = Some(Anim {
            kind,
            start: cycles(),
            dur: kind.millis() * self.per_ms,
            from,
            to,
            a0,
            a1,
            snapshot,
            size: (w, h),
            last,
            frames: 0,
        });
    }

    /// Damages where each animation drew last and where it draws now, and
    /// ends the finished ones (they are drawn normally from then on, or
    /// not at all).
    fn step_anims(&mut self) {
        let now = self.frame_now;
        for i in 0..APPS {
            let Some(a) = self.anims[i].as_mut() else {
                continue;
            };
            let (rect, _, done) = a.at(now);
            let (last, size, kind, start) = (a.last, a.size, a.kind, a.start);
            let bounds = layer_bounds(rect, size);
            a.last = bounds;
            a.frames += 1;
            let frames = a.frames;
            self.damage(last.0, last.1, last.2, last.3);
            self.damage(bounds.0, bounds.1, bounds.2, bounds.3);
            if done {
                self.anims[i] = None;
                if self.apps[i].visible() {
                    self.damage_window_area(i);
                }
                zoo_rt::log!(
                    "PeacockHandler: animation {} {}: frames {} time {} ms",
                    kind.name(),
                    self.apps[i].label,
                    frames,
                    now.saturating_sub(start) / self.per_ms.max(1)
                );
            }
        }
    }

    /// Whether any animation runs.
    fn animating(&self) -> bool {
        self.anims.iter().any(Option::is_some)
    }

    /// Window `i`'s content area (x, y, w, h): below the title bar, inside
    /// the outline.
    fn content_rect(&self, i: usize) -> (usize, usize, usize, usize) {
        let (x, y, w, h) = self.wins[i];
        (
            x + 1,
            y + TITLE_H,
            w.saturating_sub(2),
            h.saturating_sub(TITLE_H + 1),
        )
    }

    /// The content of window `i` changed (an application's update): the
    /// rows of the lines that changed are drawn again.
    fn damage_content(&mut self, i: usize) {
        let mask = core::mem::take(&mut self.apps[i].changed);
        let (x, y, w, h) = self.wins[i];
        let (_, cy, cw, ch) = self.content_rect(i);
        let bottom = cy + ch - y;
        let mut k = 0;
        while k < MAX_LINES {
            if mask & (1 << k) == 0 {
                k += 1;
                continue;
            }
            // A run of changed lines k..end.
            let mut end = k;
            while end < MAX_LINES && mask & (1 << end) != 0 {
                end += 1;
            }
            let y0 = zoo_abi::ui::line_top((0, 0, w, h), k).min(bottom);
            let y1 = zoo_abi::ui::line_top((0, 0, w, h), end).min(bottom);
            if y0 < y1 {
                if !self.layers[i].dirty {
                    self.layers[i].content_dirty = true;
                    self.layers[i].pending.push((y0, y1));
                }
                self.damage(x + 1, y + y0, cw, y1 - y0);
            }
            k = end;
        }
    }

    /// Redraws the content area into window `i`'s layer, once (no
    /// second pass: the area is opaque). Only pixels the layer already
    /// has as opaque are replaced, so the rounded corners keep their
    /// alpha.
    fn refresh_content(&mut self, i: usize) {
        let (x, y, w, h) = self.wins[i];
        let (lw, lh) = (self.layers[i].w, self.layers[i].h);
        let sc = self.screen.s;
        if (lw, lh) != ((w + 2 * MARGIN) * sc, (h + 2 * MARGIN) * sc) {
            return self.build_layer(i);
        }
        self.wins[i] = (MARGIN, MARGIN, w, h);
        let (cx, _, cw, _) = self.content_rect(i);
        let (cx, cw) = (cx * sc, cw * sc);
        let mut scratch = core::mem::take(&mut self.scratch);
        // Every pixel used is drawn first; old contents do not matter.
        scratch.resize(lw * lh, 0);
        let pending = core::mem::take(&mut self.layers[i].pending);
        for &(y0, y1) in &pending {
            let (r0, r1) = ((MARGIN + y0) * sc, (MARGIN + y1) * sc);
            self.draw_window_clipped(i, &mut scratch, lw, lh, (cx, r0, cx + cw, r1));
            let l = &mut self.layers[i];
            for row in r0..r1 {
                let at = row * lw;
                for (d, &c) in l.buf[at + cx..at + cx + cw]
                    .iter_mut()
                    .zip(&scratch[at + cx..at + cx + cw])
                {
                    if *d >> 24 == 0xFF {
                        *d = 0xFF00_0000 | (c & 0xFF_FFFF);
                    }
                }
            }
        }
        self.wins[i] = (x, y, w, h);
        let l = &mut self.layers[i];
        l.pending = pending;
        l.pending.clear();
        l.content_dirty = false;
        self.scratch = scratch;
    }

    /// `draw_window(i)` into `buf` (`w` x `h`) instead of the screen.
    fn draw_window_into(&mut self, i: usize, buf: &mut [u32], w: usize, h: usize) {
        self.draw_window_clipped(i, buf, w, h, (0, 0, w, h));
    }

    /// As `draw_window_into`, limited to `clip` (x0, y0, x1, y1).
    fn draw_window_clipped(
        &mut self,
        i: usize,
        buf: &mut [u32],
        w: usize,
        h: usize,
        clip: (usize, usize, usize, usize),
    ) {
        // SAFETY: `buf` stays borrowed for this whole call and nothing else
        // uses it; the slice is swapped back out of `screen` before the
        // call returns, so it never outlives `buf`.
        let tmp: &'static mut [u32] =
            unsafe { core::slice::from_raw_parts_mut(buf.as_mut_ptr(), buf.len()) };
        let back = core::mem::replace(&mut self.screen.back, tmp);
        let saved = (self.screen.w, self.screen.h, self.screen.clip);
        (self.screen.w, self.screen.h, self.screen.clip) = (w, h, clip);
        self.draw_window(i);
        (self.screen.w, self.screen.h, self.screen.clip) = saved;
        self.screen.back = back;
    }

    /// Window `i`'s layer over the screen, within the clip rectangle.
    fn composite(&mut self, i: usize) {
        let l = &self.layers[i];
        if l.w == 0 {
            return;
        }
        let (x, y, _, _) = self.wins[i];
        let sc = self.screen.s as isize;
        let (ox, oy) = (
            (x as isize - MARGIN as isize) * sc,
            (y as isize - MARGIN as isize) * sc,
        );
        let (cx0, cy0, cx1, cy1) = self.screen.clip;
        let ly0 = (cy0 as isize - oy).max(0) as usize;
        let ly1 = (cy1 as isize - oy).clamp(0, l.h as isize) as usize;
        let lx0 = (cx0 as isize - ox).max(0) as usize;
        let lx1 = (cx1 as isize - ox).clamp(0, l.w as isize) as usize;
        if lx0 >= lx1 || ly0 >= ly1 {
            return;
        }
        let sw = self.screen.w;
        // The screen column of layer column `lx0`.
        let sx0 = (ox + lx0 as isize) as usize;
        for ly in ly0..ly1 {
            let src = &l.buf[ly * l.w + lx0..ly * l.w + lx1];
            let sy = (oy + ly as isize) as usize;
            let dst = &mut self.screen.back[sy * sw + sx0..sy * sw + sx0 + (lx1 - lx0)];
            // The opaque span, relative to `lx0`.
            let (s0, s1) = l.spans[ly];
            let o0 = (s0 as usize).clamp(lx0, lx1) - lx0;
            let o1 = ((s1 as usize).clamp(lx0, lx1) - lx0).max(o0);
            for (d, &c) in dst[..o0].iter_mut().zip(&src[..o0]) {
                *d = over(*d, c);
            }
            for (d, &c) in dst[o0..o1].iter_mut().zip(&src[o0..o1]) {
                *d = c & 0xFF_FFFF;
            }
            for (d, &c) in dst[o1..].iter_mut().zip(&src[o1..]) {
                *d = over(*d, c);
            }
        }
    }

    fn draw_menu_bar(&mut self) {
        let t = self.theme();
        let w = self.lw;
        if self.screen.clip.1 >= MENU_H * self.screen.s {
            return;
        }
        self.screen.fill(0, 0, w, MENU_H, zoo_abi::ui::bar_color(t));
        let (bold, regular) = (self.face(FaceId::SemiBold13), self.face(FaceId::Regular13));
        let base = 18;
        self.draw_paw(18, 13, t.bar_text);
        let app = &self.apps[self.focus];
        let name = if app.visible() { app.label } else { "Finder" };
        let mut x = self.screen.text(&bold, 40, base, name, t.bar_text, w / 2) + 22;
        for m in MENUS {
            x = self.screen.text(&regular, x, base, m, t.bar_text, w / 2) + 20;
        }
        let mut clock = Line::new();
        write_clock(&mut clock);
        let cw = regular.width(clock.as_str()).div_ceil(self.screen.s);
        self.screen
            .text(&regular, w - 16 - cw, base, clock.as_str(), t.bar_text, w);
        // The appearance switch: a disc, half filled.
        let (sx, sy) = zoo_abi::ui::appearance_switch(w, cw);
        let (bx, by) = (sx - 7, sy - 7);
        self.screen.disc(bx, by, 14, t.bar_text, t.bar_text);
        self.screen
            .disc(bx + 2, by + 2, 10, zoo_abi::ui::bar_color(t), t.bar_text);
        // The right half filled: dark shown as filled.
        let sc = self.screen.s;
        let (d, x0, y0) = (10 * sc, (bx + 2) * sc, (by + 2) * sc);
        for dy in 0..d {
            for dx in d / 2..d {
                let (qx, qy) = (dx.min(d - 1 - dx), dy.min(d - 1 - dy));
                let c = corner_coverage(d / 2, qx, qy);
                self.screen.blend_dev(x0 + dx, y0 + dy, t.bar_text, c);
            }
        }
    }

    /// A small paw print: the ZooOS mark.
    fn draw_paw(&mut self, cx: usize, cy: usize, c: u32) {
        let s = &mut self.screen;
        s.disc(cx - 4, cy - 1, 9, c, c);
        for (dx, dy) in [(-7isize, -5isize), (-3, -9), (2, -9), (6, -5)] {
            let (x, y) = ((cx as isize + dx - 2) as usize, (cy as isize + dy) as usize);
            s.disc(x, y, 5, c, c);
        }
    }

    fn draw_window(&mut self, i: usize) {
        let t = self.theme();
        let focused = i == self.focus;
        let (x, y, w, h) = self.wins[i];
        let (cx0, cy0, cx1, cy1) = self.screen.clip;
        let s = self.screen.s;
        if (x + w + MARGIN) * s <= cx0
            || x * s >= cx1 + MARGIN * s
            || (y + h + MARGIN) * s <= cy0
            || y * s >= cy1 + MARGIN * s
        {
            return;
        }
        self.screen.shadow(
            (x, y, w, h),
            if focused { t.shadow } else { t.shadow_blurred },
            &self.ramp,
        );
        let all = [true; 4];
        self.screen
            .round_rect(x, y, w, h, RADIUS, t.content, 255, all);
        let title = if focused { t.title } else { t.title_blurred };
        self.screen.round_rect(
            x,
            y,
            w,
            TITLE_H,
            RADIUS,
            title,
            255,
            [true, true, false, false],
        );
        self.screen.fill(x, y + TITLE_H - 1, w, 1, t.separator);
        self.screen.round_outline(x, y, w, h, RADIUS, t.outline);
        for (k, &colours) in LIGHTS.iter().enumerate() {
            let (lx, ly) = zoo_abi::ui::light_centre(self.wins[i], k);
            let (fill, rim) = if focused {
                colours
            } else {
                (t.light_off, t.light_off)
            };
            self.screen
                .disc(lx - LIGHT_D / 2, ly - LIGHT_D / 2, LIGHT_D, fill, rim);
        }
        let medium = self.face(FaceId::Medium13);
        let regular = self.face(FaceId::Regular13);
        let app = &self.apps[i];
        let tw = medium.width(app.title.as_str()).div_ceil(self.screen.s);
        let lights_end = x + zoo_abi::ui::LIGHT_X + 2 * zoo_abi::ui::LIGHT_GAP + 16;
        let tx = (x + w.saturating_sub(tw) / 2).max(lights_end);
        let tc = if focused {
            t.title_text
        } else {
            t.title_text_blurred
        };
        self.screen
            .text(&medium, tx, y + 18, app.title.as_str(), tc, x + w - 12);
        let right = x + w - TEXT_X;
        if !app.running {
            let top = zoo_abi::ui::line_top(self.wins[i], 0);
            self.screen.text(
                &regular,
                x + TEXT_X,
                top + BASELINE,
                "Not running",
                t.text_dim,
                right,
            );
            return;
        }
        for li in 0..app.count {
            let top = zoo_abi::ui::line_top(self.wins[i], li);
            if top + LINE_H > y + h - 4 {
                break;
            }
            let selected = app.selected == Some(li);
            let fg = if selected && focused {
                t.select_text
            } else {
                t.text
            };
            if selected {
                let bg = if focused { t.select } else { t.select_blurred };
                self.screen
                    .round_rect(x + 8, top, w - 16, LINE_H, 5, bg, 255, all);
            }
            self.screen.text(
                &regular,
                x + TEXT_X,
                top + BASELINE,
                self.apps[i].lines[li].as_str(),
                fg,
                right,
            );
        }
    }

    fn draw_banner(&mut self) {
        let Some(b) = &self.banner else { return };
        let (title, body) = (b.title, b.body.as_str());
        let t = self.theme();
        let (x, y, w, h) = self.banner_rect();
        if !self.screen.visible(
            x - MARGIN,
            y.saturating_sub(MARGIN),
            w + 2 * MARGIN,
            h + 2 * MARGIN,
        ) {
            return;
        }
        self.screen
            .shadow((x, y, w, h), t.shadow_blurred, &self.ramp);
        self.screen
            .round_rect(x, y, w, h, 12, t.banner, 255, [true; 4]);
        self.screen.round_outline(x, y, w, h, 12, t.outline);
        let bold = self.face(FaceId::SemiBold13);
        let regular = self.face(FaceId::Regular13);
        self.screen
            .text(&bold, x + 16, y + 24, title, t.text, x + w - 12);
        self.screen
            .text(&regular, x + 16, y + 44, body, t.text, x + w - 12);
    }

    fn draw_dock(&mut self) {
        let t = self.theme();
        let (sw, sh) = (self.lw, self.lh);
        let (dx, dy, dw, dh) = self.dock_rect();
        // With the name shown above an icon.
        if !self.screen.visible(dx, dy.saturating_sub(40), dw, dh + 40) {
            return;
        }
        self.screen
            .round_rect(dx, dy, dw, dh, 18, t.dock_tint, t.dock_alpha, [true; 4]);
        self.screen.round_outline(dx, dy, dw, dh, 18, t.outline);
        for k in 0..APPS {
            let (ix, iy) = zoo_abi::ui::dock_icon(sw, sh, APPS, k);
            self.draw_icon(k, ix, iy);
            if self.apps[k].running {
                let c = t.bar_text;
                self.screen
                    .disc(ix + DOCK_ICON / 2 - 2, iy + DOCK_ICON + 2, 4, c, c);
            }
        }
        if let Some(k) = self.hover {
            let small = self.face(FaceId::Regular13);
            let label = self.apps[k].label;
            let lw = small.width(label).div_ceil(self.screen.s) + 20;
            let (ix, iy) = zoo_abi::ui::dock_icon(sw, sh, APPS, k);
            let lx = (ix + DOCK_ICON / 2).saturating_sub(lw / 2);
            let ly = iy - 10 - 26;
            self.screen
                .round_rect(lx, ly, lw, 24, 6, t.tooltip, 255, [true; 4]);
            self.screen.round_outline(lx, ly, lw, 24, 6, t.outline);
            self.screen
                .text(&small, lx + 10, ly + 17, label, t.tooltip_text, lx + lw);
        }
    }

    /// An application's dock icon: a rounded square in its colour with a
    /// simple white mark.
    fn draw_icon(&mut self, k: usize, x: usize, y: usize) {
        let s = &mut self.screen;
        let d = DOCK_ICON;
        if !s.visible(x, y, d, d) {
            return;
        }
        s.round_rect(x, y, d, d, 12, ICON[k], 255, [true; 4]);
        // A lighter top half, as if lit from above.
        s.round_rect(x, y, d, d / 2, 12, 0xFFFFFF, 28, [true, true, false, false]);
        let white = 0xFFFFFF;
        match k {
            // Files: a folder.
            0 => {
                s.round_rect(
                    x + 11,
                    y + 16,
                    14,
                    6,
                    2,
                    white,
                    230,
                    [true, true, false, false],
                );
                s.round_rect(x + 11, y + 20, 30, 20, 3, white, 255, [true; 4]);
            }
            // Crash Test: an exclamation mark.
            1 => {
                s.round_rect(x + d / 2 - 3, y + 12, 6, 20, 3, white, 255, [true; 4]);
                s.disc(x + d / 2 - 3, y + 36, 6, white, white);
            }
            // Assistant: a four-pointed sparkle.
            2 => {
                let (cx, cy) = (x + d / 2, y + d / 2);
                for r in 0..16usize {
                    let half = (16 - r) / 4;
                    s.fill(cx - half, cy - r, 2 * half + 1, 1, white);
                    s.fill(cx - half, cy + r, 2 * half + 1, 1, white);
                    s.fill(cx - r, cy - half, 1, 2 * half + 1, white);
                    s.fill(cx + r, cy - half, 1, 2 * half + 1, white);
                }
            }
            // Notes: a page with lines.
            _ => {
                s.round_rect(x + 10, y + 16, d - 20, d - 26, 4, white, 255, [true; 4]);
                for l in 0..4 {
                    s.fill(x + 16, y + 24 + l * 6, d - 32, 2, 0xC9C9CE);
                }
            }
        }
    }

    /// The pointer as a hardware-cursor image (0xAARRGGBB, not
    /// premultiplied): `self.pointer` in the cursor's 64x64 frame.
    /// (On the heap: 16 KiB is too much for the window server's stack.)
    fn cursor_image(&self) -> Vec<u32> {
        let mut img = alloc::vec![0u32; zoo_abi::CURSOR_SIZE * zoo_abi::CURSOR_SIZE];
        let pw = POINTER_W * self.screen.s;
        for (r, row) in self.pointer.chunks(pw).enumerate() {
            img[r * zoo_abi::CURSOR_SIZE..r * zoo_abi::CURSOR_SIZE + pw].copy_from_slice(row);
        }
        img
    }

    /// Hides the hardware cursor (an empty image) or shows the arrow again,
    /// at the pointer.
    fn apply_cursor(&mut self) {
        let s = self.screen.s;
        let at = (self.px * s, self.py * s);
        let hot = (POINTER_HOT * s, POINTER_HOT * s);
        let image = if self.cursor_hidden {
            alloc::vec![0u32; zoo_abi::CURSOR_SIZE * zoo_abi::CURSOR_SIZE]
        } else {
            self.cursor_image()
        };
        let _ = zoo_rt::cursor_set(&image, hot, at);
        zoo_rt::log!(
            "PeacockHandler: pointer {}",
            if self.cursor_hidden {
                "drawn in the frame"
            } else {
                "hardware"
            }
        );
    }

    /// The pointer image blended over the screen, its hot spot at the
    /// pointer.
    fn draw_pointer(&mut self) {
        let sc = self.screen.s;
        let (x0, y0) = (
            ((self.px as isize) - POINTER_HOT as isize) * sc as isize,
            ((self.py as isize) - POINTER_HOT as isize) * sc as isize,
        );
        let pw = POINTER_W * sc;
        for (r, row) in self.pointer.chunks(pw).enumerate() {
            for (c, &p) in row.iter().enumerate() {
                let (x, y) = (x0 + c as isize, y0 + r as isize);
                if p >> 24 != 0 && x >= 0 && y >= 0 {
                    self.screen
                        .blend_dev(x as usize, y as usize, p & 0xFF_FFFF, (p >> 24) as u8);
                }
            }
        }
    }

    fn send_key(&self, key: Key) {
        let app = &self.apps[self.focus];
        if app.running && app.visible() {
            let mut l = Line::new();
            key.encode(&mut l);
            // A full queue drops the key; the app is busy or stuck.
            let _ = zoo_rt::send(app.chan, l.as_str().as_bytes());
        }
    }

    /// Handles an application whose channel closed: it has ended.
    fn app_ended(&mut self, i: usize) {
        self.damage_window(i);
        self.damage_dock();
        self.damage_banner();
        if i == self.focus {
            self.damage_menu_bar();
        }
        let app = &mut self.apps[i];
        app.running = false;
        let _ = close(app.chan);
        let outcome = wait(app.process);
        let _ = close(app.process);
        let mut status = Line::new();
        let mut body = Line::new();
        if app.restarts < APP_RESTARTS {
            app.restarts += 1;
            let how = if outcome == Err(Error::Crashed) {
                "crashed"
            } else {
                "ended"
            };
            let _ = write!(
                status,
                "{} {}; restarted ({} of {})",
                app.label, how, app.restarts, APP_RESTARTS
            );
            let _ = write!(
                body,
                "It {} and was restarted ({} of {}).",
                how, app.restarts, APP_RESTARTS
            );
            app.start(self.home);
        } else {
            let _ = write!(status, "{} ended; restart limit reached", app.label);
            let _ = write!(body, "It ended; restart limit reached.");
        }
        zoo_rt::log!("PeacockHandler: {}", status.as_str());
        self.banner = Some(Banner {
            title: app.label,
            body,
            until: zoo_rt::ticks() + BANNER_TICKS,
        });
        self.damage_banner();
    }
}

fn main(_start_handle: u64, home: u64) -> u8 {
    let info = match display_acquire() {
        Ok(i) => i,
        Err(e) => {
            zoo_rt::log!("PeacockHandler: cannot acquire the display: {}", e.name());
            return 2;
        }
    };
    let (full_w, full_h) = (info.width as usize, info.height as usize);
    if info.bytes_per_pixel != 4 || full_w < 640 || full_h < 400 {
        zoo_rt::log!(
            "PeacockHandler: unsupported display {}x{}x{}",
            full_w,
            full_h,
            info.bytes_per_pixel
        );
        return 3;
    }
    let Some(atlas) = load_atlas() else {
        let _ = log("PeacockHandler: no font atlas (ui/fonts.zfnt) in the initrd");
        return 4;
    };
    if [
        FaceId::Regular13,
        FaceId::Medium13,
        FaceId::SemiBold13,
        FaceId::Regular11,
    ]
    .iter()
    .any(|&f| atlas.face(f).is_none())
    {
        let _ = log("PeacockHandler: the font atlas lacks a face");
        return 4;
    }
    // 4K-class screens are drawn at 2x: the same layout with twice the
    // pixels in each direction, like a Retina display (docs/u2b-design.md).
    let sc = if full_w >= 3200 && full_h >= 1800 {
        2
    } else {
        1
    };
    // The logical size, at most MAX_W x MAX_H, and the device area used.
    let (w, h) = ((full_w / sc).min(MAX_W), (full_h / sc).min(MAX_H));
    let (dw, dh) = (w * sc, h * sc);
    // A screen larger than that: the desktop uses its top-left part and
    // the rest is painted once (docs/usb-boot.md).
    if (dw, dh) != (full_w, full_h) {
        zoo_rt::log!(
            "PeacockHandler: display {}x{}: using {}x{}",
            full_w,
            full_h,
            dw,
            dh
        );
        let fb = info.base as *mut u32;
        for y in 0..full_h {
            let from = if y < dh { dw } else { 0 };
            for x in from..full_w {
                // SAFETY: (x, y) is inside the mapped framebuffer
                // (`stride * height` pixels of 4 bytes).
                unsafe { fb.add(y * info.stride as usize + x).write_volatile(0) };
            }
        }
    } else {
        zoo_rt::log!("PeacockHandler: display {}x{} acquired", dw, dh);
    }
    if sc > 1 {
        zoo_rt::log!("PeacockHandler: {}x scale, {}x{} logical", sc, w, h);
    }
    // A display that shows changes only when flushed, as wide as its
    // stride: draw into it directly (docs/u2-compositor-design.md).
    let direct = info.flags & zoo_abi::DISPLAY_FLUSH_NEEDED != 0
        && info.stride as usize == dw
        && (dw, dh) == (full_w, full_h);
    let back: &'static mut [u32] = if direct {
        // SAFETY: the kernel mapped `stride * height` pixels of 4 bytes at
        // `base` for this process alone (`display_acquire`), and the
        // mapping lasts as long as the process.
        unsafe { core::slice::from_raw_parts_mut(info.base as *mut u32, dw * dh) }
    } else {
        // The back buffer, for the life of the process.
        alloc::boxed::Box::leak(alloc::vec![0u32; dw * dh].into_boxed_slice())
    };
    let mut desk = Desktop {
        layers: [const { Layer::new() }; APPS],
        scratch: Vec::new(),
        hw_cursor: false,
        cursor_moved: false,
        cursor_hidden: false,
        cursor_switch: false,
        last_frame_tick: u64::MAX,
        pointer: pointer_image(sc),
        lw: w,
        lh: h,
        screen: Screen {
            w: dw,
            h: dh,
            s: sc,
            stride: info.stride as usize,
            fb: info.base as *mut u32,
            bgr: info.format == FORMAT_BGR,
            flush: info.flags & zoo_abi::DISPLAY_FLUSH_NEEDED != 0,
            direct,
            back: &mut back[..dw * dh],
            clip: (0, 0, dw, dh),
        },
        atlas,
        dark: false,
        wall: alloc::vec![0; dh],
        ramp: shadow_ramp(sc),
        apps: [
            App::new("Files", "files", true),
            App::new("Crash Test", "crash-test", false),
            App::new("Assistant", "assistant", true),
            App::new("Notes", "notes", true),
        ],
        home,
        focus: 0,
        wins: zoo_abi::ui::initial_windows(w, h),
        order: [3, 1, 2, 0],
        px: w / 2,
        py: h / 2,
        held: false,
        drag: None,
        resize: None,
        last_click: (0, 0, 0),
        unzoomed: [None; APPS],
        damage: [(0, 0, 0, 0); MAX_DAMAGE],
        damaged: 0,
        full: true,
        always_full: false,
        started: (cycles(), zoo_rt::ticks()),
        frames: [(0, 0); FRAME_SAMPLES],
        shows: [0; FRAME_SAMPLES],
        frame_count: 0,
        hover: None,
        anims: [const { None }; APPS],
        per_ms: calibrate_ms(),
        frame_now: 0,
        banner: None,
    };
    desk.set_appearance(false);
    if info.flags & zoo_abi::DISPLAY_HW_CURSOR != 0 {
        let image = desk.cursor_image();
        let at = (desk.px * sc, desk.py * sc);
        if zoo_rt::cursor_set(&image, (POINTER_HOT * sc, POINTER_HOT * sc), at).is_ok() {
            desk.hw_cursor = true;
            let _ = log("PeacockHandler: hardware cursor");
        }
    }
    // Notes opens from the dock (docs/d4-design.md).
    desk.apps[NOTES].closed = true;
    for app in desk.apps.iter_mut().filter(|a| !a.closed) {
        if !app.start(home) {
            zoo_rt::log!("PeacockHandler: cannot start {}", app.program);
        }
    }
    // The windows open with an animation (docs/u2d-design.md).
    for i in 0..APPS {
        if desk.apps[i].visible() {
            desk.begin(i, AnimKind::Open);
        }
    }
    desk.render();

    let mut decoder = PS2Keyboard::new(ScancodeSet1::new(), Us104Key, HandleControl::Ignore);
    let mut kb = [0u8; 32];
    let mut msg = [0u8; MAX_MESSAGE];
    let mut last_second = zoo_rt::ticks() / 100;
    loop {
        let mut dirty = false;

        // Keyboard.
        let n = input_read(&mut kb).unwrap_or(0);
        for &byte in &kb[..n] {
            let Ok(Some(event)) = decoder.add_byte(byte) else {
                continue;
            };
            match decoder.process_keyevent(event) {
                Some(DecodedKey::Unicode('\t')) => {
                    desk.focus_next();
                    dirty = true;
                }
                Some(DecodedKey::Unicode('\u{1b}')) => {
                    let _ = log("PeacockHandler: leaving the desktop");
                    return 0;
                }
                // The appearance (docs/u1-design.md).
                Some(DecodedKey::RawKey(KeyCode::F8)) => {
                    desk.set_appearance(!desk.dark);
                    dirty = true;
                }
                // Test keys for frame timing (docs/d2-design.md §3-4).
                Some(DecodedKey::RawKey(KeyCode::F9)) => {
                    desk.always_full = !desk.always_full;
                    zoo_rt::log!(
                        "PeacockHandler: full repaint every frame {}",
                        if desk.always_full { "on" } else { "off" }
                    );
                }
                Some(DecodedKey::RawKey(KeyCode::F10)) => {
                    desk.full = true;
                    // From scratch: every window drawn again, not just
                    // composited, so the check compares against it.
                    for l in &mut desk.layers {
                        l.dirty = true;
                    }
                    dirty = true;
                    let _ = log("PeacockHandler: full repaint");
                }
                Some(DecodedKey::RawKey(KeyCode::F11)) => desk.report_frames(),
                Some(DecodedKey::RawKey(KeyCode::F12)) => {
                    let _ = log("PeacockHandler: F12 test key: faulting on purpose");
                    // SAFETY: deliberately invalid (test key, docs/m4-design.md
                    // §4): kernel-only page, so this faults and ends the
                    // window server; Zookeeper restarts it.
                    let v = unsafe { core::ptr::read_volatile(KERNEL_ADDR as *const u64) };
                    zoo_rt::log!("PeacockHandler: read returned {:#x}!", v);
                    return 30;
                }
                Some(DecodedKey::RawKey(KeyCode::ArrowUp)) => desk.send_key(Key::Up),
                Some(DecodedKey::RawKey(KeyCode::ArrowDown)) => desk.send_key(Key::Down),
                Some(DecodedKey::RawKey(KeyCode::ArrowLeft)) => desk.send_key(Key::Left),
                Some(DecodedKey::RawKey(KeyCode::ArrowRight)) => desk.send_key(Key::Right),
                Some(DecodedKey::Unicode('\n')) => desk.send_key(Key::Enter),
                Some(DecodedKey::Unicode('\u{8}')) => desk.send_key(Key::Back),
                Some(DecodedKey::Unicode(c)) if (' '..='~').contains(&c) => {
                    desk.send_key(Key::Char(c))
                }
                _ => {}
            }
        }

        // Mouse.
        // Every queued event, not just one buffer's worth: the frame shows
        // where the pointer is now (docs/u2-compositor-design.md §5).
        let mut events = [MouseEvent::default(); 16];
        loop {
            let n = zoo_rt::mouse_read(&mut events).unwrap_or(0);
            for &e in &events[..n] {
                dirty |= desk.mouse(e);
            }
            if n < events.len() {
                break;
            }
        }
        if desk.cursor_moved {
            desk.cursor_moved = false;
            let s = desk.screen.s;
            let _ = zoo_rt::cursor_move(desk.px * s, desk.py * s);
        }

        // Applications.
        for i in 0..desk.apps.len() {
            while desk.apps[i].running {
                match try_recv(desk.apps[i].chan, &mut msg) {
                    Ok(n) => {
                        if desk.apps[i].apply(&msg[..n]) {
                            if core::mem::take(&mut desk.apps[i].title_changed) {
                                desk.damage_window(i);
                                // The menu bar names the focused app.
                                if i == desk.focus {
                                    desk.damage_menu_bar();
                                }
                            } else {
                                desk.damage_content(i);
                            }
                            dirty = true;
                        }
                    }
                    Err(Error::Again) => break,
                    Err(_) => {
                        desk.app_ended(i);
                        dirty = true;
                    }
                }
            }
        }

        // Clock, and banners that have had their time.
        let second = zoo_rt::ticks() / 100;
        if second != last_second {
            last_second = second;
            desk.damage_menu_bar();
            if desk
                .banner
                .as_ref()
                .is_some_and(|b| zoo_rt::ticks() >= b.until)
            {
                desk.damage_banner();
                desk.banner = None;
            }
            dirty = true;
        }

        // Damage still waiting from a paced frame counts too.
        let pending = dirty || desk.damaged > 0 || desk.full || desk.animating();
        if pending && zoo_rt::ticks() == desk.last_frame_tick {
            // A frame already went out this tick: during continuous motion
            // at most one frame a tick (100 a second), everything that
            // arrives meanwhile merged into it. Each frame costs the
            // display (QEMU's window) more than it costs this server
            // (docs/u2-compositor-design.md §5).
            zoo_rt::sleep(1);
        } else if pending {
            desk.render();
            desk.last_frame_tick = zoo_rt::ticks();
            if core::mem::take(&mut desk.cursor_switch) {
                desk.apply_cursor();
            }
        } else {
            // Until input, a message from an application, or the next
            // second (the clock); docs/u1b-design.md.
            let now = zoo_rt::ticks();
            let _ = zoo_rt::input_wait((now / 100 + 1) * 100 - now);
        }
    }
}
