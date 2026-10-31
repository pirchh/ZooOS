//! PeacockHandler: the window server (docs/m4-design.md §4). Owns the
//! display and keyboard, runs three applications (Files, Crash Test and the
//! Assistant, docs/m5c2-design.md) over channels, draws a menu bar, the
//! windows and a status line, routes keys to the focused window, and
//! restarts applications that crash. The mouse (docs/d1-design.md) moves a
//! pointer; a click focuses a window and a title-bar drag moves it.
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
use zoo_abi::{FORMAT_BGR, MAX_MESSAGE, MouseEvent};
use zoo_rt::app::{Key, MAX_LINES};
use zoo_rt::{
    Error, Line, channel_create, close, dir_open, dir_rights, display_acquire, input_read, log,
    spawn, spawn_with, try_recv, wait,
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
}

impl Screen {
    fn fill(&mut self, x: usize, y: usize, w: usize, h: usize, c: u32) {
        let x1 = (x + w).min(self.w);
        let y1 = (y + h).min(self.h);
        for row in y.min(y1)..y1 {
            self.back[row * self.w + x.min(x1)..row * self.w + x1].fill(c);
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
            let raster = get_raster(c, WEIGHT, SIZE)
                .or_else(|| get_raster('?', WEIGHT, SIZE))
                .expect("'?' is in the font");
            for (dy, row) in raster.raster().iter().enumerate() {
                let py = y + dy;
                if py >= self.h {
                    break;
                }
                for (dx, &i) in row.iter().enumerate() {
                    self.back[py * self.w + cx + dx] = blend(bg, fg, i);
                }
            }
            cx += CHAR_W;
        }
    }

