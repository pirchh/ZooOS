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
const POINTER: u32 = 0x000000;
const POINTER_EDGE: u32 = 0xFFFFFF;

/// Largest supported screen; the back buffer lives in `.bss`.
const MAX_W: usize = 1920;
const MAX_H: usize = 1200;
static mut BACK: [u32; MAX_W * MAX_H] = [0; MAX_W * MAX_H];

/// Times one application may be restarted after crashing.
const APP_RESTARTS: u32 = 3;
/// Damage rectangles kept per frame before falling back to a full repaint.
const MAX_DAMAGE: usize = 12;
/// Pointer arrow size.
const POINTER_W: usize = 12;
const POINTER_H: usize = 17;
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
            self.title.set(t);
        } else if let Some(n) = msg.strip_prefix(b"lines:") {
            if let Some(n) = parse_num(n) {
                self.count = n.min(MAX_LINES);
            }
        } else if let Some(rest) = msg.strip_prefix(b"line:") {
            if let Some(colon) = rest.iter().position(|&b| b == b':')
                && let Some(i) = parse_num(&rest[..colon])
                && i < MAX_LINES
            {
                self.lines[i].set(&rest[colon + 1..]);
            }
        } else if let Some(s) = msg.strip_prefix(b"sel:") {
            self.selected = parse_num(s);
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

/// Smooth falloff over `2 * SHADOW` steps: 255 inside, 0 outside.
fn shadow_ramp() -> [u8; 2 * SHADOW] {
    let mut r = [0u8; 2 * SHADOW];
    for (k, v) in r.iter_mut().enumerate() {
        let t = (k as f32 + 0.5) / (2 * SHADOW) as f32;
        let s = 1.0 - t * t * (3.0 - 2.0 * t);
        *v = (s * 255.0 + 0.5) as u8;
    }
    r
}

struct Screen {
    w: usize,
    h: usize,
    stride: usize,
    fb: *mut u32,
    bgr: bool,
    /// Changes show only after `display_flush` (a virtio GPU).
    flush: bool,
    back: &'static mut [u32],
    /// Drawing is limited to this rectangle (x0, y0, x1, y1), exclusive.
    clip: (usize, usize, usize, usize),
}

impl Screen {
    /// True if rectangle (x, y, w, h) reaches into the clip rectangle:
    /// shapes that do not are skipped (docs/u1-design.md §4).
    fn visible(&self, x: usize, y: usize, w: usize, h: usize) -> bool {
        let (cx0, cy0, cx1, cy1) = self.clip;
        x < cx1 && y < cy1 && x + w > cx0 && y + h > cy0
    }

    fn fill(&mut self, x: usize, y: usize, w: usize, h: usize, c: u32) {
        let (cx0, cy0, cx1, cy1) = self.clip;
        let (x0, y0, x1, y1) = (x.max(cx0), y.max(cy0), (x + w).min(cx1), (y + h).min(cy1));
        if x0 >= x1 || y0 >= y1 {
            return;
        }
        for row in y0..y1 {
            self.back[row * self.w + x0..row * self.w + x1].fill(c);
        }
    }

    /// Blends `c` over the pixel at (x, y) by `a` (0..=255).
    fn blend_px(&mut self, x: usize, y: usize, c: u32, a: u8) {
        let (cx0, cy0, cx1, cy1) = self.clip;
        if a != 0 && x >= cx0 && x < cx1 && y >= cy0 && y < cy1 {
            let p = &mut self.back[y * self.w + x];
            *p = if a == 255 { c } else { blend(*p, c, a) };
        }
    }

    /// Blends `c` over a rectangle by `a`.
    fn tint(&mut self, x: usize, y: usize, w: usize, h: usize, c: u32, a: u8) {
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
        if !self.visible(x, y, w, h) {
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
                    self.fill(x, y + row, w, 1, c);
                } else {
                    self.tint(x, y + row, w, 1, c, a);
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
                self.blend_px(x + col, y + row, c, cov);
            }
        }
    }

    /// A one-pixel anti-aliased outline of a rounded rectangle.
    fn round_outline(&mut self, x: usize, y: usize, w: usize, h: usize, r: usize, c: u32) {
        if !self.visible(x, y, w, h) {
            return;
        }
        let r = r.min(w / 2).min(h / 2);
        // Straight edges.
        self.fill(x + r, y, w - 2 * r, 1, c);
        self.fill(x + r, y + h - 1, w - 2 * r, 1, c);
        self.fill(x, y + r, 1, h - 2 * r, c);
        self.fill(x + w - 1, y + r, 1, h - 2 * r, c);
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
                    self.blend_px(px, py, c, cov);
                }
            }
        }
    }

    /// A filled anti-aliased disc of diameter `d` with its top-left at
    /// (x, y), and a rim colour on its outermost pixel ring.
    fn disc(&mut self, x: usize, y: usize, d: usize, fill: u32, rim: u32) {
        if !self.visible(x, y, d, d) {
            return;
        }
        let r = d / 2;
        for dy in 0..d {
            for dx in 0..d {
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
                self.blend_px(x + dx, y + dy, rim, outer);
                self.blend_px(x + dx, y + dy, fill, inner);
            }
        }
    }

    /// A soft shadow of black under the rectangle, `a` at its strongest,
    /// drawn only outside the rectangle itself (which covers the rest).
    fn shadow(&mut self, rect: (usize, usize, usize, usize), a: u8, ramp: &[u8; 2 * SHADOW]) {
        let (wx, wy, ww, wh) = rect;
        // The shadow's rectangle: the window's, moved down.
        let (sx0, sy0) = (wx as isize, (wy + SHADOW_DROP) as isize);
        let (sx1, sy1) = (sx0 + ww as isize, sy0 + wh as isize);
        let s = SHADOW as isize;
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

    /// Draws `text` in `face` with its baseline at `y`, starting at `x`,
    /// clipped at `max_x`; returns where it ended.
    fn text(&mut self, face: &Face, x: usize, y: usize, text: &str, c: u32, max_x: usize) -> usize {
        if !self.visible(x, y.saturating_sub(20), max_x.saturating_sub(x), 28) {
            return x + face.width(text);
        }
        let end = face.draw(text, x as i32, y as i32, max_x as i32, |px, py, cov| {
            if px >= 0 && py >= 0 {
                self.blend_px(px as usize, py as usize, c, cov);
            }
        });
        end.max(0) as usize
    }

    /// Copies rectangle (x0, y0, x1, y1) of the back buffer to the
    /// framebuffer. Called after every damaged rectangle is drawn, so the
    /// screen only shows complete frames.
    fn present(&mut self, (x0, y0, x1, y1): (usize, usize, usize, usize)) {
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
    /// The pointer is a hardware cursor: moving it needs no redraw
    /// (docs/gpu1-design.md).
    hw_cursor: bool,
    /// The hardware cursor has moved since it was last told.
    cursor_moved: bool,
    atlas: Atlas<'static>,
    dark: bool,
    /// Wallpaper colour per row, for the current appearance.
    wall: [u32; MAX_H],
    ramp: [u8; 2 * SHADOW],
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
    frame_count: usize,
    /// The dock icon under the pointer (its name shows above it).
    hover: Option<usize>,
    banner: Option<Banner>,
}

impl Desktop {
    fn face(&self, id: FaceId) -> Face<'static> {
        self.atlas.face(id).expect("checked at start-up")
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
        zoo_rt::log!("PeacockHandler: appearance {}", t.name);
    }

    /// Marks rectangle (x, y, w, h) for redrawing.
    fn damage(&mut self, x: usize, y: usize, w: usize, h: usize) {
        let r = (
            x.min(self.screen.w),
            y.min(self.screen.h),
            (x + w).min(self.screen.w),
            (y + h).min(self.screen.h),
        );
        if r.0 >= r.2 || r.1 >= r.3 || self.full {
            return;
        }
        if self.damaged == MAX_DAMAGE {
            self.full = true;
            return;
        }
        self.damage[self.damaged] = r;
        self.damaged += 1;
    }

    /// A window, with its shadow.
    fn damage_window(&mut self, i: usize) {
        let (x, y, w, h) = self.wins[i];
        self.damage(
            x.saturating_sub(MARGIN),
            y.saturating_sub(MARGIN),
            w + 2 * MARGIN,
            h + 2 * MARGIN,
        );
    }

    fn damage_pointer(&mut self) {
        if self.hw_cursor {
            self.cursor_moved = true;
        } else {
            self.damage(self.px, self.py, POINTER_W, POINTER_H);
        }
    }

    fn damage_menu_bar(&mut self) {
        self.damage(0, 0, self.screen.w, MENU_H);
    }

    fn dock_rect(&self) -> (usize, usize, usize, usize) {
        zoo_abi::ui::dock_rect(self.screen.w, self.screen.h, APPS)
    }

    fn damage_dock(&mut self) {
        let (x, y, w, h) = self.dock_rect();
        // With the label above an icon.
        self.damage(x, y.saturating_sub(40), w, h + 40);
    }

    fn banner_rect(&self) -> (usize, usize, usize, usize) {
        (
            self.screen.w - 16 - BANNER_W,
            MENU_H + 10,
            BANNER_W,
            BANNER_H,
        )
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
                self.wins[i] = zoo_abi::ui::zoomed(self.screen.w, self.screen.h);
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
        let (w, h) = (self.screen.w, self.screen.h);
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
        } else if self.apps[i].minimized {
            self.apps[i].minimized = false;
            zoo_rt::log!("PeacockHandler: showed {}", self.apps[i].label);
            self.damage_window(i);
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
        let (w, h) = (self.screen.w, self.screen.h);
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
                self.damage_window(i);
                let (_, _, ww, wh) = self.wins[i];
                // Keep part of the title bar reachable, below the menu bar.
                let x =
                    (self.px as isize - ox as isize).clamp(-(ww as isize) + 80, w as isize - 80);
                let y =
                    (self.py as isize - oy as isize).clamp(MENU_H as isize, (h - TITLE_H) as isize);
                self.wins[i] = (x.max(0) as usize, y as usize, ww, wh);
                self.damage_window(i);
            }
        } else if let Some((i, _, _)) = self.resize.take() {
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
            let cw = self.face(FaceId::Regular13).width(clock.as_str());
            let (sx, sy) = zoo_abi::ui::appearance_switch(self.screen.w, cw);
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
        for &r in &rects[..n] {
            self.screen.clip = r;
            self.draw();
        }
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
        c[..n].sort_unstable();
        p[..n].sort_unstable();
        zoo_rt::log!(
            "PeacockHandler: frames {} mode {} median {} cycles {} px",
            self.frame_count,
            if self.always_full { "full" } else { "damage" },
            if n > 0 { c[n / 2] } else { 0 },
            if n > 0 { p[n / 2] } else { 0 }
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
            if self.apps[i].visible() {
                self.draw_window(i);
            }
        }
        self.draw_menu_bar();
        self.draw_banner();
        self.draw_dock();
        if !self.hw_cursor {
            self.draw_pointer();
        }
    }

    fn draw_menu_bar(&mut self) {
        let t = self.theme();
        let w = self.screen.w;
        if self.screen.clip.1 >= MENU_H {
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
        let cw = regular.width(clock.as_str());
        self.screen
            .text(&regular, w - 16 - cw, base, clock.as_str(), t.bar_text, w);
        // The appearance switch: a disc, half filled.
        let (sx, sy) = zoo_abi::ui::appearance_switch(w, cw);
        let (bx, by) = (sx - 7, sy - 7);
        self.screen.disc(bx, by, 14, t.bar_text, t.bar_text);
        self.screen
            .disc(bx + 2, by + 2, 10, zoo_abi::ui::bar_color(t), t.bar_text);
        // The right half filled: dark shown as filled.
        for dy in 0..10 {
            for dx in 5..10 {
                let (qx, qy) = (dx.min(9 - dx), dy.min(9 - dy));
                let c = corner_coverage(5, qx, qy);
                self.screen
                    .blend_px(bx + 2 + dx, by + 2 + dy, t.bar_text, c);
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
        if x + w + MARGIN <= cx0 || x >= cx1 + MARGIN || y + h + MARGIN <= cy0 || y >= cy1 + MARGIN
        {
            return;
        }
        let ramp = self.ramp;
        self.screen.shadow(
            (x, y, w, h),
            if focused { t.shadow } else { t.shadow_blurred },
            &ramp,
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
        let tw = medium.width(app.title.as_str());
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
        let ramp = self.ramp;
        self.screen.shadow((x, y, w, h), t.shadow_blurred, &ramp);
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
        let (sw, sh) = (self.screen.w, self.screen.h);
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
            let lw = small.width(label) + 20;
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

    /// A black arrow with a white edge, its tip at the pointer.
    /// The pointer arrow as a hardware-cursor image (0xAARRGGBB), the
    /// same shape `draw_pointer` draws.
    fn cursor_image() -> [u32; zoo_abi::CURSOR_SIZE * zoo_abi::CURSOR_SIZE] {
        let mut img = [0u32; zoo_abi::CURSOR_SIZE * zoo_abi::CURSOR_SIZE];
        for r in 0..POINTER_H {
            let (from, to) = if r < 12 { (0, r) } else { (3, 6) };
            for c in from..=to {
                let edge = c == from || c == to || r == 11 || r == POINTER_H - 1;
                img[r * zoo_abi::CURSOR_SIZE + c] =
                    0xFF00_0000 | if edge { POINTER_EDGE } else { POINTER };
            }
        }
        img
    }

    fn draw_pointer(&mut self) {
        let (px, py) = (self.px, self.py);
        let s = &mut self.screen;
        for r in 0..POINTER_H {
            // Rows 0-11: the head, widening by one pixel a row; rows 12-16:
            // the stem.
            let (from, to) = if r < 12 { (0, r) } else { (3, 6) };
            for c in from..=to {
                let edge = c == from || c == to || r == 11 || r == POINTER_H - 1;
                s.blend_px(
                    px + c,
                    py + r,
                    if edge { POINTER_EDGE } else { POINTER },
                    255,
                );
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
    // A screen larger than the back buffer: the desktop uses its top-left
    // part and the rest is painted once (docs/usb-boot.md).
    let (w, h) = (full_w.min(MAX_W), full_h.min(MAX_H));
    if (w, h) != (full_w, full_h) {
        zoo_rt::log!(
            "PeacockHandler: display {}x{}: using {}x{}",
            full_w,
            full_h,
            w,
            h
        );
        let fb = info.base as *mut u32;
        for y in 0..full_h {
            let from = if y < h { w } else { 0 };
            for x in from..full_w {
                // SAFETY: (x, y) is inside the mapped framebuffer
                // (`stride * height` pixels of 4 bytes).
                unsafe { fb.add(y * info.stride as usize + x).write_volatile(0) };
            }
        }
    } else {
        zoo_rt::log!("PeacockHandler: display {}x{} acquired", w, h);
    }
    // SAFETY: this process is single-threaded and this is the only place a
    // reference to the back buffer is created.
    let back = unsafe { &mut *core::ptr::addr_of_mut!(BACK) };
    let mut desk = Desktop {
        hw_cursor: false,
        cursor_moved: false,
        screen: Screen {
            w,
            h,
            stride: info.stride as usize,
            fb: info.base as *mut u32,
            bgr: info.format == FORMAT_BGR,
            flush: info.flags & zoo_abi::DISPLAY_FLUSH_NEEDED != 0,
            back: &mut back[..w * h],
            clip: (0, 0, w, h),
        },
        atlas,
        dark: false,
        wall: [0; MAX_H],
        ramp: shadow_ramp(),
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
        frame_count: 0,
        hover: None,
        banner: None,
    };
    desk.set_appearance(false);
    if info.flags & zoo_abi::DISPLAY_HW_CURSOR != 0 {
        let image = Desktop::cursor_image();
        if zoo_rt::cursor_set(&image, (0, 0), (desk.px, desk.py)).is_ok() {
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
        let mut events = [MouseEvent::default(); 16];
        let n = zoo_rt::mouse_read(&mut events).unwrap_or(0);
        for &e in &events[..n] {
            dirty |= desk.mouse(e);
        }
        if desk.cursor_moved {
            desk.cursor_moved = false;
            let _ = zoo_rt::cursor_move(desk.px, desk.py);
        }

        // Applications.
        for i in 0..desk.apps.len() {
            while desk.apps[i].running {
                match try_recv(desk.apps[i].chan, &mut msg) {
                    Ok(n) => {
                        if desk.apps[i].apply(&msg[..n]) {
                            desk.damage_window(i);
                            if i == desk.focus {
                                desk.damage_menu_bar();
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

        if dirty {
            desk.render();
        } else {
            // Until input, a message from an application, or the next
            // second (the clock); docs/u1b-design.md.
            let now = zoo_rt::ticks();
            let _ = zoo_rt::input_wait((now / 100 + 1) * 100 - now);
        }
    }
}
