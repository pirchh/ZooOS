//! PeacockHandler: the window server (docs/m4-design.md §4). Owns the
//! display and keyboard, runs three applications (Files, Crash Test and the
//! Assistant, docs/m5c2-design.md) over channels, draws a menu bar, the
//! windows and a status line, routes keys to the focused window, and
//! restarts applications that crash. The mouse (docs/d1-design.md) moves a
//! pointer; a click focuses a window and a title-bar drag moves it. Only
//! the parts of the screen that changed are redrawn (docs/d2-design.md).
//! Windows have close buttons and an auto-hiding dock reopens closed
//! applications (docs/d3-design.md).
//!
//! Its second start-up handle is the home directory (from Zookeeper); each
//! start of the Assistant gets a fresh copy, which it passes on to the file
//! broker.

#![no_std]
#![no_main]

use core::fmt::Write as _;
use noto_sans_mono_bitmap::{FontWeight, RasterHeight, get_raster, get_raster_width};
use pc_keyboard::layouts::Us104Key;
use pc_keyboard::{DecodedKey, HandleControl, KeyCode, PS2Keyboard, ScancodeSet1};
use zoo_abi::{CLOCK_REALTIME, FORMAT_BGR, MAX_MESSAGE, MouseEvent};
use zoo_rt::app::{Key, MAX_LINES};
use zoo_rt::{
    Error, Line, channel_create, close, dir_open, dir_rights, display_acquire, input_read, kill,
    log, spawn, spawn_with, try_recv, wait, wait_any,
};

zoo_rt::entry2!(main);

// ---- colours (docs/m4-design.md §7), as 0xRRGGBB ----------------------
const DESKTOP: u32 = 0x1E3A4C;
const BAR: u32 = 0xE8E8E8;
const BAR_TEXT: u32 = 0x202020;
const TITLE_FOCUSED: u32 = 0x3D7EBF;
const TITLE_FOCUSED_TEXT: u32 = 0xFFFFFF;
const TITLE_UNFOCUSED: u32 = 0x9AA5AE;
const TITLE_UNFOCUSED_TEXT: u32 = 0x202020;
const CONTENT: u32 = 0xFFFFFF;
const CONTENT_TEXT: u32 = 0x202020;
const SELECTED: u32 = 0xCFE3F7;
const BORDER: u32 = 0x5A6670;
const CLOSE_BUTTON: u32 = 0xE0443E;
const DOCK_BG: u32 = 0xDDE3E8;
const DOCK_TEXT: u32 = 0x202020;
const TILE_TEXT: u32 = 0xFFFFFF;
/// Dock tile colours, indexed like the applications.
const TILE: [u32; APPS] = [0x3D7EBF, 0xD9822B, 0x3E9E6B, 0xC9A227];
const POINTER: u32 = 0xFFFFFF;
const POINTER_EDGE: u32 = 0x000000;

const WEIGHT: FontWeight = FontWeight::Regular;
const SIZE: RasterHeight = RasterHeight::Size16;
const CHAR_W: usize = get_raster_width(WEIGHT, SIZE);
const BAR_H: usize = 24;
const TITLE_H: usize = 24;
const LINE_H: usize = 20;
const MARGIN: usize = 24;
const TEXT_COLS: usize = 80;

/// Largest supported screen; the back buffer lives in `.bss`.
const MAX_W: usize = 1920;
const MAX_H: usize = 1200;
static mut BACK: [u32; MAX_W * MAX_H] = [0; MAX_W * MAX_H];

/// Times one application may be restarted after crashing.
const APP_RESTARTS: u32 = 3;

/// Damage rectangles kept per frame before falling back to a full repaint.
const MAX_DAMAGE: usize = 8;
/// Pointer arrow size.
const POINTER_W: usize = 12;
const POINTER_H: usize = 17;
/// Applications, in dock order; Notes is the last.
const APPS: usize = 4;
const NOTES: usize = 3;
/// Where Notes opens: (x, y, w, h).
const NOTES_RECT: (usize, usize, usize, usize) = (200, 100, 640, 480);
/// Close button: size, and gap from the title bar's right end.
const CLOSE_SIZE: usize = 12;
const CLOSE_GAP: usize = 8;
/// Dock geometry: one slot per application, a square tile in each, the
/// name under it, then the running dot.
const DOCK_SLOT_W: usize = 96;
const DOCK_PAD: usize = 8;
const DOCK_TILE: usize = 48;
const DOCK_H: usize = 92;
/// Ticks an application gets to end after its window is closed.
const CLOSE_WAIT: u64 = 100;
/// Frames kept for the timing report (F11).
const FRAME_SAMPLES: usize = 64;

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
    /// Shown in the status line, e.g. "Crash Test".
    label: &'static str,
    program: &'static str,
    /// Gets a copy of the home directory handle as its second handle.
    wants_home: bool,
    chan: u64,
    process: u64,
    running: bool,
    /// Closed by the user: window hidden, not restarted.
    closed: bool,
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
            restarts: 0,
            title: Text::new(),
            lines: [const { Text::new() }; MAX_LINES],
            count: 0,
            selected: None,
        }
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