    /// Copies the whole back buffer to the framebuffer, so the screen only
    /// ever shows complete frames.
    fn present(&mut self) {
        for y in 0..self.h {
            let src = &self.back[y * self.w..(y + 1) * self.w];
            // SAFETY: the framebuffer is mapped at `fb` for `stride * h`
            // pixels of 4 bytes (checked at start-up); row `y` is in range.
            let dst = unsafe { self.fb.add(y * self.stride) };
            if self.bgr {
                // 0xRRGGBB in little-endian memory is B, G, R, X: BGR.
                // SAFETY: as above; source and destination do not overlap.
                unsafe { core::ptr::copy_nonoverlapping(src.as_ptr(), dst, self.w) };
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
    apps: [App; 3],
    /// Home directory handle; copies go to the Assistant.
    home: u64,
    focus: usize,
    status: Line,
    /// Window rectangles (x, y, w, h), indexed like `apps`.
    wins: [(usize, usize, usize, usize); 3],
    /// Drawing order, back to front; the focused window is last.
    order: [usize; 3],
    /// Pointer position.
    px: usize,
    py: usize,
    /// Left button held on the previous event.
    held: bool,
    /// Window being dragged, and the grab offset inside it.
    drag: Option<(usize, usize, usize)>,
}

impl Desktop {
    /// The starting layout: Files on the left; Crash Test and the Assistant
    /// stacked on the right.
    fn layout(w: usize, h: usize) -> [(usize, usize, usize, usize); 3] {
        let win_w = (w - 3 * MARGIN) / 2;
        let top_y = BAR_H + MARGIN;
        let full_h = h - top_y - BAR_H - MARGIN;
        let half_h = (full_h - MARGIN) / 2;
        [
            (MARGIN, top_y, win_w, full_h),
            (2 * MARGIN + win_w, top_y, win_w, half_h),
            (2 * MARGIN + win_w, top_y + half_h + MARGIN, win_w, half_h),
        ]
    }

    fn set_focus(&mut self, i: usize) {
        self.focus = i;
        let mut order = [0; 3];
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
            x >= wx && x < wx + ww && y >= wy && y < wy + wh
        })
    }

    /// Applies one mouse event; true if the screen changed.
    fn mouse(&mut self, e: MouseEvent) -> bool {
        let (w, h) = (self.screen.w, self.screen.h);
        // The mouse reports dy positive upwards; screen y grows downwards.
        self.px = (self.px as isize + e.dx as isize).clamp(0, w as isize - 1) as usize;
        self.py = (self.py as isize - e.dy as isize).clamp(0, h as isize - 1) as usize;
        let down = e.buttons & 1 != 0;
        if down && !self.held {
            if let Some(i) = self.window_at(self.px, self.py) {
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
                let (_, _, ww, wh) = self.wins[i];
                // Keep the border on screen and part of the title bar
                // reachable.
                let x = (self.px as isize - ox as isize).clamp(1, w as isize - 48) as usize;
                let y = (self.py as isize - oy as isize)
                    .clamp(BAR_H as isize + 1, (h - BAR_H - TITLE_H) as isize)
                    as usize;
                self.wins[i] = (x, y, ww, wh);
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
        true
    }

    /// A white arrow with a black edge, its tip at the pointer.
    fn draw_pointer(&mut self) {
        let (px, py) = (self.px, self.py);
        let s = &mut self.screen;
        for r in 0..17usize {
            // Rows 0-11: the head, widening by one pixel a row; rows 12-16:
            // the stem.
            let (from, to) = if r < 12 { (0, r) } else { (3, 6) };
            for c in from..=to {
                let (x, y) = (px + c, py + r);
                if x >= s.w || y >= s.h {
                    continue;
                }
                let edge = c == from || c == to || r == 11 || r == 16;
                s.back[y * s.w + x] = if edge { POINTER_EDGE } else { POINTER };
            }
        }
    }

    fn render(&mut self) {
        let (w, h) = (self.screen.w, self.screen.h);
        let s = &mut self.screen;
        s.fill(0, 0, w, h, DESKTOP);

        // Menu bar: product name, focused window, uptime.
        s.fill(0, 0, w, BAR_H, BAR);
        s.text(8, 4, "ZooOS", BAR_TEXT, BAR, 80);
        let focused = self.apps[self.focus].title.as_str();
        s.text(8 + 8 * CHAR_W, 4, focused, BAR_TEXT, BAR, w / 2);
        let mut up = Line::new();
        let _ = write!(up, "up {}s", zoo_rt::ticks() / 100);
        s.text(
            w.saturating_sub(16 * CHAR_W),
            4,
            up.as_str(),
            BAR_TEXT,
            BAR,
            16 * CHAR_W,
        );

        // Windows, back to front.
        for i in self.order {
            let app = &self.apps[i];
            let (x, win_y, win_w, win_h) = self.wins[i];
            let (tbg, tfg) = if i == self.focus {
                (TITLE_FOCUSED, TITLE_FOCUSED_TEXT)
            } else {
                (TITLE_UNFOCUSED, TITLE_UNFOCUSED_TEXT)
            };
            s.fill(x - 1, win_y - 1, win_w + 2, win_h + 2, BORDER);
            s.fill(x, win_y, win_w, TITLE_H, tbg);
            s.text(x + 8, win_y + 4, app.title.as_str(), tfg, tbg, win_w - 16);
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
        self.draw_pointer();
        self.screen.present();
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
    let (w, h) = (info.width as usize, info.height as usize);
    if info.bytes_per_pixel != 4 || w > MAX_W || h > MAX_H || w < 640 || h < 400 {
        zoo_rt::log!(
            "PeacockHandler: unsupported display {}x{}x{}",
            w,
            h,
            info.bytes_per_pixel
        );
        return 3;
    }
    zoo_rt::log!("PeacockHandler: display {}x{} acquired", w, h);
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
        },
        apps: [
            App::new("Files", "files", false),
            App::new("Crash Test", "crash-test", false),
            App::new("Assistant", "assistant", true),
        ],
        home,
        focus: 0,
        status: Line::new(),
        wins: Desktop::layout(w, h),
        order: [1, 2, 0],
        px: w / 2,
        py: h / 2,
        held: false,
        drag: None,
    };
    let _ = write!(
        desk.status,
        "Tab: next window   Arrows/Enter/Backspace: use window   Esc: leave the desktop"
    );
    for app in desk.apps.iter_mut() {
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
                    desk.set_focus((desk.focus + 1) % desk.apps.len());
                    dirty = true;
                }
                Some(DecodedKey::Unicode('\u{1b}')) => {
                    let _ = log("PeacockHandler: leaving the desktop");
                    return 0;
                }
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
                    Ok(n) => dirty |= desk.apps[i].apply(&msg[..n]),
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
            dirty = true;
        }

        if dirty {
            desk.render();
        } else {
            zoo_rt::sleep(1);
        }
    }
}