struct Screen {
    w: usize,
    h: usize,
    stride: usize,
    fb: *mut u32,
    bgr: bool,
    back: &'static mut [u32],
    /// Drawing is limited to this rectangle (x0, y0, x1, y1), exclusive.
    clip: (usize, usize, usize, usize),
}

impl Screen {
    fn fill(&mut self, x: usize, y: usize, w: usize, h: usize, c: u32) {
        let (cx0, cy0, cx1, cy1) = self.clip;
        let x0 = x.max(cx0);
        let y0 = y.max(cy0);
        let x1 = (x + w).min(cx1);
        let y1 = (y + h).min(cy1);
        if x0 >= x1 || y0 >= y1 {
            return;
        }
        for row in y0..y1 {
            self.back[row * self.w + x0..row * self.w + x1].fill(c);
        }
    }

    fn plot(&mut self, x: usize, y: usize, c: u32) {
        let (cx0, cy0, cx1, cy1) = self.clip;
        if x >= cx0 && x < cx1 && y >= cy0 && y < cy1 {
            self.back[y * self.w + x] = c;
        }
    }

    /// Draws `text` with its top-left at (x, y), blending each glyph pixel
    /// between `bg` and `fg` (the formula the verifier reproduces), clipped
    /// to `max_w` pixels.
    fn text(&mut self, x: usize, y: usize, text: &str, fg: u32, bg: u32, max_w: usize) {
        let mut cx = x;
        for c in text.chars() {
            if cx + CHAR_W > x + max_w || cx + CHAR_W > self.w {
                break;
            }
            let (cx0, cy0, cx1, cy1) = self.clip;
            // Glyphs wholly outside the clip are skipped.
            if cx + CHAR_W <= cx0 || cx >= cx1 || y + LINE_H <= cy0 || y >= cy1 {
                cx += CHAR_W;
                continue;
            }
            let raster = get_raster(c, WEIGHT, SIZE)
                .or_else(|| get_raster('?', WEIGHT, SIZE))
                .expect("'?' is in the font");
            for (dy, row) in raster.raster().iter().enumerate() {
                for (dx, &i) in row.iter().enumerate() {
                    self.plot(cx + dx, y + dy, blend(bg, fg, i));
                }
            }
            cx += CHAR_W;
        }
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

/// Columns kept for the menu-bar clock (`Sat 31 Oct 06:31 UTC` and a
/// margin).
const CLOCK_COLS: usize = 22;
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
/// uptime when there is no clock.
fn write_clock(out: &mut Line) {
    match zoo_rt::clock(CLOCK_REALTIME) {
        Ok(ns) => {
            let secs = (ns / 1_000_000_000) as i64;
            let days = secs.div_euclid(86_400);
            let rem = secs.rem_euclid(86_400);
            let (month, day) = civil_from_days(days);
            let _ = write!(
                out,
                "{} {} {} {:02}:{:02} UTC",
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

fn blend(bg: u32, fg: u32, i: u8) -> u32 {
    let i = i as u32;
    let ch = |shift: u32| {
        let b = (bg >> shift) & 0xFF;
        let f = (fg >> shift) & 0xFF;
        ((b * (255 - i) + f * i + 127) / 255) << shift
    };
    ch(16) | ch(8) | ch(0)
}

struct Desktop {
    screen: Screen,
    apps: [App; APPS],
    /// Home directory handle; copies go to the Assistant.
    home: u64,
    focus: usize,
    status: Line,
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
    /// Areas to redraw (x0, y0, x1, y1); `full` overrides them.
    damage: [(usize, usize, usize, usize); MAX_DAMAGE],
    damaged: usize,
    full: bool,
    /// Test mode (F9): repaint everything every frame, as before D2.
    always_full: bool,
    /// Recent frames: (cycles, pixels drawn).
    frames: [(u64, u64); FRAME_SAMPLES],
    frame_count: usize,
    /// The dock is shown (pointer near the bottom).
    dock_visible: bool,
}

impl Desktop {
    /// The starting layout: Files on the left; Crash Test and the Assistant
    /// stacked on the right.
    fn layout(w: usize, h: usize) -> [(usize, usize, usize, usize); APPS] {
        let win_w = (w - 3 * MARGIN) / 2;
        let top_y = BAR_H + MARGIN;
        let full_h = h - top_y - BAR_H - MARGIN;
        let half_h = (full_h - MARGIN) / 2;
        [
            (MARGIN, top_y, win_w, full_h),
            (2 * MARGIN + win_w, top_y, win_w, half_h),
            (2 * MARGIN + win_w, top_y + half_h + MARGIN, win_w, half_h),
            NOTES_RECT,
        ]
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

    /// A window, with its border.
    fn damage_window(&mut self, i: usize) {
        let (x, y, w, h) = self.wins[i];
        self.damage(x - 1, y - 1, w + 2, h + 2);
    }

    fn damage_pointer(&mut self) {
        self.damage(self.px, self.py, POINTER_W, POINTER_H);
    }

    fn damage_menu_bar(&mut self) {
        self.damage(0, 0, self.screen.w, BAR_H);
    }

    fn damage_status(&mut self) {
        self.damage(0, self.screen.h - BAR_H, self.screen.w, BAR_H);
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

    /// Topmost window under the pointer.
    fn window_at(&self, x: usize, y: usize) -> Option<usize> {
        self.order.iter().rev().copied().find(|&i| {
            let (wx, wy, ww, wh) = self.wins[i];
            !self.apps[i].closed && x >= wx && x < wx + ww && y >= wy && y < wy + wh
        })
    }

    /// Top-left corner of window `i`'s close button.
    fn close_button(&self, i: usize) -> (usize, usize) {
        let (x, y, w, _) = self.wins[i];
        (
            x + w - CLOSE_GAP - CLOSE_SIZE,
            y + (TITLE_H - CLOSE_SIZE) / 2,
        )
    }

    fn on_close_button(&self, i: usize) -> bool {
        let (bx, by) = self.close_button(i);
        self.px >= bx && self.px < bx + CLOSE_SIZE && self.py >= by && self.py < by + CLOSE_SIZE
    }

    /// The dock panel (x, y, w, h), centred above the status line.
    fn dock_rect(&self) -> (usize, usize, usize, usize) {
        let w = self.apps.len() * DOCK_SLOT_W + 2 * DOCK_PAD;
        let x = (self.screen.w - w) / 2;
        let y = self.screen.h - BAR_H - DOCK_PAD - DOCK_H;
        (x, y, w, DOCK_H)
    }

    fn damage_dock(&mut self) {
        let (x, y, w, h) = self.dock_rect();
        self.damage(x - 1, y - 1, w + 2, h + 2);
    }

    /// The dock slot under the pointer.
    fn dock_slot(&self) -> Option<usize> {
        let (x, y, w, h) = self.dock_rect();
        let inside = self.px >= x + DOCK_PAD
            && self.px < x + w - DOCK_PAD
            && self.py >= y
            && self.py < y + h;
        inside.then(|| (self.px - x - DOCK_PAD) / DOCK_SLOT_W)
    }

    /// A dock tile: opens a closed application, or raises a running one.
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
        }
        self.set_focus(i);
    }

    /// The close button: ends the application without restarting it and
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
        if self.focus == i
            && let Some(j) = self
                .order
                .iter()
                .rev()
                .copied()
                .find(|&j| j != i && !self.apps[j].closed)
        {
            self.set_focus(j);
        }
    }

    /// Tab: the next open window.
    fn focus_next(&mut self) {
        let n = self.apps.len();
        if let Some(j) = (1..n)
            .map(|k| (self.focus + k) % n)
            .find(|&j| !self.apps[j].closed)
        {
            self.set_focus(j);
        }
    }

    fn draw_dock(&mut self) {
        let (dx, dy, dw, dh) = self.dock_rect();
        let s = &mut self.screen;
        s.fill(dx - 1, dy - 1, dw + 2, dh + 2, BORDER);
        s.fill(dx, dy, dw, dh, DOCK_BG);
        for (i, app) in self.apps.iter().enumerate() {
            let slot = dx + DOCK_PAD + i * DOCK_SLOT_W;
            let tx = slot + (DOCK_SLOT_W - DOCK_TILE) / 2;
            let ty = dy + DOCK_PAD;
            s.fill(tx, ty, DOCK_TILE, DOCK_TILE, TILE[i]);
            let initial = &app.label[..1];
            s.text(
                tx + (DOCK_TILE - CHAR_W) / 2,
                ty + (DOCK_TILE - 16) / 2,
                initial,
                TILE_TEXT,
                TILE[i],
                CHAR_W,
            );
            let name_w = app.label.len() * CHAR_W;
            s.text(
                slot + (DOCK_SLOT_W - name_w) / 2,
                ty + DOCK_TILE + 4,
                app.label,
                DOCK_TEXT,
                DOCK_BG,
                DOCK_SLOT_W,
            );
            if app.running {
                s.fill(
                    slot + DOCK_SLOT_W / 2 - 2,
                    ty + DOCK_TILE + 24,
                    4,
                    4,
                    DOCK_TEXT,
                );
            }
        }
    }

    /// Applies one mouse event; true if the screen changed.
    fn mouse(&mut self, e: MouseEvent) -> bool {
        let (w, h) = (self.screen.w, self.screen.h);
        self.damage_pointer();
        // The mouse reports dy positive upwards; screen y grows downwards.
        self.px = (self.px as isize + e.dx as isize).clamp(0, w as isize - 1) as usize;
        self.py = (self.py as isize - e.dy as isize).clamp(0, h as isize - 1) as usize;
        let down = e.buttons & 1 != 0;
        if down && !self.held {
            if self.dock_visible
                && let Some(i) = self.dock_slot()
            {
                self.dock_click(i);
            } else if let Some(i) = self.window_at(self.px, self.py)
                && self.on_close_button(i)
            {
                self.close_app(i);
            } else if let Some(i) = self.window_at(self.px, self.py) {
                if i != self.focus {
                    zoo_rt::log!("PeacockHandler: click focuses {}", self.apps[i].label);
                }
                self.set_focus(i);
                let (wx, wy, _, _) = self.wins[i];
                if self.py < wy + TITLE_H {
                    self.drag = Some((i, self.px - wx, self.py - wy));
                }
            }
        } else if down {
            if let Some((i, ox, oy)) = self.drag {
                self.damage_window(i);
                let (_, _, ww, wh) = self.wins[i];
                // Keep the border on screen and part of the title bar
                // reachable.
                let x = (self.px as isize - ox as isize).clamp(1, w as isize - 48) as usize;
                let y = (self.py as isize - oy as isize)
                    .clamp(BAR_H as isize + 1, (h - BAR_H - TITLE_H) as isize)
                    as usize;
                self.wins[i] = (x, y, ww, wh);
                self.damage_window(i);
            }
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
        // Like an auto-hiding dock: shown while the pointer is near it.
        let show = self.py + DOCK_PAD >= self.dock_rect().1;
        if show != self.dock_visible {
            self.dock_visible = show;
            self.damage_dock();
        }
        true
    }

    /// A white arrow with a black edge, its tip at the pointer.
    fn draw_pointer(&mut self) {
        let (px, py) = (self.px, self.py);
        let s = &mut self.screen;
        for r in 0..POINTER_H {
            // Rows 0-11: the head, widening by one pixel a row; rows 12-16:
            // the stem.
            let (from, to) = if r < 12 { (0, r) } else { (3, 6) };
            for c in from..=to {
                let edge = c == from || c == to || r == 11 || r == POINTER_H - 1;
                s.plot(px + c, py + r, if edge { POINTER_EDGE } else { POINTER });
            }
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
    }

    /// Draws everything, limited to the screen's clip rectangle.
    fn draw(&mut self) {
        let (w, h) = (self.screen.w, self.screen.h);
        let s = &mut self.screen;
        s.fill(0, 0, w, h, DESKTOP);

        // Menu bar: product name, focused window, uptime.
        s.fill(0, 0, w, BAR_H, BAR);
        s.text(8, 4, "ZooOS", BAR_TEXT, BAR, 80);
        let focused = if self.apps[self.focus].closed {
            ""
        } else {
            self.apps[self.focus].title.as_str()
        };
        s.text(8 + 8 * CHAR_W, 4, focused, BAR_TEXT, BAR, w / 2);
        let mut clock = Line::new();
        write_clock(&mut clock);
        s.text(
            w.saturating_sub(CLOCK_COLS * CHAR_W),
            4,
            clock.as_str(),
            BAR_TEXT,
            BAR,
            CLOCK_COLS * CHAR_W,
        );

        // Windows, back to front.
        for i in self.order {
            let app = &self.apps[i];
            if app.closed {
                continue;
            }
            let (x, win_y, win_w, win_h) = self.wins[i];
            let (tbg, tfg) = if i == self.focus {
                (TITLE_FOCUSED, TITLE_FOCUSED_TEXT)
            } else {
                (TITLE_UNFOCUSED, TITLE_UNFOCUSED_TEXT)
            };
            s.fill(x - 1, win_y - 1, win_w + 2, win_h + 2, BORDER);
            s.fill(x, win_y, win_w, TITLE_H, tbg);
            // Title text stops short of the close button.
            s.text(
                x + 8,
                win_y + 4,
                app.title.as_str(),
                tfg,
                tbg,
                win_w - 16 - CLOSE_SIZE - CLOSE_GAP,
            );
            let (bx, by) = (
                x + win_w - CLOSE_GAP - CLOSE_SIZE,
                win_y + (TITLE_H - CLOSE_SIZE) / 2,
            );
            for cy in 0..CLOSE_SIZE {
                for cx in 0..CLOSE_SIZE {
                    // A disc: (2c - 11)^2 summed within 12^2.
                    let (ex, ey) = (2 * cx as isize - 11, 2 * cy as isize - 11);
                    if ex * ex + ey * ey <= 144 {
                        s.plot(bx + cx, by + cy, CLOSE_BUTTON);
                    }
                }
            }
            s.fill(x, win_y + TITLE_H, win_w, win_h - TITLE_H, CONTENT);
            let top = win_y + TITLE_H + 6;
            if !app.running {
                s.text(
                    x + 8,
                    top + 2,
                    "(not running)",
                    CONTENT_TEXT,
                    CONTENT,
                    win_w - 16,
                );
                continue;
            }
            for (li, line) in app.lines[..app.count].iter().enumerate() {
                let ly = top + li * LINE_H;
                if ly + LINE_H > win_y + win_h {
                    break;
                }
                let bg = if app.selected == Some(li) {
                    SELECTED
                } else {
                    CONTENT
                };
                if bg != CONTENT {
                    s.fill(x + 2, ly, win_w - 4, LINE_H, bg);
                }
                s.text(x + 8, ly + 2, line.as_str(), CONTENT_TEXT, bg, win_w - 16);
            }
        }

        // Status line.
        s.fill(0, h - BAR_H, w, BAR_H, BAR);
        s.text(
            8,
            h - BAR_H + 4,
            self.status.as_str(),
            BAR_TEXT,
            BAR,
            w - 16,
        );
        if self.dock_visible {
            self.draw_dock();
        }
        self.draw_pointer();
    }

    fn send_key(&self, key: Key) {
        let app = &self.apps[self.focus];
        if app.running {
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
        self.damage_status();
        if i == self.focus {
            self.damage_menu_bar();
        }
        let app = &mut self.apps[i];
        app.running = false;
        let _ = close(app.chan);
        let outcome = wait(app.process);
        let _ = close(app.process);
        let mut status = Line::new();
        if app.restarts < APP_RESTARTS {
            app.restarts += 1;
            let _ = write!(
                status,
                "{} {}; restarted ({} of {})",
                app.label,
                if outcome == Err(Error::Crashed) {
                    "crashed"
                } else {
                    "ended"
                },
                app.restarts,
                APP_RESTARTS
            );
            app.start(self.home);
        } else {
            let _ = write!(status, "{} ended; restart limit reached", app.label);
        }
        zoo_rt::log!("PeacockHandler: {}", status.as_str());
        self.status = status;
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
        screen: Screen {
            w,
            h,
            stride: info.stride as usize,
            fb: info.base as *mut u32,
            bgr: info.format == FORMAT_BGR,
            back: &mut back[..w * h],
            clip: (0, 0, w, h),
        },
        apps: [
            App::new("Files", "files", true),
            App::new("Crash Test", "crash-test", false),
            App::new("Assistant", "assistant", true),
            App::new("Notes", "notes", true),
        ],
        home,
        focus: 0,
        status: Line::new(),
        wins: Desktop::layout(w, h),
        order: [3, 1, 2, 0],
        px: w / 2,
        py: h / 2,
        held: false,
        drag: None,
        damage: [(0, 0, 0, 0); MAX_DAMAGE],
        damaged: 0,
        full: true,
        always_full: false,
        frames: [(0, 0); FRAME_SAMPLES],
        frame_count: 0,
        dock_visible: false,
    };
    let _ = write!(
        desk.status,
        "Tab: next window   Arrows/Enter/Backspace: use window   Esc: leave the desktop"
    );
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

        // Clock.
        let second = zoo_rt::ticks() / 100;
        if second != last_second {
            last_second = second;
            desk.damage_menu_bar();
            dirty = true;
        }

        if dirty {
            desk.render();
        } else {
            zoo_rt::sleep(1);
        }
    }
}
