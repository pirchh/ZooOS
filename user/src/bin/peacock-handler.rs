//! PeacockHandler: the window server (docs/m4-design.md §4; look:
//! docs/u1-design.md). Owns the display and keyboard, runs four
//! applications (Files, Crash Test, Bella and Notes) over channels,
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
//! start of Bella gets a fresh copy, which it passes on to the file
//! broker.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;

use core::fmt::Write as _;
use pc_keyboard::layouts::Us104Key;
use pc_keyboard::{DecodedKey, HandleControl, KeyCode, KeyState, PS2Keyboard, ScancodeSet1};
use zoo_abi::font::{Atlas, Face, FaceId, blend};
use zoo_abi::ui::{
    BASELINE, DARK, DOCK_BOTTOM, DOCK_GAP, DOCK_H, DOCK_ICON, DOCK_PAD, LIGHT, LIGHT_D, LINE_H,
    MENU_H, MENU_ITEM_H, Palette as Theme, RADIUS, RESIZE_CORNER, TEXT_X, TITLE_H,
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
const ICON: [u32; PROGRAMS] = [0x3B8EEA, 0x8E8E93, 0x7A5AF8, 0xF5C342, 0x1FA67A];

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
/// The ZooOS mark (the owner's choice "A, the soft paw", docs/u2f-design.md),
/// in a 100-unit design box: a rounded main pad (cubic curves) and four
/// oval toe beans, the outer ones tilted out.
const PAW_PAD: [[(f32, f32); 4]; 6] = [
    [(30.0, 70.0), (30.0, 55.0), (40.0, 48.0), (50.0, 48.0)],
    [(50.0, 48.0), (60.0, 48.0), (70.0, 55.0), (70.0, 70.0)],
    [(70.0, 70.0), (70.0, 80.0), (62.0, 86.0), (56.0, 84.0)],
    [(56.0, 84.0), (53.0, 83.0), (52.0, 82.0), (50.0, 82.0)],
    [(50.0, 82.0), (48.0, 82.0), (47.0, 83.0), (44.0, 84.0)],
    [(44.0, 84.0), (38.0, 86.0), (30.0, 80.0), (30.0, 70.0)],
];
/// Toes: centre, radii, and the tilt as (cos, sin) of its angle (-25°,
/// -8°, 8°, 25°; positive turns clockwise on screen).
/// A point or pair in design units.
type Pt = (f32, f32);
const PAW_TOES: [(Pt, Pt, Pt); 4] = [
    ((23.0, 47.0), (7.0, 9.5), (0.906_308, -0.422_618)),
    ((38.0, 30.0), (7.5, 10.5), (0.990_268, -0.139_173)),
    ((62.0, 30.0), (7.5, 10.5), (0.990_268, 0.139_173)),
    ((77.0, 47.0), (7.0, 9.5), (0.906_308, 0.422_618)),
];
/// The part of the design box the mark is drawn from (x, y, side).
const PAW_VIEW: (f32, f32, f32) = (14.0, 18.0, 72.0);
/// Its size in the menu bar, and where, in logical pixels.
const PAW_SIZE: usize = 18;
const PAW_AT: (usize, usize) = (9, 4);

/// The paw's coverage mask, `side` x `side` device pixels, 4 x 4 samples
/// a pixel (docs/u2f-design.md).
fn paw_mask(side: usize) -> Vec<u8> {
    // The main pad as a polygon: each curve in 12 steps.
    let mut pad: Vec<(f32, f32)> = Vec::new();
    for c in PAW_PAD {
        for k in 0..12 {
            let t = k as f32 / 12.0;
            let u = 1.0 - t;
            let b = [u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t];
            let x = b[0] * c[0].0 + b[1] * c[1].0 + b[2] * c[2].0 + b[3] * c[3].0;
            let y = b[0] * c[0].1 + b[1] * c[1].1 + b[2] * c[2].1 + b[3] * c[3].1;
            pad.push((x, y));
        }
    }
    let inside = |x: f32, y: f32| -> bool {
        let mut odd = false;
        for k in 0..pad.len() {
            let (ax, ay) = pad[k];
            let (bx, by) = pad[(k + 1) % pad.len()];
            if (ay > y) != (by > y) && x < ax + (y - ay) * (bx - ax) / (by - ay) {
                odd = !odd;
            }
        }
        odd || PAW_TOES.iter().any(|&((cx, cy), (rx, ry), (c, s))| {
            let (dx, dy) = (x - cx, y - cy);
            let (u, v) = (c * dx + s * dy, -s * dx + c * dy);
            (u / rx) * (u / rx) + (v / ry) * (v / ry) <= 1.0
        })
    };
    let (vx, vy, vs) = PAW_VIEW;
    let mut mask = alloc::vec![0u8; side * side];
    for r in 0..side {
        for c in 0..side {
            let mut n = 0u32;
            for sy in 0..4 {
                for sx in 0..4 {
                    let x = vx + (c as f32 + (sx as f32 + 0.5) / 4.0) / side as f32 * vs;
                    let y = vy + (r as f32 + (sy as f32 + 0.5) / 4.0) / side as f32 * vs;
                    n += u32::from(inside(x, y));
                }
            }
            mask[r * side + c] = (n * 255 / 16) as u8;
        }
    }
    mask
}

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
/// The dock's offset when hidden: below the screen's bottom edge, with
/// its outline (docs/u2e-design.md).
const DOCK_HIDDEN: f32 = (DOCK_H + DOCK_BOTTOM + 4) as f32;
/// The dock slides in this fast, out this fast, and hides this long after
/// the pointer leaves it (milliseconds).
const DOCK_SHOW_MS: u64 = 250;
const DOCK_HIDE_MS: u64 = 300;
const DOCK_HIDE_DELAY_MS: u64 = 400;
/// The dock shows when the pointer is this close to the bottom edge
/// (logical pixels).
const DOCK_EDGE: usize = 6;

/// The About and Keyboard Shortcuts notes (docs/u4a-design.md).
const ABOUT: &str = concat!(
    "ZooOS ",
    env!("CARGO_PKG_VERSION"),
    ", a Rust operating system."
);
const SHORTCUTS: &str = "Ctrl+N new window, Ctrl+W close, Ctrl+M minimise.";

/// Where the appearance is kept, in the home folder.
const APPEARANCE_FILE: &str = ".appearance";

/// Where the dock's kept applications are saved, in the home folder.
const DOCK_FILE: &str = ".dock";

/// Applications, in dock order; Notes is the last.
const PROGRAMS: usize = 5;
const NOTES: usize = 3;
const NETWORK: usize = 4;
/// Window slots (docs/u3-design.md): the first `PROGRAMS` are each
/// application's first window, in catalog order; the rest hold further
/// windows (Ctrl+N).
const WINDOWS: usize = 9;
/// The applications, in dock order: (label, program, gets the home
/// directory).
const CATALOG: [(&str, &str, bool); PROGRAMS] = [
    ("Files", "files", true),
    ("Crash Test", "crash-test", false),
    ("Bella", "bella", true),
    ("Notes", "notes", true),
    ("Network", "network", false),
];
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
const TEXT_COLS: usize = 240;
/// Sidebar items a window can show (docs/u3f-design.md).
const SIDE_ITEMS: usize = 12;

/// Test-only fault address (docs/m4-design.md §4, the F12 key).
const KERNEL_ADDR: u64 = 0xFFFF_C000_0000_1000;

#[derive(Clone)]
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

/// Keys held for one app while its queue is full, at most (docs/keys-design.md).
const HELD_KEYS: usize = 256;

struct App {
    /// Keys typed for it that its queue had no room for yet, oldest first:
    /// delivered in order as it reads (docs/keys-design.md).
    held: alloc::collections::VecDeque<Line>,
    /// Keys held during the current burst, for the log line when it ends.
    held_burst: usize,
    /// The window that asked it to do an action, waiting for its answer
    /// (docs/actions-design.md).
    asked_by: Option<usize>,
    /// Its name in the dock and banners, e.g. "Crash Test".
    label: &'static str,
    program: &'static str,
    /// Gets a copy of the home directory handle as its second handle.
    wants_home: bool,
    /// Its application, an index into `CATALOG` (and the dock).
    prog: usize,
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
    /// On the heap: eight windows of these are too much for the stack.
    lines: Vec<Text>,
    count: usize,
    selected: Option<usize>,
    /// A request waiting to be handled (`Request`).
    request: Option<Request>,
    /// Sidebar items (docs/u3f-design.md); none: no sidebar.
    side: Vec<Text>,
    sides: usize,
    side_selected: Option<usize>,
}

impl App {
    fn new(prog: usize) -> App {
        let (label, program, wants_home) = CATALOG[prog];
        App {
            asked_by: None,
            held: alloc::collections::VecDeque::new(),
            held_burst: 0,
            label,
            program,
            wants_home,
            prog,
            chan: 0,
            process: 0,
            running: false,
            closed: false,
            minimized: false,
            restarts: 0,
            title: Text::new(),
            title_changed: false,
            changed: 0,
            lines: (0..MAX_LINES).map(|_| Text::new()).collect(),
            count: 0,
            selected: None,
            request: None,
            side: (0..SIDE_ITEMS).map(|_| Text::new()).collect(),
            sides: 0,
            side_selected: None,
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
        } else if let Some(items) = msg.strip_prefix(b"menu:") {
            let mut labels = [const { Text::new() }; MENU_ITEMS];
            let mut n = 0;
            for item in items.split(|&b| b == b'|').take(MENU_ITEMS) {
                labels[n].set(item);
                n += 1;
            }
            self.request = Some(Request::Menu(alloc::boxed::Box::new(labels), n));
        } else if let Some(rest) = msg.strip_prefix(b"side:") {
            // The sidebar is drawn with the frame: a change redraws the
            // window (as a new title does).
            if let Some(colon) = rest.iter().position(|&b| b == b':')
                && let Some(i) = parse_num(&rest[..colon])
                && i < SIDE_ITEMS
            {
                let t = &rest[colon + 1..];
                let n = t.len().min(TEXT_COLS);
                if self.side[i].buf[..self.side[i].len] != t[..n] {
                    self.side[i].set(t);
                    self.title_changed = true;
                }
            }
        } else if let Some(n) = msg.strip_prefix(b"sides:") {
            if let Some(n) = parse_num(n) {
                let n = n.min(SIDE_ITEMS);
                if n != self.sides {
                    self.sides = n;
                    self.title_changed = true;
                }
            }
        } else if let Some(s) = msg.strip_prefix(b"side-sel:") {
            let new = parse_num(s);
            if new != self.side_selected {
                self.side_selected = new;
                self.title_changed = true;
            }
        } else if let Some(rest) = msg.strip_prefix(b"open:") {
            if let Some(colon) = rest.iter().position(|&b| b == b':')
                && let Some(prog) = program_index(&rest[..colon])
            {
                let mut path = Text::new();
                path.set(&rest[colon + 1..]);
                self.request = Some(Request::Open(prog, alloc::boxed::Box::new(path)));
            }
        } else if msg == b"context?" {
            self.request = Some(Request::Context);
        } else if let Some(p) = msg.strip_prefix(b"dock-add:") {
            self.request = program_index(p).map(Request::DockAdd);
        } else if let Some(p) = msg.strip_prefix(b"launch:") {
            self.request = program_index(p).map(Request::Launch);
        } else if let Some(rest) = msg.strip_prefix(b"do:") {
            // do:<program>:<action>:<argument>, checked against the
            // manifest (docs/actions-design.md).
            self.request = Some(match zoo_abi::actions::parse(rest) {
                Some((a, arg)) if a.effect != zoo_abi::actions::Effect::Delete => {
                    match program_index(a.program.as_bytes()) {
                        Some(prog) => {
                            let mut line = alloc::boxed::Box::new(Line::new());
                            let _ = write!(line, "do:{}:{}", a.name, arg);
                            Request::Do(prog, line)
                        }
                        None => Request::BadDo,
                    }
                }
                _ => Request::BadDo,
            });
        } else if let Some(said) = msg.strip_prefix(b"did:") {
            let mut t = alloc::boxed::Box::new(Line::new());
            let _ = t.write_str(core::str::from_utf8(said).unwrap_or("done"));
            self.request = Some(Request::Did(t));
        } else if msg == b"split-front" {
            self.request = Some(Request::SplitFront);
        } else if let Some(rest) = msg.strip_prefix(b"window:") {
            // window:<action>[@<monitor>]:<program>[#<m>]
            if let Some(colon) = rest.iter().position(|&b| b == b':') {
                let act = &rest[..colon];
                let (act, monitor) = match act.iter().position(|&b| b == b'@') {
                    // `@next`: the monitor after the window's own.
                    Some(at) if &act[at + 1..] == b"next" => (&act[..at], Some(NEXT_MONITOR)),
                    Some(at) => (
                        &act[..at],
                        parse_num(&act[at + 1..]).and_then(|n| n.checked_sub(1)),
                    ),
                    None => (act, None),
                };
                let Some(action) = WinAction::parse(act) else {
                    return false;
                };
                let name = &rest[colon + 1..];
                let (name, m) = match name.iter().position(|&b| b == b'#') {
                    Some(h) => (&name[..h], parse_num(&name[h + 1..]).unwrap_or(0)),
                    None => (name, 0),
                };
                if let Some(prog) = program_index(name) {
                    self.request = Some(Request::Window(action, prog, m.min(2), monitor));
                }
            }
        }
        false
    }
}

/// `Request::Window`'s monitor for "the next one".
const NEXT_MONITOR: usize = usize::MAX;

/// What Bella says, and the event, for column k of n.
fn side_name(k: usize, n: usize) -> (&'static str, &'static str) {
    match (k, n) {
        (0, 1) => ("fills the screen", "maximized"),
        (0, _) => ("is on the left", "put on the left"),
        (1, 3) => ("is in the middle", "put in the middle"),
        _ => ("is on the right", "put on the right"),
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

    /// An anti-aliased shape in logical box (x, y, w, h): `inside(u, v)`
    /// says whether a point (logical units from the box's corner) is in
    /// it; each device pixel is sampled 4 x 4 times (docs/u3f-design.md).
    fn shape(
        &mut self,
        x: usize,
        y: usize,
        w: usize,
        h: usize,
        c: u32,
        inside: impl Fn(f32, f32) -> bool,
    ) {
        let s = self.s;
        if !self.visible(x, y, w, h) {
            return;
        }
        let step = 1.0 / (4 * s) as f32;
        for dy in 0..h * s {
            for dx in 0..w * s {
                let mut n = 0u32;
                for sy in 0..4 {
                    for sx in 0..4 {
                        let u = (dx * 4 + sx) as f32 * step + step / 2.0;
                        let v = (dy * 4 + sy) as f32 * step + step / 2.0;
                        n += u32::from(inside(u, v));
                    }
                }
                if n > 0 {
                    self.blend_dev(x * s + dx, y * s + dy, c, (n * 255 / 16) as u8);
                }
            }
        }
    }

    /// As `shape`, filled with a vertical gradient from `top` to `bottom`
    /// over the box, and blended by `alpha` (docs/u3g-design.md).
    #[allow(clippy::too_many_arguments)]
    fn shade(
        &mut self,
        x: usize,
        y: usize,
        w: usize,
        h: usize,
        (top, bottom): (u32, u32),
        alpha: u8,
        inside: impl Fn(f32, f32) -> bool,
    ) {
        let s = self.s;
        if !self.visible(x, y, w, h) {
            return;
        }
        let step = 1.0 / (4 * s) as f32;
        let rows = (h * s).max(1);
        for dy in 0..h * s {
            let c = blend(top, bottom, (dy * 255 / rows) as u8);
            for dx in 0..w * s {
                let mut n = 0u32;
                for sy in 0..4 {
                    for sx in 0..4 {
                        let u = (dx * 4 + sx) as f32 * step + step / 2.0;
                        let v = (dy * 4 + sy) as f32 * step + step / 2.0;
                        n += u32::from(inside(u, v));
                    }
                }
                if n > 0 {
                    let a = n * u32::from(alpha) / 16;
                    self.blend_dev(x * s + dx, y * s + dy, c, a as u8);
                }
            }
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

// ---- Context menus (docs/u3-design.md) -----------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum MenuAction {
    NewWindow,
    Open,
    Remove,
    Keep,
    Quit,
    /// A separator line.
    Line,
    /// Item `n` of an application's own menu (docs/u3-design.md).
    App(usize),
    // Menu-bar menus (docs/u4a-design.md).
    Close,
    Minimize,
    Zoom,
    /// Bring window `i` forward (restoring it if minimised).
    Focus(usize),
    Appearance,
    About,
    Shortcuts,
    /// Shown dim; not choosable.
    Disabled,
}

impl MenuAction {
    fn label(self) -> &'static str {
        match self {
            MenuAction::NewWindow => "New Window",
            MenuAction::Open => "Open",
            MenuAction::Remove => "Remove from Dock",
            MenuAction::Keep => "Keep in Dock",
            MenuAction::Quit => "Quit",
            MenuAction::Close => "Close Window\tCtrl+W",
            MenuAction::Minimize => "Minimize\tCtrl+M",
            MenuAction::Zoom => "Zoom",
            MenuAction::About => "About ZooOS",
            MenuAction::Shortcuts => "Keyboard Shortcuts",
            _ => "",
        }
    }
}

/// The menu bar's titles (docs/u4a-design.md): the ZooOS mark, the
/// focused application's name, then `MENUS`.
const BAR_TITLES: usize = 2 + MENUS.len();

/// What an application asked the window server for, besides drawing.
enum Request {
    /// A context menu of these items at the pointer.
    Menu(alloc::boxed::Box<[Text; MENU_ITEMS]>, usize),
    /// Keep this application in the dock.
    DockAdd(usize),
    /// Open this application.
    Launch(usize),
    /// Open this file in a window of this application.
    Open(usize, alloc::boxed::Box<Text>),
    /// What is on the screen and what just happened, for Bella
    /// (docs/context-design.md).
    Context,
    /// Do this to a window of this application (docs/window-actions-design.md):
    /// its `m`th window from the front (0: the front one), opened if there
    /// are fewer (docs/tiling-design.md); on this monitor, when given
    /// (0-based; docs/monitors-design.md).
    Window(WinAction, usize, usize, Option<usize>),
    /// The two front windows side by side ("split these two windows";
    /// docs/tiling-design.md).
    SplitFront,
    /// An application action for another (docs/actions-design.md): the
    /// target program and the `do:<action>:<argument>` line it gets.
    Do(usize, alloc::boxed::Box<Line>),
    /// This application's answer to an action it was asked to do.
    Did(alloc::boxed::Box<Line>),
    /// An invocation the manifest refuses.
    BadDo,
}

/// What an application may ask done to another's window: Bella's
/// `maximize`, `minimize` and `close` (docs/window-actions-design.md).
#[derive(Clone, Copy, PartialEq, Eq)]
enum WinAction {
    /// Open it if need be, bring it to the front, and fill the screen.
    Maximize,
    Minimize,
    Close,
    /// Like `Maximize`, but column k of n side by side
    /// (docs/tiling-design.md).
    Column(usize, usize),
    /// To another monitor, at the same place on it
    /// (docs/monitors-design.md).
    Move,
}

impl WinAction {
    fn parse(s: &[u8]) -> Option<WinAction> {
        match s {
            b"maximize" => Some(WinAction::Maximize),
            b"minimize" => Some(WinAction::Minimize),
            b"close" => Some(WinAction::Close),
            b"move" => Some(WinAction::Move),
            b"left" => Some(WinAction::Column(0, 2)),
            b"right" => Some(WinAction::Column(1, 2)),
            // col<k>of<n>
            _ => {
                let s = s.strip_prefix(b"col")?;
                let of = s.windows(2).position(|w| w == b"of")?;
                let k = parse_num(&s[..of])?;
                let n = parse_num(&s[of + 2..])?;
                ((1..=3).contains(&n) && k < n).then_some(WinAction::Column(k, n))
            }
        }
    }
}

/// Whether (u, v) is in the rounded rectangle (x0, y0, x1, y1) of
/// corner radius `r`.
fn rr(u: f32, v: f32, (x0, y0, x1, y1): (f32, f32, f32, f32), r: f32) -> bool {
    if u < x0 || u > x1 || v < y0 || v > y1 {
        return false;
    }
    // Not `clamp`: it panics when rounding makes x0 + r > x1 - r.
    let cx = u.max(x0 + r).min(x1 - r);
    let cy = v.max(y0 + r).min(y1 - r);
    (u - cx) * (u - cx) + (v - cy) * (v - cy) <= r * r
}

fn circle(u: f32, v: f32, cx: f32, cy: f32, r: f32) -> bool {
    (u - cx) * (u - cx) + (v - cy) * (v - cy) <= r * r
}

/// A dock icon's image: the icon with room for its shadow, the icon at
/// `ICON_AT` in it (logical pixels).
const ICON_BOX: (usize, usize) = (MAG_ICON + 8, MAG_ICON + 10);
const ICON_AT: (usize, usize) = (4, 2);

/// Dock magnification (docs/u2g-design.md): the largest icon size, the
/// reach of the bell around the pointer, and how long it takes to grow or
/// settle back.
const MAG_ICON: usize = 84;
const MAG_REACH: f32 = 150.0;
const MAG_MS: u64 = 140;

/// The launch bounce: its length, and how high the first hop goes.
const BOUNCE_MS: u64 = 640;
const BOUNCE_HIGH: f32 = 24.0;

/// Bilinear blend of four premultiplied pixels (fx, fy in 0..1); the
/// result is premultiplied too, so each channel stays at most its alpha.
fn bilerp(a: u32, b: u32, c: u32, d: u32, fx: f32, fy: f32) -> u32 {
    let ch = |shift: u32| {
        let g = |p: u32| ((p >> shift) & 0xFF) as f32;
        let top = g(a) + (g(b) - g(a)) * fx;
        let bot = g(c) + (g(d) - g(c)) * fx;
        ((top + (bot - top) * fy + 0.5) as u32).min(255)
    };
    let al = ch(24);
    al << 24 | ch(16).min(al) << 16 | ch(8).min(al) << 8 | ch(0).min(al)
}

/// A four-pointed sparkle centred at (cx, cy), `r` from centre to tip.
fn sparkle(u: f32, v: f32, cx: f32, cy: f32, r: f32) -> bool {
    let (a, b) = ((u - cx).abs(), (v - cy).abs());
    a / r + b / (r * 0.3) <= 1.0 || a / (r * 0.3) + b / r <= 1.0 || a + b <= r * 0.42
}

/// Application `k`'s icon, `d` logical pixels square, at (x, y)
/// (docs/u3g-design.md): a tile lit from above (a vertical gradient), a
/// bright rim along its top and a darker one along its bottom, a soft
/// shadow under it, and its symbol with a shadow of its own.
#[allow(clippy::manual_range_contains)]
fn app_icon(s: &mut Screen, k: usize, x: usize, y: usize, d: usize) {
    let f = d as f32;
    let rad = 0.225 * f;
    let tile = move |u: f32, v: f32| rr(u, v, (0.0, 0.0, f, f), rad);
    let big = d >= 32;
    if big && x >= 4 && y >= 2 {
        // The shadow: three widening, faint layers, a little lower.
        for (grow, a) in [(3.0f32, 10u8), (2.0, 14), (1.0, 20)] {
            s.shade(x - 4, y - 2, d + 8, d + 10, (0, 0), a, move |u, v| {
                rr(
                    u - 4.0,
                    v - 4.0,
                    (-grow, -grow, f + grow, f + grow),
                    rad + grow,
                )
            });
        }
    }
    let colours = [
        (0x6CC4FF, 0x1F7AE8),
        (0xB9B9C0, 0x6C6C73),
        (0xB497FF, 0x5A36E0),
        (0xFFFFFF, 0xECECEF),
        (0x5DDBA8, 0x14946A),
    ];
    s.shade(x, y, d, d, colours[k], 255, tile);
    if k == 3 {
        // Notes: a yellow band at the top, ruled lines below it.
        s.shade(
            x,
            y,
            d,
            d * 32 / 100,
            (0xFFE27A, 0xF6C12E),
            255,
            move |u, v| tile(u, v) && v < 0.3 * f,
        );
        s.shade(x, y, d, d, (0xD9A621, 0xD9A621), 160, move |u, v| {
            tile(u, v) && v >= 0.3 * f && v < 0.3 * f + f / 48.0 * 1.5
        });
        for row in [0.47f32, 0.6, 0.73] {
            s.shade(x, y, d, d, (0xC9C9CE, 0xC9C9CE), 255, move |u, v| {
                u >= 0.18 * f && u <= 0.82 * f && v >= row * f && v <= row * f + f / 32.0
            });
        }
    }
    // The rims: a bright edge along the top, a darker one along the bottom.
    let w = (f / 40.0).max(0.6);
    s.shade(x, y, d, d, (0xFFFFFF, 0xFFFFFF), 90, move |u, v| {
        tile(u, v) && !rr(u, v - w, (0.0, 0.0, f, f), rad) && v < f * 0.5
    });
    s.shade(x, y, d, d, (0, 0), 50, move |u, v| {
        tile(u, v) && !rr(u, v + w, (0.0, 0.0, f, f), rad) && v > f * 0.5
    });
    if k == 3 {
        return;
    }
    // The symbol, in unit coordinates, and its colours.
    let glyph = move |u: f32, v: f32| -> bool {
        let (u, v) = (u / f, v / f);
        match k {
            0 => rr(u, v, (0.2, 0.28, 0.52, 0.42), 0.04) || rr(u, v, (0.2, 0.34, 0.8, 0.76), 0.06),
            1 => v <= 0.78 && v >= 0.2 && (u - 0.5).abs() <= (v - 0.2) * 0.6,
            // A globe: a ring, the equator, two parallels and a meridian.
            4 => {
                let (du, dv) = (u - 0.5, v - 0.5);
                let r2 = du * du + dv * dv;
                let ring = r2 <= 0.30 * 0.30 && r2 >= 0.255 * 0.255;
                let inside = r2 < 0.255 * 0.255;
                let line = |d: f32| d.abs() <= 0.022;
                let meridian =
                    (du * du / (0.12 * 0.12) + dv * dv / (0.28 * 0.28) - 1.0).abs() <= 0.16;
                ring || (inside
                    && (line(dv) || line(dv - 0.13) || line(dv + 0.13) || line(du) || meridian))
            }
            _ => sparkle(u, v, 0.45, 0.54, 0.3) || sparkle(u, v, 0.74, 0.27, 0.11),
        }
    };
    let ink = [
        (0xD6EBFF, 0xBFDDFB),
        (0xFFE066, 0xF5B400),
        (0xFFFFFF, 0xEDE6FF),
        (0, 0),
        (0xFFFFFF, 0xE2F7EE),
    ][k];
    let drop = (f / 36.0).max(0.5);
    s.shade(x, y, d, d, (0, 0), 70, move |u, v| glyph(u, v - drop));
    s.shade(x, y, d, d, ink, 255, glyph);
    match k {
        // Files: the folder's lighter front.
        0 => s.shade(x, y, d, d, (0xFFFFFF, 0xE4F0FC), 255, move |u, v| {
            rr(u / f, v / f, (0.2, 0.42, 0.8, 0.76), 0.06)
        }),
        // Crash Test: an exclamation mark on the warning sign.
        1 => s.shade(x, y, d, d, (0x2C2C2E, 0x2C2C2E), 255, move |u, v| {
            let (u, v) = (u / f, v / f);
            rr(u, v, (0.47, 0.4, 0.53, 0.6), 0.03) || circle(u, v, 0.5, 0.69, 0.036)
        }),
        _ => {}
    }
}

/// The colour of markup role `r` (docs/u3f-design.md).
fn role_colour(t: &Theme, r: u8, fg: u32) -> u32 {
    match r {
        b'k' => t.syntax[0],
        b's' => t.syntax[1],
        b'c' => t.syntax[2],
        b'n' => t.syntax[3],
        b't' => t.syntax[4],
        b'f' => t.syntax[5],
        b'm' => t.syntax[6],
        b'd' => t.text_dim,
        _ => fg,
    }
}

/// The application whose program is `program`.
fn program_index(program: &[u8]) -> Option<usize> {
    // Bella was called Bella before 0.77.0; docks saved then still
    // name `assistant`.
    let program = if program == b"assistant" {
        b"bella".as_slice()
    } else {
        program
    };
    CATALOG.iter().position(|c| c.1.as_bytes() == program)
}

/// What is under the pointer in the dock: an application's icon or a
/// minimised window's thumbnail (docs/u3-design.md).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum DockHit {
    App(usize),
    Window(usize),
}

/// Most items in one menu.
const MENU_ITEMS: usize = 16;

/// An open context menu: for application `prog`, at logical (x, y), `w`
/// wide, its items, and the one under the pointer.
struct Menu {
    /// The window whose application asked for it, for an application's
    /// own menu.
    owner: Option<usize>,
    /// Labels of `MenuAction::App` items.
    labels: [Text; MENU_ITEMS],
    prog: usize,
    x: usize,
    y: usize,
    w: usize,
    items: [MenuAction; MENU_ITEMS],
    n: usize,
    hover: Option<usize>,
    /// The menu-bar title it hangs from, if it is a menu-bar menu.
    bar: Option<usize>,
}

impl Menu {
    fn label(&self, k: usize) -> &str {
        if self.labels[k].len > 0 {
            return self.labels[k].as_str();
        }
        self.items[k].label()
    }

    fn separators(&self) -> [bool; MENU_ITEMS] {
        core::array::from_fn(|k| self.items[k] == MenuAction::Line)
    }

    fn height(&self) -> usize {
        zoo_abi::ui::menu_height(&self.separators()[..self.n])
    }

    /// The item at logical (px, py), if a choosable one.
    fn item_at(&self, px: usize, py: usize) -> Option<usize> {
        if px < self.x || px >= self.x + self.w {
            return None;
        }
        let seps = self.separators();
        (0..self.n).find(|&k| {
            let top = zoo_abi::ui::menu_item_top(self.y, &seps[..self.n], k);
            !matches!(self.items[k], MenuAction::Line | MenuAction::Disabled)
                && py >= top
                && py < top + MENU_ITEM_H
        })
    }
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
            AnimKind::Minimize => 520,
            AnimKind::Restore => 460,
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
    /// Time elapsed, 0..=1, without easing.
    fn raw(&self, now: u64) -> f32 {
        if self.dur == 0 {
            1.0
        } else {
            (now.saturating_sub(self.start) as f32 / self.dur as f32).min(1.0)
        }
    }

    /// Minimise and restore are drawn as the genie (`composite_genie`):
    /// how far along it is (0: the window, 1: in the dock).
    fn genie(&self, now: u64) -> Option<f32> {
        match self.kind {
            AnimKind::Minimize => Some(self.raw(now)),
            AnimKind::Restore => Some(1.0 - self.raw(now)),
            _ => None,
        }
    }

    /// The logical area this frame draws in.
    fn bounds(&self, now: u64) -> (usize, usize, usize, usize) {
        if self.genie(now).is_some() {
            // The whole funnel, window to dock icon.
            let (a, b) = (self.from, self.to);
            let x0 = floor(a.0.min(b.0)).max(0.0);
            let y0 = floor(a.1.min(b.1)).max(0.0);
            let x1 = ceil((a.0 + a.2).max(b.0 + b.2)) + 2.0;
            let y1 = ceil((a.1 + a.3).max(b.1 + b.3)) + 2.0;
            return (
                x0 as usize,
                y0 as usize,
                (x1 - x0) as usize,
                (y1 - y0) as usize,
            );
        }
        layer_bounds(self.at(now).0, self.size)
    }

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

/// The genie (the macOS minimise): layer `l` (a window of logical `size`)
/// drawn between the window rectangle `win` and the dock icon `icon` at
/// progress `g` (0: the window, 1: inside the icon).
///
/// Every row of the window is stretched between a left and a right edge.
/// The edges are S-curves from the window's width at its top to the
/// icon's width at the icon's bottom. In the first 40 % the curves bend in
/// (the bottom of the window pinches toward the icon); in the rest the
/// window slides down the funnel, its bottom ahead of its top, and fades
/// at the very end. Only the window body is drawn, not its shadow.
#[allow(clippy::too_many_arguments)]
fn composite_genie(
    back: &mut [u32],
    sw: usize,
    clip: (usize, usize, usize, usize),
    s: usize,
    l: &Layer,
    size: (usize, usize),
    win: RectF,
    icon: RectF,
    g: f32,
) {
    if l.w == 0 || g >= 1.0 {
        return;
    }
    let smooth = |u: f32| {
        let u = u.clamp(0.0, 1.0);
        u * u * (3.0 - 2.0 * u)
    };
    let p1 = smooth(g / 0.4);
    let p2 = {
        let u = ((g - 0.4) / 0.6).clamp(0.0, 1.0);
        u * u
    };
    let (wb, ib) = (win.1 + win.3, icon.1 + icon.3);
    // The body's top and bottom now.
    let top = win.1 + (icon.1 - win.1) * p2;
    let bottom = wb + (ib - wb) * (p2 * 1.6).min(1.0);
    if bottom - top < 0.5 {
        return;
    }
    // The edges at logical height y.
    let span = (ib - win.1).max(1.0);
    let edge = |y: f32| {
        let k = p1 * smooth((y - win.1) / span);
        (
            win.0 + (icon.0 - win.0) * k,
            win.0 + win.2 + (icon.0 + icon.2 - win.0 - win.2) * k,
        )
    };
    let alpha = 1.0 - ((g - 0.85) / 0.15).clamp(0.0, 1.0) * 0.85;
    let k = (alpha * 256.0) as u32;
    let sf = s as f32;
    // The body in the layer, in device pixels.
    let (bx, by) = ((MARGIN * s) as i64, (MARGIN * s) as i64);
    let (bw, bh) = ((size.0 * s) as i64, (size.1 * s) as i64);
    let px = |x: i64, y: i64| -> u32 {
        if x < bx || y < by || x >= bx + bw || y >= by + bh {
            0
        } else {
            l.buf[y as usize * l.w + x as usize]
        }
    };
    let (cx0, cy0, cx1, cy1) = clip;
    let y0 = ((top * sf) as usize).max(cy0);
    let y1 = ((ceil(bottom * sf) as usize) + 1).min(cy1);
    for dy in y0..y1 {
        let y = (dy as f32 + 0.5) / sf;
        let f = (y - top) / (bottom - top);
        if !(0.0..=1.0).contains(&f) {
            continue;
        }
        let (le, re) = edge(y);
        let (ld, rd) = (le * sf, re * sf);
        if rd - ld < 0.5 {
            continue;
        }
        let v = ((by as f32 + f * bh as f32 - 0.5) * 65536.0) as i64;
        let (vi, vf) = (v >> 16, ((v >> 8) & 0xFF) as u32);
        let x0 = (floor(ld).max(0.0) as usize).max(cx0);
        let x1 = ((ceil(rd) as usize) + 1).min(cx1);
        if x0 >= x1 {
            continue;
        }
        let step = (bw as f32 / (rd - ld) * 65536.0) as i64;
        let mut u =
            ((bx as f32 + (x0 as f32 + 0.5 - ld) / (rd - ld) * bw as f32 - 0.5) * 65536.0) as i64;
        for d in &mut back[dy * sw + x0..dy * sw + x1] {
            let (ui, uf) = (u >> 16, ((u >> 8) & 0xFF) as u32);
            u += step;
            let t = lerp_px(px(ui, vi), px(ui + 1, vi), uf);
            let b = lerp_px(px(ui, vi + 1), px(ui + 1, vi + 1), uf);
            let p = lerp_px(t, b, vf);
            if p != 0 {
                *d = over(*d, if k >= 256 { p } else { fade_px(p, k) });
            }
        }
    }
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
    layers: [Layer; WINDOWS],
    /// Scratch buffer for building layers (the render over white).
    scratch: Vec<u32>,
    /// Application icons for the dock, drawn once at the screen's scale
    /// (docs/u3g-design.md): premultiplied, `ICON_BOX` square plus margins.
    icons: Vec<Vec<u32>>,
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
    /// Monitors side by side across `lw`, each `mw` wide; the menu bar,
    /// the dock and banners are on the first (docs/monitors-design.md).
    monitors: usize,
    mw: usize,
    atlas: Atlas<'static>,
    dark: bool,
    /// Wallpaper colour per row, for the current appearance.
    wall: Vec<u32>,
    ramp: Vec<u8>,
    apps: [App; WINDOWS],
    /// Home directory handle; copies go to Bella.
    home: u64,
    focus: usize,
    /// The last few things that happened on the desktop, oldest first
    /// (docs/context-design.md).
    recent: alloc::collections::VecDeque<Line>,
    /// Window rectangles (x, y, w, h), indexed like `apps`.
    wins: [(usize, usize, usize, usize); WINDOWS],
    /// Drawing order, back to front; the focused window is last.
    order: [usize; WINDOWS],
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
    /// The last click on a content line: (tick, window, line).
    last_line: (u64, usize, usize),
    /// A zoomed window's rectangle before zooming (docs/d9-design.md).
    unzoomed: [Option<(usize, usize, usize, usize)>; WINDOWS],
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
    /// What in the dock is under the pointer (its name shows above it).
    hover: Option<DockHit>,
    /// Each window's running animation (docs/u2d-design.md).
    anims: [Option<Anim>; WINDOWS],
    /// Time-stamp-counter cycles per millisecond (calibrated at start).
    per_ms: u64,
    /// The time this frame is drawn for (all damaged areas alike).
    frame_now: u64,
    /// Shift is held: new animations run ten times slower.
    shift: bool,
    /// Ctrl is held (Ctrl+N).
    ctrl: bool,
    /// Applications kept in the dock (shown even when not running).
    pinned: [bool; PROGRAMS],
    /// Minimised windows, in the order they went to the dock.
    minis: Vec<usize>,
    /// The open context menu, and whether the right button is held.
    menu: Option<Menu>,
    rheld: bool,
    /// The ZooOS mark's coverage, `PAW_SIZE` logical pixels square.
    paw: Vec<u8>,
    /// The dock hides itself (docs/u2e-design.md): how far below its
    /// place it is now (0 shown, `DOCK_HIDDEN` gone), whether it is meant
    /// to be shown, its slide (from, to, start, length), and when it hides
    /// after the pointer left it.
    dock_off: f32,
    dock_shown: bool,
    dock_slide: Option<(f32, f32, u64, u64)>,
    /// How much the dock magnifies (0 to 1), eased toward its target;
    /// and the pointer x it was last drawn for.
    mag: f32,
    mag_x: usize,
    /// The previous frame's time (for easing the magnification).
    frame_prev: u64,
    /// Launch bounces (docs/u2h-design.md): per application, its start
    /// and length (cycles) and the frames drawn so far.
    bounce: [Option<(u64, u64, u32)>; PROGRAMS],
    dock_hide_at: Option<u64>,
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

    /// The user switched the appearance: show it and keep it in
    /// `.appearance` in the home folder (docs/u4b-design.md).
    fn change_appearance(&mut self, dark: bool) {
        self.set_appearance(dark);
        let name = self.theme().name.as_bytes();
        let _ = zoo_rt::file_create(self.home, APPEARANCE_FILE);
        let saved = zoo_rt::file_truncate(self.home, APPEARANCE_FILE, 0)
            .and_then(|()| zoo_rt::file_write(self.home, APPEARANCE_FILE, 0, name));
        match saved {
            Ok(_) => zoo_rt::log!("PeacockHandler: appearance saved"),
            Err(e) => zoo_rt::log!("PeacockHandler: cannot save the appearance: {}", e.name()),
        }
    }

    /// The appearance kept in `.appearance`, if there is one.
    fn load_appearance(&mut self) {
        let mut buf = [0u8; 16];
        if let Ok(n) = zoo_rt::file_read(self.home, APPEARANCE_FILE, 0, &mut buf)
            && buf[..n] == *b"dark"
            && !self.dark
        {
            self.set_appearance(true);
        }
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
        self.damage(0, 0, self.mw, MENU_H);
    }

    fn dock_rect(&self) -> (usize, usize, usize, usize) {
        zoo_abi::ui::dock_rect_with(
            self.mw,
            self.lh,
            self.dock_entries().1.max(1),
            self.minis.len(),
        )
    }

    /// The applications in the dock, in order: kept ones and running ones
    /// (docs/u3-design.md); and how many.
    fn dock_entries(&self) -> ([usize; PROGRAMS], usize) {
        let mut out = [0; PROGRAMS];
        let mut n = 0;
        for k in 0..PROGRAMS {
            if self.pinned[k] || self.program_running(k) {
                out[n] = k;
                n += 1;
            }
        }
        (out, n)
    }

    /// Where application `k`'s dock icon is (its top-left, logical), if it
    /// is in the dock.
    fn dock_pos(&self, k: usize) -> Option<(usize, usize)> {
        let (e, n) = self.dock_entries();
        let slot = e[..n].iter().position(|&p| p == k)?;
        Some(zoo_abi::ui::dock_icon_with(
            self.mw,
            self.lh,
            n,
            self.minis.len(),
            slot,
        ))
    }

    /// Where minimised window `i`'s thumbnail is, if it is in the dock.
    fn mini_pos(&self, i: usize) -> Option<(usize, usize)> {
        let j = self.minis.iter().position(|&w| w == i)?;
        let n = self.dock_entries().1.max(1);
        Some(zoo_abi::ui::dock_mini(
            self.mw,
            self.lh,
            n,
            self.minis.len(),
            j,
        ))
    }

    /// Each dock item's place as drawn (docs/u2g-design.md): the left x
    /// and the size of the application icons, then of the minimised
    /// windows, magnified around the pointer; and the background's left
    /// and right. Items grow from their bottoms; the item under the
    /// pointer stays under it, so clicks find the same item as without
    /// magnification.
    fn dock_layout(&self) -> (Vec<(f32, f32)>, (f32, f32)) {
        let (e, n) = self.dock_entries();
        let base: Vec<usize> = e[..n]
            .iter()
            .filter_map(|&k| self.dock_pos(k).map(|p| p.0))
            .chain(
                self.minis
                    .iter()
                    .filter_map(|&i| self.mini_pos(i).map(|p| p.0)),
            )
            .collect();
        let d = DOCK_ICON as f32;
        let px = self.px as f32;
        let grow = (MAG_ICON - DOCK_ICON) as f32 * self.mag;
        let sizes: Vec<f32> = base
            .iter()
            .map(|&x| {
                let t = ((px - (x as f32 + d / 2.0)) / MAG_REACH).abs().min(1.0);
                let bell = (1.0 - t * t) * (1.0 - t * t);
                d + grow * bell
            })
            .collect();
        // The item under the pointer (or nearest) and how far across it.
        let k = base
            .iter()
            .position(|&x| px < x as f32 + d + DOCK_GAP as f32 / 2.0)
            .unwrap_or(base.len().saturating_sub(1));
        let frac = base
            .get(k)
            .map_or(0.5, |&x| ((px - x as f32) / d).clamp(0.0, 1.0));
        let before: f32 = sizes[..k].iter().map(|w| w - d).sum();
        let anchor = before + sizes.get(k).map_or(0.0, |w| (w - d) * frac);
        let mut extra = 0.0;
        let mut out = Vec::with_capacity(base.len());
        for (&x, &w) in base.iter().zip(&sizes) {
            out.push((x as f32 + extra - anchor, w));
            extra += w - d;
        }
        let (rx, _, rw, _) = self.dock_rect();
        let pad = DOCK_PAD as f32;
        let left = out
            .first()
            .map_or(rx as f32, |&(x, _)| x - pad)
            .min(rx as f32);
        let right = out
            .last()
            .map_or((rx + rw) as f32, |&(x, w)| x + w + pad)
            .max((rx + rw) as f32);
        (out, (left, right))
    }

    /// How high application `k`'s icon is in its launch bounce: two hops,
    /// the second smaller (docs/u2h-design.md).
    fn bounce_height(&self, k: usize) -> f32 {
        let Some((start, dur, _)) = self.bounce[k] else {
            return 0.0;
        };
        let t = (self.frame_now.saturating_sub(start) as f32 / dur.max(1) as f32).min(1.0);
        let (p, h) = if t < 0.58 {
            (t / 0.58, BOUNCE_HIGH)
        } else {
            ((t - 0.58) / 0.42, BOUNCE_HIGH * 0.4)
        };
        h * 4.0 * p * (1.0 - p)
    }

    fn damage_dock(&mut self) {
        let (_, y, _, _) = self.dock_rect();
        // The whole band (its width changes with its entries), with the
        // label above an icon, down to the bottom edge (it slides).
        let top = y.saturating_sub(40 + MAG_ICON - DOCK_ICON);
        self.damage(0, top, self.lw, self.lh - top);
    }

    /// The dock's current offset below its place, in whole logical pixels.
    fn dock_shift(&self) -> usize {
        (self.dock_off + 0.5) as usize
    }

    /// Shows or hides the dock (docs/u2e-design.md): shown while the
    /// pointer is at the bottom edge or over the dock, or a window genies
    /// to or from it; hidden a moment after the pointer leaves.
    fn update_dock(&mut self, now: u64) {
        let (dx, dy, dw, _) = self.dock_rect();
        // The bottom few rows: a scaled QEMU window (or a fast hand) can
        // stop the pointer a few pixels short of the very last row.
        let edge = self.py + DOCK_EDGE >= self.lh;
        let over = self.px + 10 >= dx && self.px < dx + dw + 10 && self.py + 50 >= dy;
        let genie = self
            .anims
            .iter()
            .flatten()
            .any(|a| matches!(a.kind, AnimKind::Minimize | AnimKind::Restore));
        let bouncing = self.bounce.iter().any(Option::is_some);
        let want = edge || genie || bouncing || self.menu.is_some() || (self.dock_shown && over);
        let ms = self.per_ms;
        if want {
            self.dock_hide_at = None;
            if !self.dock_shown {
                self.dock_shown = true;
                self.dock_slide = Some((self.dock_off, 0.0, now, DOCK_SHOW_MS * ms));
            }
        } else if self.dock_shown {
            match self.dock_hide_at {
                None => self.dock_hide_at = Some(now + DOCK_HIDE_DELAY_MS * ms),
                Some(t) if now >= t => {
                    self.dock_hide_at = None;
                    self.dock_shown = false;
                    self.dock_slide = Some((self.dock_off, DOCK_HIDDEN, now, DOCK_HIDE_MS * ms));
                    self.hover = None;
                }
                Some(_) => {}
            }
        }
        // Magnification: grows while the pointer is over the icons, settles
        // back after; redrawn as it changes and as the pointer moves.
        let (_, _, _, dh) = self.dock_rect();
        let on_band = self.dock_shown
            && self.dock_slide.is_none()
            && self.menu.is_none()
            && self.py >= dy
            && self.py < dy + dh
            && self.px + 10 >= dx
            && self.px < dx + dw + 10;
        let target = if on_band { 1.0 } else { 0.0 };
        if self.mag != target {
            let step = (now.saturating_sub(self.frame_prev) as f32 / (MAG_MS * ms).max(1) as f32)
                .max(0.05);
            self.mag = if target > self.mag {
                (self.mag + step).min(1.0)
            } else {
                (self.mag - step).max(0.0)
            };
            self.damage_dock();
        } else if self.mag > 0.0 && self.px != self.mag_x {
            self.damage_dock();
        }
        self.frame_prev = now;
        // Launch bounces: drawn every frame until done.
        for k in 0..PROGRAMS {
            let Some((start, dur, frames)) = self.bounce[k] else {
                continue;
            };
            self.damage_dock();
            if now.saturating_sub(start) >= dur {
                self.bounce[k] = None;
                zoo_rt::log!(
                    "PeacockHandler: animation bounce {}: frames {} time {} ms",
                    CATALOG[k].0,
                    frames,
                    now.saturating_sub(start) / ms.max(1)
                );
            } else {
                self.bounce[k] = Some((start, dur, frames + 1));
            }
        }
        if let Some((from, to, start, dur)) = self.dock_slide {
            self.damage_dock();
            let t = (now.saturating_sub(start) as f32 / dur.max(1) as f32).min(1.0);
            // In: slowing at the end; out: speeding away.
            let e = if to < from {
                1.0 - (1.0 - t) * (1.0 - t) * (1.0 - t)
            } else {
                t * t
            };
            self.dock_off = from + (to - from) * e;
            if t >= 1.0 {
                self.dock_slide = None;
                zoo_rt::log!(
                    "PeacockHandler: dock {}",
                    if self.dock_shown { "shown" } else { "hidden" }
                );
            }
        }
    }

    /// Shows a notification banner.
    fn notify(&mut self, title: &'static str, text: &str) {
        let mut body = Line::new();
        let _ = body.write_str(text);
        self.damage_banner();
        self.banner = Some(Banner {
            title,
            body,
            until: zoo_rt::ticks() + BANNER_TICKS,
        });
        self.damage_banner();
        zoo_rt::log!("PeacockHandler: banner {}: {}", title, text);
    }

    fn banner_rect(&self) -> (usize, usize, usize, usize) {
        (self.mw - 16 - BANNER_W, MENU_H + 10, BANNER_W, BANNER_H)
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
        } else if self.order[WINDOWS - 1] != i {
            self.damage_window(i);
        }
        self.focus = i;
        let mut order = [0; WINDOWS];
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

    /// Window `i`'s rectangle on monitor `to`: filled if it fills its own,
    /// else at the same place on the other monitor.
    fn moved_to(&self, i: usize, to: usize) -> (usize, usize, usize, usize) {
        let from = self.monitor_of(i);
        if self.wins[i] == self.zoomed_on(from) {
            return self.zoomed_on(to);
        }
        let (x, y, w, h) = self.wins[i];
        let x = x - from * self.mw + to * self.mw;
        (x.min((to + 1) * self.mw - w.min(self.mw)), y, w, h)
    }

    /// Window `i` to `rect` keeping its size and place from before (a
    /// move, not a resize).
    fn move_to(&mut self, i: usize, rect: (usize, usize, usize, usize)) {
        self.damage_window(i);
        self.drag = None;
        self.wins[i] = rect;
        zoo_rt::log!(
            "PeacockHandler: placed {} at {}x{} {}x{}",
            self.apps[i].label,
            rect.0,
            rect.1,
            rect.2,
            rect.3
        );
        self.damage_window(i);
    }

    /// The monitor (0-based) window `i`'s centre is on.
    fn monitor_of(&self, i: usize) -> usize {
        let (x, _, w, _) = self.wins[i];
        ((x + w / 2) / self.mw.max(1)).min(self.monitors - 1)
    }

    /// The zoomed space (between the menu bar's height and the dock's) of
    /// monitor `m` (docs/monitors-design.md).
    fn zoomed_on(&self, m: usize) -> (usize, usize, usize, usize) {
        let (x, y, w, h) = zoo_abi::ui::zoomed(self.mw, self.lh);
        (x + m * self.mw, y, w, h)
    }

    /// Column `k` of `n` on monitor `m`.
    fn column_on(&self, m: usize, k: usize, n: usize) -> (usize, usize, usize, usize) {
        let (x, y, w, h) = zoo_abi::ui::column(self.mw, self.lh, k, n);
        (x + m * self.mw, y, w, h)
    }

    /// Window `i` to `rect` (filled or half the screen). The size and place
    /// from before are kept, once, so the green light gives them back
    /// (docs/tiling-design.md).
    fn place(&mut self, i: usize, rect: (usize, usize, usize, usize)) {
        if self.wins[i] == rect {
            return;
        }
        self.damage_window(i);
        self.drag = None;
        if self.unzoomed[i].is_none() {
            self.unzoomed[i] = Some(self.wins[i]);
        }
        self.wins[i] = rect;
        zoo_rt::log!(
            "PeacockHandler: placed {} at {}x{} {}x{}",
            self.apps[i].label,
            rect.0,
            rect.1,
            rect.2,
            rect.3
        );
        self.damage_window(i);
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
                self.wins[i] = self.zoomed_on(self.monitor_of(i));
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

    /// What in the dock is under the pointer.
    fn dock_hit(&self) -> Option<DockHit> {
        if let Some(k) = self.dock_slot() {
            return Some(DockHit::App(k));
        }
        if !self.dock_shown || self.dock_off > 1.0 {
            return None;
        }
        self.minis.iter().copied().find_map(|i| {
            let (x, y) = self.mini_pos(i)?;
            (self.px >= x && self.px < x + DOCK_ICON && self.py >= y && self.py < y + DOCK_ICON)
                .then_some(DockHit::Window(i))
        })
    }

    /// The dock icon under the pointer.
    fn dock_slot(&self) -> Option<usize> {
        if !self.dock_shown || self.dock_off > 1.0 {
            return None;
        }
        let (w, h) = (self.lw, self.lh);
        let _ = (w, h);
        let (e, n) = self.dock_entries();
        e[..n].iter().copied().find(|&k| {
            let Some((ix, iy)) = self.dock_pos(k) else {
                return false;
            };
            self.px >= ix && self.px < ix + DOCK_ICON && self.py >= iy && self.py < iy + DOCK_ICON
        })
    }

    /// Whether any window of application `k` is open.
    fn program_running(&self, k: usize) -> bool {
        self.apps
            .iter()
            .any(|a| a.prog == k && !a.closed && a.running)
    }

    /// A dock icon (application `k`, docs/u3-design.md): raises its
    /// frontmost window, or shows a minimised one, or opens its first
    /// window when none is open.
    fn dock_click(&mut self, k: usize) {
        self.damage_dock();
        let open = |a: &App| a.prog == k && !a.closed;
        if let Some(i) = self
            .order
            .iter()
            .rev()
            .copied()
            .find(|&i| open(&self.apps[i]) && self.apps[i].visible())
        {
            return self.set_focus(i);
        }
        let i = (0..WINDOWS)
            .find(|&i| open(&self.apps[i]) && self.apps[i].minimized)
            .unwrap_or(k);
        if self.apps[i].closed {
            let app = &mut self.apps[i];
            app.restarts = 0;
            if !app.start(self.home) {
                zoo_rt::log!("PeacockHandler: cannot start {}", app.program);
                return;
            }
            app.closed = false;
            zoo_rt::log!("PeacockHandler: opened {}", app.label);
            let mut e = Line::new();
            let _ = write!(e, "opened {}", app.label);
            self.happened(e);
            self.damage_window(i);
            self.begin(i, AnimKind::Open);
            // Its dock icon hops as it opens, as on a Mac.
            let slow = if self.shift { 10 } else { 1 };
            self.bounce[k] = Some((self.frame_now, BOUNCE_MS * self.per_ms * slow, 0));
        } else if self.apps[i].minimized {
            return self.restore(i);
        }
        self.set_focus(i);
    }

    /// A minimised window comes back from its thumbnail (docs/u3-design.md).
    fn restore(&mut self, i: usize) {
        self.damage_dock();
        // The genie starts from where its thumbnail is now.
        self.begin(i, AnimKind::Restore);
        self.minis.retain(|&w| w != i);
        self.damage_dock();
        self.apps[i].minimized = false;
        zoo_rt::log!("PeacockHandler: showed {}", self.apps[i].label);
        self.damage_window(i);
        self.set_focus(i);
    }

    /// The right button went down: a context menu for a dock icon, or
    /// none (an open one closes).
    fn right_press(&mut self) {
        if self.menu.is_some() {
            self.close_menu();
            return;
        }
        let Some(DockHit::App(k)) = self.dock_hit() else {
            // On a window's content line: its application decides.
            if let Some(i) = self.window_at(self.px, self.py) {
                let (_, cy, _, _) = self.content_rect(i);
                if self.py < cy || self.on_sidebar(i) || !self.apps[i].running {
                    return;
                }
                self.set_focus(i);
                let mut l = Line::new();
                match self.line_at(i).filter(|&k| k < self.apps[i].count) {
                    Some(line) => {
                        let _ = write!(l, "rclick:{}", line);
                    }
                    None => {
                        let _ = l.write_str("rclick:none");
                    }
                }
                let _ = zoo_rt::send(self.apps[i].chan, l.as_str().as_bytes());
            }
            return;
        };
        let running = self.program_running(k);
        let mut items = [MenuAction::Line; MENU_ITEMS];
        let mut n = 0;
        let mut add = |a: MenuAction| {
            items[n] = a;
            n += 1;
        };
        add(if running {
            MenuAction::NewWindow
        } else {
            MenuAction::Open
        });
        add(MenuAction::Line);
        add(if self.pinned[k] {
            MenuAction::Remove
        } else {
            MenuAction::Keep
        });
        if running {
            add(MenuAction::Quit);
        }
        let face = self.face(FaceId::Regular13);
        let s = self.screen.s;
        let w = items[..n]
            .iter()
            .map(|a| face.width(a.label()).div_ceil(s))
            .max()
            .unwrap_or(0)
            .max(120)
            + 40;
        let (ix, iy) = self.dock_pos(k).unwrap_or((self.px, self.py));
        let mut m = Menu {
            owner: None,
            labels: [const { Text::new() }; MENU_ITEMS],
            prog: k,
            x: 0,
            y: 0,
            w,
            items,
            n,
            hover: None,
            bar: None,
        };
        let h = m.height();
        m.x = (ix + DOCK_ICON / 2)
            .saturating_sub(w / 2)
            .clamp(8, self.lw.saturating_sub(w + 8));
        m.y = iy.saturating_sub(10 + h);
        let mut names = Line::new();
        for (j, a) in items[..n]
            .iter()
            .filter(|a| **a != MenuAction::Line)
            .enumerate()
        {
            let _ = write!(names, "{}{}", if j > 0 { ", " } else { "" }, a.label());
        }
        zoo_rt::log!("PeacockHandler: menu {}: {}", CATALOG[k].0, names.as_str());
        self.hover = None;
        self.menu = Some(m);
        self.damage_menu();
        self.damage_dock();
    }

    /// The menu's area, with its shadow.
    fn damage_menu(&mut self) {
        if let Some(m) = &self.menu {
            let (x, y, w, h) = (m.x, m.y, m.w, m.height());
            self.damage(
                x.saturating_sub(MARGIN),
                y.saturating_sub(MARGIN),
                w + 2 * MARGIN,
                h + 2 * MARGIN,
            );
        }
    }

    fn close_menu(&mut self) {
        self.damage_menu();
        self.menu = None;
    }

    /// A left click while a menu is open: its item under the pointer, if
    /// any, is done; the menu closes either way.
    fn menu_click(&mut self) {
        let Some(m) = &self.menu else {
            return;
        };
        // On the menu bar: another title opens its menu; the open one's
        // title closes it.
        if let Some(t) = self.bar_title_at() {
            if m.bar == Some(t) {
                self.close_menu();
            } else {
                self.open_bar_menu(t);
            }
            return;
        }
        let pick = m.item_at(self.px, self.py);
        let choice = pick.map(|k| m.items[k]);
        let mut name = Line::new();
        if let Some(k) = pick {
            let _ = name.write_str(m.label(k));
        }
        let (prog, owner) = (m.prog, m.owner);
        self.close_menu();
        let Some(a) = choice else {
            return;
        };
        zoo_rt::log!("PeacockHandler: menu chose {}", name.as_str());
        if let (MenuAction::App(k), Some(i)) = (a, owner) {
            let mut l = Line::new();
            let _ = write!(l, "menu-choose:{}", k);
            let _ = zoo_rt::send(self.apps[i].chan, l.as_str().as_bytes());
            return;
        }
        let label = CATALOG[prog].0;
        match a {
            MenuAction::NewWindow => {
                self.new_window_of(prog);
            }
            MenuAction::Open => self.dock_click(prog),
            MenuAction::Remove => {
                self.damage_dock();
                self.pinned[prog] = false;
                zoo_rt::log!("PeacockHandler: removed {} from the dock", label);
                self.save_dock();
            }
            MenuAction::Keep => self.keep_in_dock(prog),
            MenuAction::Quit => {
                for i in 0..WINDOWS {
                    if self.apps[i].prog == prog && !self.apps[i].closed {
                        self.close_app(i);
                    }
                }
                zoo_rt::log!("PeacockHandler: quit {}", label);
            }
            MenuAction::Close => {
                let i = self.focus;
                if self.apps[i].visible() {
                    self.close_app(i);
                }
            }
            MenuAction::Minimize => {
                let i = self.focus;
                if self.apps[i].visible() {
                    self.minimize(i);
                }
            }
            MenuAction::Zoom => {
                let i = self.focus;
                if self.apps[i].visible() {
                    self.toggle_zoom(i);
                }
            }
            MenuAction::Focus(i) => {
                if self.apps[i].minimized {
                    self.restore(i);
                } else {
                    self.set_focus(i);
                }
                zoo_rt::log!("PeacockHandler: brought {} forward", self.apps[i].label);
            }
            MenuAction::Appearance => self.change_appearance(!self.dark),
            MenuAction::About => self.notify("About ZooOS", ABOUT),
            MenuAction::Shortcuts => self.notify("Keyboard Shortcuts", SHORTCUTS),
            MenuAction::Line | MenuAction::App(_) | MenuAction::Disabled => {}
        }
    }

    fn keep_in_dock(&mut self, prog: usize) {
        self.damage_dock();
        self.pinned[prog] = true;
        zoo_rt::log!("PeacockHandler: kept {} in the dock", CATALOG[prog].0);
        self.save_dock();
    }

    /// The dock's kept applications, saved in `.dock` in the home folder
    /// (docs/u3-design.md): their program names, comma-separated.
    fn save_dock(&self) {
        let mut l = Line::new();
        let mut n = 0;
        for (k, c) in CATALOG.iter().enumerate() {
            if self.pinned[k] {
                let _ = write!(l, "{}{}", if n > 0 { "," } else { "" }, c.1);
                n += 1;
            }
        }
        let _ = zoo_rt::file_create(self.home, DOCK_FILE);
        let saved = zoo_rt::file_truncate(self.home, DOCK_FILE, 0)
            .and_then(|()| zoo_rt::file_write(self.home, DOCK_FILE, 0, l.as_str().as_bytes()));
        match saved {
            Ok(_) => zoo_rt::log!("PeacockHandler: dock saved ({} kept)", n),
            Err(e) => zoo_rt::log!("PeacockHandler: cannot save the dock: {}", e.name()),
        }
    }

    /// The kept applications from `.dock`, if it exists.
    fn load_dock(&mut self) {
        let mut buf = [0u8; 256];
        let Ok(n) = zoo_rt::file_read(self.home, DOCK_FILE, 0, &mut buf) else {
            return;
        };
        self.pinned = [false; PROGRAMS];
        for name in buf[..n].split(|&b| b == b',') {
            if let Some(k) = program_index(name) {
                self.pinned[k] = true;
            }
        }
        let kept = self.pinned.iter().filter(|&&p| p).count();
        zoo_rt::log!("PeacockHandler: dock loaded ({} kept)", kept);
    }

    /// Keeps `e` among the last six things that happened (for Bella).
    fn happened(&mut self, e: Line) {
        if self.recent.len() == 6 {
            self.recent.pop_front();
        }
        self.recent.push_back(e);
    }

    /// Something an application asked for (docs/u3-design.md).
    fn app_request(&mut self, i: usize, req: Request) {
        match req {
            Request::DockAdd(prog) => self.keep_in_dock(prog),
            Request::Context => {
                let mut l = Line::new();
                let _ = l.write_str("context:");
                let f = &self.apps[self.focus];
                // A window's title only where it says more than its app's
                // name (Notes' title is its file), to keep this short.
                let name = |a: &App, l: &mut Line| {
                    let _ = l.write_str(a.label);
                    if a.title.as_str() != a.label && !a.title.as_str().is_empty() {
                        let _ = write!(l, " showing {}", a.title.as_str());
                    }
                    if a.minimized {
                        let _ = l.write_str(", minimised");
                    }
                };
                if !f.closed {
                    let _ = l.write_str("focused: ");
                    name(f, &mut l);
                    let _ = l.write_str("; ");
                }
                let _ = l.write_str("open: ");
                let mut first = true;
                for a in self.apps.iter().filter(|a| !a.closed) {
                    if !first {
                        let _ = l.write_str(", ");
                    }
                    name(a, &mut l);
                    first = false;
                }
                let _ = l.write_str("; recently: ");
                for (k, e) in self.recent.iter().enumerate() {
                    let _ = write!(l, "{}{}", if k == 0 { "" } else { ", " }, e.as_str());
                }
                let _ = zoo_rt::send(self.apps[i].chan, l.as_str().as_bytes());
                zoo_rt::log!("PeacockHandler: context for {}", self.apps[i].label);
            }
            Request::Launch(prog) => self.dock_click(prog),
            Request::Do(prog, line) => {
                // Opened, shown or raised, as a dock click does; then the
                // front window of it gets the action.
                self.dock_click(prog);
                let label = CATALOG[prog].0;
                match self.front_window_of(prog, App::visible) {
                    Some(w) if w != i => {
                        self.apps[w].asked_by = Some(i);
                        let _ = zoo_rt::send(self.apps[w].chan, line.as_str().as_bytes());
                        zoo_rt::log!(
                            "PeacockHandler: {} asks {}: {}",
                            self.apps[i].label,
                            label,
                            line.as_str()
                        );
                        let mut e = Line::new();
                        let _ = write!(e, "asked {} to {}", label, line.as_str());
                        self.happened(e);
                    }
                    _ => {
                        let mut l = Line::new();
                        let _ = write!(l, "window-done:Error: {} could not be opened.", label);
                        let _ = zoo_rt::send(self.apps[i].chan, l.as_str().as_bytes());
                    }
                }
            }
            Request::Did(said) => {
                if let Some(asker) = self.apps[i].asked_by.take()
                    && self.apps[asker].running
                {
                    let mut l = Line::new();
                    let _ = write!(l, "window-done:{}", said.as_str());
                    let _ = zoo_rt::send(self.apps[asker].chan, l.as_str().as_bytes());
                    zoo_rt::log!(
                        "PeacockHandler: {} did: {}",
                        self.apps[i].label,
                        said.as_str()
                    );
                }
            }
            Request::BadDo => {
                let _ = zoo_rt::send(self.apps[i].chan, b"window-done:Error: no app can do that.");
                zoo_rt::log!("PeacockHandler: an action the manifest refuses");
            }
            Request::SplitFront => {
                let said = self.split_front(i);
                let mut l = Line::new();
                let _ = write!(l, "window-done:{}", said.as_str());
                let _ = zoo_rt::send(self.apps[i].chan, l.as_str().as_bytes());
            }
            Request::Window(action, prog, m, monitor) => {
                let said = self.window_action(action, prog, m, monitor);
                let mut l = Line::new();
                let _ = write!(l, "window-done:{}", said.as_str());
                let _ = zoo_rt::send(self.apps[i].chan, l.as_str().as_bytes());
            }
            Request::Open(prog, path) => {
                let w = if self.program_running(prog) {
                    self.new_window_of(prog)
                } else {
                    self.dock_click(prog);
                    (!self.apps[prog].closed).then_some(prog)
                };
                if let Some(w) = w {
                    let mut l = Line::new();
                    let _ = write!(l, "open:{}", path.as_str());
                    let _ = zoo_rt::send(self.apps[w].chan, l.as_str().as_bytes());
                    zoo_rt::log!(
                        "PeacockHandler: {} opens {}",
                        self.apps[w].label,
                        path.as_str()
                    );
                    let mut e = Line::new();
                    let _ = write!(e, "opened {} in {}", path.as_str(), self.apps[w].label);
                    self.happened(e);
                }
            }
            Request::Menu(labels, n) => {
                if n == 0 || i != self.focus {
                    return;
                }
                let s = self.screen.s;
                let face = self.face(FaceId::Regular13);
                let w = labels[..n]
                    .iter()
                    .map(|t| face.width(t.as_str()).div_ceil(s))
                    .max()
                    .unwrap_or(0)
                    .max(120)
                    + 40;
                let mut m = Menu {
                    owner: Some(i),
                    labels: *labels,
                    prog: self.apps[i].prog,
                    x: 0,
                    y: 0,
                    w,
                    items: core::array::from_fn(MenuAction::App),
                    n,
                    hover: None,
                    bar: None,
                };
                let h = m.height();
                // Below and right of the pointer, kept on the screen.
                m.x = self.px.min(self.lw.saturating_sub(w + 4));
                m.y = (self.py + 2).min(self.lh.saturating_sub(h + 4));
                let mut names = Line::new();
                for k in 0..n {
                    let _ = write!(names, "{}{}", if k > 0 { ", " } else { "" }, m.label(k));
                }
                zoo_rt::log!(
                    "PeacockHandler: menu {}: {}",
                    self.apps[i].label,
                    names.as_str()
                );
                self.menu = Some(m);
                self.damage_menu();
            }
        }
    }

    /// A context menu: a rounded panel with a shadow, items in a column,
    /// the one under the pointer on a selection bar.
    fn draw_menu(&mut self) {
        let Some(m) = &self.menu else {
            return;
        };
        let t = self.theme();
        let (x, y, w, h) = (m.x, m.y, m.w, m.height());
        let (items, n, hover) = (m.items, m.n, m.hover);
        let labels: [Text; MENU_ITEMS] = core::array::from_fn(|k| {
            let mut t = Text::new();
            if k < n {
                t.set(m.label(k).as_bytes());
            }
            t
        });
        if !self.screen.visible(
            x.saturating_sub(MARGIN),
            y.saturating_sub(MARGIN),
            w + 2 * MARGIN,
            h + 2 * MARGIN,
        ) {
            return;
        }
        self.screen
            .shadow((x, y, w, h), t.shadow_blurred, &self.ramp);
        self.screen
            .round_rect(x, y, w, h, 8, t.tooltip, 255, [true; 4]);
        self.screen.round_outline(x, y, w, h, 8, t.outline);
        let face = self.face(FaceId::Regular13);
        let seps: [bool; MENU_ITEMS] = core::array::from_fn(|k| items[k] == MenuAction::Line);
        for (k, item) in items.iter().enumerate().take(n) {
            let top = zoo_abi::ui::menu_item_top(y, &seps[..n], k);
            if *item == MenuAction::Line {
                self.screen.fill(x + 10, top + 4, w - 20, 1, t.separator);
                continue;
            }
            let on = hover == Some(k);
            if on {
                self.screen.round_rect(
                    x + 5,
                    top,
                    w - 10,
                    MENU_ITEM_H,
                    5,
                    t.select,
                    255,
                    [true; 4],
                );
            }
            let c = if on {
                t.select_text
            } else if *item == MenuAction::Disabled {
                t.text_dim
            } else {
                t.tooltip_text
            };
            let (label, keys) = labels[k]
                .as_str()
                .split_once('\t')
                .unwrap_or((labels[k].as_str(), ""));
            // Windows in the Window menu: a column for the check mark on
            // the focused one.
            let mut lx = x + 16;
            if let MenuAction::Focus(i) = *item {
                if i == self.focus && self.apps[i].visible() {
                    self.screen
                        .text(&face, lx, top + 17, "\u{2713}", c, x + w - 8);
                }
                lx += 16;
            }
            self.screen.text(&face, lx, top + 17, label, c, x + w - 8);
            if !keys.is_empty() {
                let kw = face.width(keys).div_ceil(self.screen.s);
                let kc = if on { t.select_text } else { t.text_dim };
                self.screen
                    .text(&face, x + w - 14 - kw, top + 17, keys, kc, x + w - 8);
            }
        }
    }

    /// Each menu-bar title's span, logical x (docs/u4a-design.md): the
    /// ZooOS mark, the application name, then `MENUS`, as drawn.
    fn bar_titles(&self) -> [(usize, usize); BAR_TITLES] {
        let s = self.screen.s;
        let bold = self.face(FaceId::SemiBold13);
        let regular = self.face(FaceId::Regular13);
        let mut out = [(0, 0); BAR_TITLES];
        out[0] = (PAW_AT.0 - 4, PAW_AT.0 + PAW_SIZE + 4);
        let name = self.bar_name();
        let mut x = 40;
        let end = x + bold.width(name).div_ceil(s);
        out[1] = (x - 8, end + 8);
        x = end + 22;
        for (k, m) in MENUS.iter().enumerate() {
            let end = x + regular.width(m).div_ceil(s);
            out[2 + k] = (x - 8, end + 8);
            x = end + 20;
        }
        out
    }

    /// The application name in the menu bar.
    fn bar_name(&self) -> &'static str {
        let app = &self.apps[self.focus];
        if app.visible() { app.label } else { "Finder" }
    }

    /// The menu-bar title under the pointer, if any.
    fn bar_title_at(&self) -> Option<usize> {
        if self.py >= MENU_H {
            return None;
        }
        self.bar_titles()
            .iter()
            .position(|&(a, b)| self.px >= a && self.px < b)
    }

    /// Opens menu-bar menu `t` (docs/u4a-design.md).
    fn open_bar_menu(&mut self, t: usize) {
        if self.menu.is_some() {
            self.close_menu();
        }
        let mut items = [MenuAction::Line; MENU_ITEMS];
        let mut labels: [Text; MENU_ITEMS] = core::array::from_fn(|_| Text::new());
        let mut n = 0;
        let mut add = |a: MenuAction, label: &str| {
            if n < MENU_ITEMS {
                items[n] = a;
                labels[n].set(label.as_bytes());
                n += 1;
            }
        };
        let focus = self.focus;
        let app = self.bar_name();
        let has_window = self.apps[focus].visible();
        let dark = self.dark;
        match t {
            0 => {
                add(MenuAction::About, "");
                add(MenuAction::Line, "");
                add(
                    MenuAction::Appearance,
                    if dark {
                        "Light Appearance"
                    } else {
                        "Dark Appearance"
                    },
                );
            }
            1 => {
                let mut l = Line::new();
                let _ = write!(l, "Quit {}", app);
                add(
                    if has_window {
                        MenuAction::Quit
                    } else {
                        MenuAction::Disabled
                    },
                    l.as_str(),
                );
            }
            2 => {
                add(
                    if has_window {
                        MenuAction::NewWindow
                    } else {
                        MenuAction::Disabled
                    },
                    "New Window\tCtrl+N",
                );
                add(
                    if has_window {
                        MenuAction::Close
                    } else {
                        MenuAction::Disabled
                    },
                    "",
                );
            }
            3 => {
                for label in ["Undo", "Cut", "Copy", "Paste"] {
                    add(MenuAction::Disabled, label);
                }
            }
            4 => add(
                MenuAction::Appearance,
                if dark {
                    "Light Appearance"
                } else {
                    "Dark Appearance"
                },
            ),
            5 => {
                add(
                    if has_window {
                        MenuAction::Minimize
                    } else {
                        MenuAction::Disabled
                    },
                    "",
                );
                add(
                    if has_window {
                        MenuAction::Zoom
                    } else {
                        MenuAction::Disabled
                    },
                    "",
                );
                add(MenuAction::Line, "");
                for (i, a) in self.apps.iter().enumerate() {
                    if a.closed || !a.running {
                        continue;
                    }
                    // The check mark is drawn in its own column.
                    let mut l = Line::new();
                    let _ = l.write_str(a.title.as_str());
                    if a.minimized {
                        let _ = l.write_str(" (minimized)");
                    }
                    add(MenuAction::Focus(i), l.as_str());
                }
            }
            _ => add(MenuAction::Shortcuts, ""),
        }
        if n > 0 && items[n - 1] == MenuAction::Line {
            n -= 1;
        }
        // Labels for the actions that name themselves.
        for k in 0..n {
            if labels[k].len == 0 {
                labels[k].set(items[k].label().as_bytes());
            }
        }
        let face = self.face(FaceId::Regular13);
        let sc = self.screen.s;
        let w = labels[..n]
            .iter()
            .map(|l| {
                let (a, b) = l.as_str().split_once('\t').unwrap_or((l.as_str(), ""));
                face.width(a).div_ceil(sc) + face.width(b).div_ceil(sc) + 30
            })
            .max()
            .unwrap_or(0)
            .max(180)
            + 30;
        let (tx, _) = self.bar_titles()[t];
        let m = Menu {
            owner: None,
            labels,
            prog: self.apps[focus].prog,
            x: tx.min(self.lw.saturating_sub(w + 4)),
            y: MENU_H + 2,
            w,
            items,
            n,
            hover: None,
            bar: Some(t),
        };
        let mut names = Line::new();
        for k in 0..m.n {
            if m.items[k] != MenuAction::Line {
                // The log is ASCII: the check mark is left out.
                let name = m.label(k).split('\t').next().unwrap_or("");
                let name = name.trim().trim_start_matches('\u{2713}').trim();
                let _ = write!(
                    names,
                    "{}{}",
                    if names.as_str().is_empty() { "" } else { ", " },
                    name
                );
            }
        }
        let title = match t {
            0 => "ZooOS",
            1 => app,
            k => MENUS[k - 2],
        };
        zoo_rt::log!("PeacockHandler: menu bar {}: {}", title, names.as_str());
        self.menu = Some(m);
        self.damage_menu();
        self.damage_menu_bar();
    }

    /// Ctrl+N: another window of the focused window's application.
    fn new_window(&mut self) {
        let prog = self.apps[self.focus].prog;
        self.new_window_of(prog);
    }

    /// Another window of application `prog`, in a free slot, cascaded
    /// from its frontmost window (docs/u3-design.md); its first window if
    /// none is open.
    fn new_window_of(&mut self, prog: usize) -> Option<usize> {
        if !self.program_running(prog) {
            self.dock_click(prog);
            return Some(prog);
        }
        let base = self
            .order
            .iter()
            .rev()
            .copied()
            .find(|&i| self.apps[i].prog == prog && self.apps[i].visible())
            .unwrap_or(prog);
        let Some(i) = (PROGRAMS..WINDOWS).find(|&i| self.apps[i].closed && self.anims[i].is_none())
        else {
            zoo_rt::log!("PeacockHandler: no room for another window");
            return None;
        };
        self.apps[i] = App::new(prog);
        let (fx, fy, fw, fh) = self.wins[base];
        let x = (fx + 32).min(self.lw.saturating_sub(fw + 8));
        let y = (fy + 32)
            .min(self.lh.saturating_sub(fh + 8))
            .max(MENU_H + 8);
        self.wins[i] = (x, y, fw, fh);
        self.unzoomed[i] = None;
        if !self.apps[i].start(self.home) {
            zoo_rt::log!("PeacockHandler: cannot start {}", self.apps[i].program);
            self.apps[i].closed = true;
            return None;
        }
        zoo_rt::log!(
            "PeacockHandler: new window {} (window {})",
            self.apps[i].label,
            i
        );
        self.damage_window(i);
        self.damage_dock();
        self.begin(i, AnimKind::Open);
        self.set_focus(i);
        Some(i)
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
        app.minimized = false;
        zoo_rt::log!("PeacockHandler: closed {}", app.label);
        let mut e = Line::new();
        let _ = write!(e, "closed {}", app.label);
        self.happened(e);
        self.minis.retain(|&w| w != i);
        self.focus_another(i);
    }

    /// The two front windows other than the asking app's (`asker`), side by
    /// side, the front one on the left; with only one other window, it and
    /// the asker (docs/tiling-design.md).
    fn split_front(&mut self, asker: usize) -> Line {
        let mut pick: Vec<usize> = self
            .order
            .iter()
            .rev()
            .copied()
            .filter(|&w| w != asker && self.apps[w].visible() && self.apps[w].running)
            .take(2)
            .collect();
        if pick.len() < 2 && self.apps[asker].visible() {
            pick.push(asker);
        }
        let mut said = Line::new();
        if pick.len() < 2 {
            let _ = said.write_str("There are not two windows to split.");
            return said;
        }
        let on = self.monitor_of(pick[0]);
        for (k, &w) in pick.iter().enumerate() {
            let rect = self.column_on(on, k, 2);
            self.place(w, rect);
        }
        // The left one in front, with the keyboard.
        self.set_focus(pick[1]);
        self.set_focus(pick[0]);
        let _ = write!(
            said,
            "{} is on the left and {} on the right.",
            self.apps[pick[0]].label, self.apps[pick[1]].label
        );
        let mut e = Line::new();
        let _ = write!(
            e,
            "split {} and {}",
            self.apps[pick[0]].label, self.apps[pick[1]].label
        );
        self.happened(e);
        zoo_rt::log!("PeacockHandler: window action: {}", said.as_str());
        said
    }

    /// Application `prog`'s `m`th visible window from the front, a
    /// minimised one shown, or a new one opened when it has fewer
    /// ("two Files windows side by side"; docs/tiling-design.md).
    fn nth_window_of(&mut self, prog: usize, m: usize) -> Option<usize> {
        let mut shown: Vec<usize> = self
            .order
            .iter()
            .rev()
            .copied()
            .filter(|&w| self.apps[w].prog == prog && self.apps[w].visible())
            .collect();
        // Earlier columns raised theirs: the front ones are taken already.
        shown.truncate(shown.len().min(m + 1));
        if shown.len() > m {
            // The one not yet placed this time: the last raised are
            // in front, so the m-th is the oldest of the front m + 1.
            return shown.last().copied();
        }
        if let Some(w) = (0..WINDOWS)
            .find(|&w| self.apps[w].prog == prog && !self.apps[w].closed && self.apps[w].minimized)
        {
            self.restore(w);
            return Some(w);
        }
        let w = self.new_window_of(prog)?;
        self.set_focus(w);
        Some(w)
    }

    /// The frontmost window of application `prog` that `want` accepts.
    fn front_window_of(&self, prog: usize, want: impl Fn(&App) -> bool) -> Option<usize> {
        let of = |i: usize| self.apps[i].prog == prog && !self.apps[i].closed;
        self.order
            .iter()
            .rev()
            .copied()
            .find(|&i| of(i) && want(&self.apps[i]))
            .or_else(|| (0..WINDOWS).find(|&i| of(i) && want(&self.apps[i])))
    }

    /// Does `action` to application `prog`'s frontmost window, as the
    /// lights and the dock would; returns the sentence the asking
    /// application shows (docs/window-actions-design.md).
    fn window_action(
        &mut self,
        action: WinAction,
        prog: usize,
        m: usize,
        monitor: Option<usize>,
    ) -> Line {
        let label = CATALOG[prog].0;
        let mut said = Line::new();
        if let Some(n) = monitor
            && n >= self.monitors
            && n != NEXT_MONITOR
        {
            let _ = write!(
                said,
                "There is no monitor {}; this screen has {}.",
                n + 1,
                self.monitors
            );
            zoo_rt::log!(
                "PeacockHandler: window action for {}: {}",
                label,
                said.as_str()
            );
            return said;
        }
        match action {
            WinAction::Maximize | WinAction::Column(..) | WinAction::Move => {
                // Opened, shown or raised, as a dock click does.
                self.dock_click(prog);
                let target = if m == 0 {
                    self.front_window_of(prog, App::visible)
                } else {
                    self.nth_window_of(prog, m)
                };
                match target {
                    Some(w) => {
                        // The monitor asked for, else the window's own.
                        let on = match monitor {
                            Some(NEXT_MONITOR) => (self.monitor_of(w) + 1) % self.monitors,
                            Some(n) => n,
                            None => self.monitor_of(w),
                        };
                        let (rect, done, event) = match action {
                            WinAction::Column(k, n) => (
                                self.column_on(on, k, n),
                                side_name(k, n).0,
                                side_name(k, n).1,
                            ),
                            WinAction::Move => {
                                (self.moved_to(w, on), "is on monitor", "moved to monitor")
                            }
                            _ => (self.zoomed_on(on), "fills the screen", "maximized"),
                        };
                        if action == WinAction::Move {
                            self.move_to(w, rect);
                        } else {
                            self.place(w, rect);
                        }
                        self.set_focus(w);
                        if action == WinAction::Move {
                            let _ = write!(said, "{} {} {}.", label, done, on + 1);
                        } else if monitor.is_some() && self.monitors > 1 {
                            let _ = write!(said, "{} {} of monitor {}.", label, done, on + 1);
                        } else {
                            let _ = write!(said, "{} {}.", label, done);
                        }
                        let mut e = Line::new();
                        let _ = write!(e, "{} {}", event, label);
                        self.happened(e);
                    }
                    None => {
                        let _ = write!(said, "Error: {} could not be opened.", label);
                    }
                }
            }
            WinAction::Minimize => match self.front_window_of(prog, App::visible) {
                Some(w) => {
                    self.minimize(w);
                    let _ = write!(said, "Minimized {}; it is in the dock.", label);
                    let mut e = Line::new();
                    let _ = write!(e, "minimized {}", label);
                    self.happened(e);
                }
                None => {
                    let _ = write!(said, "{} has no window showing.", label);
                }
            },
            WinAction::Close => match self.front_window_of(prog, |a| a.running) {
                Some(w) => {
                    self.close_app(w);
                    let _ = write!(said, "Closed {}.", label);
                }
                None => {
                    let _ = write!(said, "{} is not open.", label);
                }
            },
        }
        zoo_rt::log!(
            "PeacockHandler: window action for {}: {}",
            label,
            said.as_str()
        );
        said
    }

    /// The yellow light: hides the window; the dock shows it again.
    fn minimize(&mut self, i: usize) {
        self.damage_dock();
        self.minis.push(i);
        self.damage_dock();
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
        let right = e.buttons & 2 != 0;
        if right && !self.rheld {
            self.right_press();
        }
        self.rheld = right;
        if let Some(Menu {
            bar: Some(open), ..
        }) = self.menu
            && let Some(t) = self.bar_title_at()
            && t != open
        {
            self.open_bar_menu(t);
        }
        if let Some(m) = &self.menu {
            let hover = m.item_at(self.px, self.py);
            if hover != m.hover {
                self.damage_menu();
                if let Some(m) = &mut self.menu {
                    m.hover = hover;
                }
            }
        }
        if down && !self.held && self.menu.is_some() {
            self.menu_click();
        } else if down && !self.held {
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
        let hover = self.dock_hit();
        if hover != self.hover {
            self.hover = hover;
            self.damage_dock();
        }
        true
    }

    /// The left button went down.
    fn press(&mut self) {
        match self.dock_hit() {
            Some(DockHit::App(k)) => return self.dock_click(k),
            Some(DockHit::Window(i)) => return self.restore(i),
            None => {}
        }
        if let Some(t) = self.bar_title_at() {
            return self.open_bar_menu(t);
        }
        if self.py < MENU_H {
            let mut clock = Line::new();
            write_clock(&mut clock);
            let cw = self
                .face(FaceId::Regular13)
                .width(clock.as_str())
                .div_ceil(self.screen.s);
            let (sx, sy) = zoo_abi::ui::appearance_switch(self.mw, cw);
            if self.px.abs_diff(sx) <= 9 && self.py.abs_diff(sy) <= 9 {
                self.change_appearance(!self.dark);
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
        } else {
            self.content_click(i);
        }
    }

    /// The content line of window `i` under the pointer.
    fn line_at(&self, i: usize) -> Option<usize> {
        (0..MAX_LINES).find(|&k| {
            let top = zoo_abi::ui::line_top(self.wins[i], k);
            self.py >= top && self.py < top + LINE_H
        })
    }

    /// Whether the pointer is over window `i`'s sidebar.
    fn on_sidebar(&self, i: usize) -> bool {
        self.apps[i].sides > 0 && self.px < self.wins[i].0 + zoo_abi::ui::SIDEBAR_W
    }

    /// A click in window `i`'s content goes to its application
    /// (docs/u3f-design.md): `sclick:<k>` on a sidebar item, `click:<k>`
    /// on a line, `dclick:<k>` for the second click of a double-click.
    fn content_click(&mut self, i: usize) {
        let Some(k) = self.line_at(i) else {
            return;
        };
        let mut l = Line::new();
        if self.on_sidebar(i) {
            if k >= self.apps[i].sides {
                return;
            }
            let _ = write!(l, "sclick:{}", k);
        } else {
            if k >= self.apps[i].count {
                return;
            }
            let now = zoo_rt::ticks();
            let (t, w, line) = self.last_line;
            let double = w == i && line == k && now.saturating_sub(t) <= DOUBLE_CLICK;
            let _ = write!(l, "{}:{}", if double { "dclick" } else { "click" }, k);
            self.last_line = if double { (0, WINDOWS, 0) } else { (now, i, k) };
        }
        let _ = zoo_rt::send(self.apps[i].chan, l.as_str().as_bytes());
    }

    /// Redraws the damaged areas and shows them; records the frame's cost.
    fn render(&mut self) {
        if self.always_full {
            self.full = true;
        }
        self.frame_now = cycles();
        self.step_anims();
        self.update_dock(self.frame_now);
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
        for i in 0..WINDOWS {
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
        for k in 0..WINDOWS {
            let i = self.order[k];
            if let Some(a) = &self.anims[i] {
                let l = a.snapshot.as_ref().unwrap_or(&self.layers[i]);
                let (sw, clip, s) = (self.screen.w, self.screen.clip, self.screen.s);
                if let Some(g) = a.genie(self.frame_now) {
                    // From the window to the icon either way round.
                    let (win, icon) = match a.kind {
                        AnimKind::Restore => (a.to, a.from),
                        _ => (a.from, a.to),
                    };
                    composite_genie(self.screen.back, sw, clip, s, l, a.size, win, icon, g);
                } else {
                    let (rect, alpha, _) = a.at(self.frame_now);
                    composite_scaled(self.screen.back, sw, clip, s, l, a.size, rect, alpha);
                }
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
        self.draw_menu();
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
        // The dock icon (the genie's funnel ends in it).
        let icon: RectF = {
            let (ix, iy) = self
                .mini_pos(i)
                .or_else(|| self.dock_pos(self.apps[i].prog))
                .unwrap_or(zoo_abi::ui::dock_icon(self.mw, self.lh, 1, 0));
            let d = DOCK_ICON as f32;
            (ix as f32 + d * 0.1, iy as f32 + d * 0.1, d * 0.8, d * 0.8)
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
            .map_or(layer_bounds(win, (w, h)), |a| a.last);
        self.anims[i] = Some(Anim {
            kind,
            start: cycles(),
            dur: kind.millis() * self.per_ms * if self.shift { 10 } else { 1 },
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
        for i in 0..WINDOWS {
            let Some(a) = self.anims[i].as_mut() else {
                continue;
            };
            let (_, _, done) = a.at(now);
            let (last, kind, start) = (a.last, a.kind, a.start);
            let bounds = a.bounds(now);
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
            || self.dock_slide.is_some()
            || self.dock_hide_at.is_some()
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
        let w = self.mw;
        if self.screen.clip.1 >= MENU_H * self.screen.s {
            return;
        }
        self.screen.fill(0, 0, w, MENU_H, zoo_abi::ui::bar_color(t));
        let (bold, regular) = (self.face(FaceId::SemiBold13), self.face(FaceId::Regular13));
        let base = 18;
        self.draw_paw(PAW_AT.0, PAW_AT.1, t.bar_text);
        let app = &self.apps[self.focus];
        let name = if app.visible() { app.label } else { "Finder" };
        // The open menu's title: a rounded highlight behind it.
        if let Some(Menu { bar: Some(k), .. }) = self.menu {
            let (a, b) = self.bar_titles()[k];
            self.screen
                .round_rect(a, 3, b - a, MENU_H - 6, 5, t.bar_text, 40, [true; 4]);
        }
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

    /// The ZooOS mark (the soft paw) with its box's top-left at logical
    /// (x, y), from the mask made at start-up.
    fn draw_paw(&mut self, x: usize, y: usize, c: u32) {
        let s = self.screen.s;
        let side = PAW_SIZE * s;
        for r in 0..side {
            for k in 0..side {
                let a = self.paw[r * side + k];
                if a != 0 {
                    self.screen.blend_dev(x * s + k, y * s + r, c, a);
                }
            }
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
        // The sidebar (docs/u3f-design.md).
        let sides = app.sides;
        let side_w = if sides > 0 { zoo_abi::ui::SIDEBAR_W } else { 0 };
        if sides > 0 {
            let top = y + TITLE_H;
            let sh = h.saturating_sub(TITLE_H + 1);
            self.screen.round_rect(
                x + 1,
                top,
                side_w,
                sh,
                RADIUS,
                t.sidebar,
                255,
                [false, false, true, false],
            );
            self.screen.fill(x + side_w, top, 1, sh, t.separator);
            for k in 0..sides {
                let top = zoo_abi::ui::line_top(self.wins[i], k);
                if top + LINE_H > y + h - 4 {
                    break;
                }
                if self.apps[i].side_selected == Some(k) {
                    self.screen.round_rect(
                        x + 9,
                        top,
                        side_w - 16,
                        LINE_H,
                        5,
                        t.sidebar_select,
                        255,
                        all,
                    );
                }
                let item = self.apps[i].side[k].clone();
                self.rich_text(
                    x + TEXT_X,
                    top + BASELINE,
                    item.as_str(),
                    t.text,
                    x + side_w - 8,
                    false,
                );
            }
        }
        let cx = x + side_w;
        for li in 0..self.apps[i].count {
            let top = zoo_abi::ui::line_top(self.wins[i], li);
            if top + LINE_H > y + h - 4 {
                break;
            }
            let selected = self.apps[i].selected == Some(li);
            let fg = if selected && focused {
                t.select_text
            } else {
                t.text
            };
            if selected {
                let bg = if focused { t.select } else { t.select_blurred };
                self.screen
                    .round_rect(cx + 8, top, w - side_w - 16, LINE_H, 5, bg, 255, all);
            }
            let line = self.apps[i].lines[li].clone();
            self.rich_text(
                cx + TEXT_X,
                top + BASELINE,
                line.as_str(),
                fg,
                right,
                selected && focused,
            );
        }
    }

    /// A content line with markup (docs/u3f-design.md): an icon at the
    /// start, colour roles, a caret, a right-aligned dim tail. `plain`
    /// (a selected line) draws all of it in `fg`.
    fn rich_text(&mut self, x: usize, base: usize, text: &str, fg: u32, max_x: usize, plain: bool) {
        let t = self.theme();
        let face = self.face(FaceId::Regular13);
        let s = self.screen.s;
        let b = text.as_bytes();
        let mut x = x;
        let mut k = 0;
        if b.len() >= 2 && b[0] == 2 {
            self.icon_small(b[1], x, base + 4 - 16, plain);
            x += 22;
            k = 2;
        }
        let mut colour = fg;
        let mut start = k;
        loop {
            let end = k == b.len();
            if !end && !matches!(b[k], 1 | 3 | b'\t') {
                k += 1;
                continue;
            }
            if start < k {
                x = self
                    .screen
                    .text(&face, x, base, &text[start..k], colour, max_x);
            }
            if end {
                return;
            }
            match b[k] {
                1 => {
                    if let Some(&r) = b.get(k + 1) {
                        if !plain {
                            colour = role_colour(t, r, fg);
                        }
                        k += 1;
                    }
                }
                3 => {
                    self.screen
                        .fill(x + 1, base - 13, 2, 17, if plain { fg } else { t.accent });
                    // Room for the bar, so it never covers a letter.
                    x += 4;
                }
                _ => {
                    let rest = &text[k + 1..];
                    let w = face.width(rest).div_ceil(s);
                    let c = if plain { fg } else { t.text_dim };
                    let at = max_x.saturating_sub(w).max(x + 12);
                    self.screen.text(&face, at, base, rest, c, max_x);
                    return;
                }
            }
            k += 1;
            start = k;
        }
    }

    /// A 16-px icon (docs/u3f-design.md) with its top-left at (x, y).
    /// (Shapes read as coordinate bounds, so not as ranges.)
    #[allow(clippy::manual_range_contains)]
    fn icon_small(&mut self, kind: u8, x: usize, y: usize, on_select: bool) {
        let t = self.theme();
        let s = &mut self.screen;
        if !s.visible(x, y, 16, 16) {
            return;
        }
        let sym = if on_select { 0xFFFFFF } else { t.accent };
        match kind {
            b'F' => {
                // A folder: the back with its tab, then the lighter front.
                s.shape(x, y, 16, 16, 0x4A9FE8, |u, v| {
                    rr(u, v, (1.0, 2.5, 7.5, 7.0), 1.5) || rr(u, v, (1.0, 4.0, 15.0, 14.5), 1.8)
                });
                s.shape(x, y, 16, 16, 0x7DC2F7, |u, v| {
                    rr(u, v, (1.0, 6.0, 15.0, 14.5), 1.6)
                });
                s.shape(x, y, 16, 16, 0xA9D8FB, |u, v| {
                    rr(u, v, (1.0, 6.0, 15.0, 7.0), 0.5)
                });
            }
            b'D' | b'C' => {
                // A page with a folded corner and lines of text.
                let page = |u: f32, v: f32| rr(u, v, (2.5, 0.5, 13.5, 15.5), 1.5) && u - v < 9.5;
                s.shape(x, y, 16, 16, 0xA9A9AF, page);
                s.shape(x, y, 16, 16, 0xFFFFFF, |u, v| {
                    rr(u, v, (3.3, 1.3, 12.7, 14.7), 1.0) && u - v < 8.7
                });
                s.shape(x, y, 16, 16, 0xD4D4D9, |u, v| {
                    u >= 9.6 && v <= 4.4 && v >= u - 9.6 && u <= 13.0
                });
                let lines: [(u32, f32); 4] = if kind == b'C' {
                    [
                        (0xAD3DA4, 8.0),
                        (0x3A7BD5, 10.5),
                        (0x4C9A2A, 7.0),
                        (0x3A7BD5, 9.0),
                    ]
                } else {
                    [(0xC5C5CB, 10.5); 4]
                };
                for (j, &(c, end)) in lines.iter().enumerate() {
                    let v0 = 5.5 + 2.3 * j as f32;
                    s.shape(x, y, 16, 16, c, |u, v| {
                        u >= 5.0 && u <= end && v >= v0 && v <= v0 + 1.1
                    });
                }
            }
            b'H' => s.shape(x, y, 16, 16, sym, |u, v| {
                // A house: the roof, the walls, a door.
                let roof = v >= 1.5
                    && v <= 8.5
                    && (u - 8.0).abs() <= (v - 1.5) * 1.05 + 0.6
                    && (u - 8.0).abs() >= (v - 1.5) * 1.05 - 1.9;
                let walls = rr(u, v, (3.2, 7.0, 12.8, 14.5), 1.0)
                    && !(rr(u, v, (4.6, 8.0, 11.4, 13.2), 0.6)
                        && !(u >= 6.6 && u <= 9.4 && v >= 9.8));
                roof || walls
            }),
            b'O' => s.shape(x, y, 16, 16, sym, |u, v| {
                // A page outline with two lines.
                (rr(u, v, (3.0, 1.0, 13.0, 15.0), 1.8) && !rr(u, v, (4.4, 2.4, 11.6, 13.6), 0.8))
                    || (u >= 6.0
                        && u <= 10.0
                        && ((v >= 6.0 && v <= 7.2) || (v >= 9.0 && v <= 10.2)))
            }),
            b'M' => s.shape(x, y, 16, 16, sym, |u, v| {
                // Two beamed notes.
                circle(u, v, 5.0, 12.5, 2.4)
                    || circle(u, v, 12.0, 11.0, 2.4)
                    || (u >= 6.2 && u <= 7.4 && v >= 4.0 && v <= 12.5)
                    || (u >= 13.2 && u <= 14.4 && v >= 2.5 && v <= 11.0)
                    || (u >= 6.2 && u <= 14.4 && {
                        let top = 4.0 - (u - 6.2) * (1.5 / 8.2);
                        v >= top - 0.2 && v <= top + 2.0
                    })
            }),
            b'P' => s.shape(x, y, 16, 16, sym, |u, v| {
                // A photo: a frame, a hill and the sun.
                let frame =
                    rr(u, v, (1.0, 2.5, 15.0, 13.5), 2.2) && !rr(u, v, (2.4, 3.9, 13.6, 12.1), 1.0);
                let hill = v <= 12.1 && v >= 6.0 + (u - 6.5).abs() * 1.1 && u >= 2.4;
                frame || hill || circle(u, v, 11.0, 6.6, 1.5)
            }),
            b'T' => s.shape(x, y, 16, 16, sym, |u, v| {
                // A bin: the lid and its handle, the body with two grooves.
                let lid =
                    rr(u, v, (2.0, 3.0, 14.0, 4.6), 0.6) || rr(u, v, (6.0, 1.2, 10.0, 3.2), 0.6);
                let body =
                    rr(u, v, (3.6, 5.6, 12.4, 15.0), 1.4) && !rr(u, v, (4.9, 5.6, 11.1, 13.7), 0.6);
                let grooves = (6.9..=7.7).contains(&u) || (8.3..=9.1).contains(&u);
                lid || body || (grooves && (7.0..=12.5).contains(&v))
            }),
            b'A' => s.shape(x, y, 16, 16, sym, |u, v| {
                // Applications: four rounded squares.
                [(1.5, 1.5), (8.7, 1.5), (1.5, 8.7), (8.7, 8.7)]
                    .iter()
                    .any(|&(a, b)| rr(u, v, (a, b, a + 5.8, b + 5.8), 1.6))
            }),
            b'0'..=b'4' => app_icon(s, (kind - b'0') as usize, x, y, 16),
            _ => {}
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
        let (sw, sh) = (self.mw, self.lh);
        let off = self.dock_shift();
        if off as f32 >= DOCK_HIDDEN {
            return;
        }
        let (dx, dy, dw, dh) = self.dock_rect();
        let dy = dy + off;
        // With the name shown above an icon.
        if !self.screen.visible(dx, dy.saturating_sub(40), dw, dh + 40) {
            return;
        }
        self.mag_x = self.px;
        let (items, (bl, br)) = self.dock_layout();
        let (bx, bw) = (bl as usize, (br - bl + 0.5) as usize);
        let _ = (dx, dw);
        self.screen
            .round_rect(bx, dy, bw, dh, 18, t.dock_tint, t.dock_alpha, [true; 4]);
        self.screen.round_outline(bx, dy, bw, dh, 18, t.outline);
        let _ = (sw, sh);
        let (entries, n) = self.dock_entries();
        let bottom = (dy + DOCK_PAD + DOCK_ICON) as f32;
        for (slot, &k) in entries[..n].iter().enumerate() {
            let Some(&(ix, size)) = items.get(slot) else {
                continue;
            };
            let hop = self.bounce_height(k);
            self.draw_icon(k, ix, bottom - size - hop, size);
            if self.program_running(k) {
                let c = t.bar_text;
                let cx = (ix + size / 2.0) as usize;
                self.screen
                    .disc(cx - 2, dy + DOCK_PAD + DOCK_ICON + 2, 4, c, c);
            }
        }
        // Minimised windows, after a divider: each its window scaled into
        // the slot, with a small badge in its application's colour.
        if !self.minis.is_empty() {
            let n = self.dock_entries().1.max(1);
            let lx = zoo_abi::ui::dock_divider_x(sw, sh, n, self.minis.len());
            let (_, dy0, _, dh0) = self.dock_rect();
            self.screen
                .fill(lx, dy0 + off + 12, 1, dh0 - 24, t.separator);
            for j in 0..self.minis.len() {
                let i = self.minis[j];
                let Some(&(mxf, size)) = items.get(n + j) else {
                    continue;
                };
                let (mx, my) = (mxf as usize, (bottom - size) as usize);
                let (_, _, ww, wh) = self.wins[i];
                let d = size;
                let k = (d / ww.max(1) as f32).min(d / wh.max(1) as f32) * 0.92;
                let (tw, th) = (ww as f32 * k, wh as f32 * k);
                let rect = (
                    mx as f32 + (d - tw) / 2.0,
                    my as f32 + (d - th) / 2.0,
                    tw,
                    th,
                );
                let (scw, clip, s) = (self.screen.w, self.screen.clip, self.screen.s);
                composite_scaled(
                    self.screen.back,
                    scw,
                    clip,
                    s,
                    &self.layers[i],
                    (ww, wh),
                    rect,
                    1.0,
                );
                let badge = ICON[self.apps[i].prog];
                let (bx, by) = (mx + size as usize - 16, my + size as usize - 16);
                self.screen
                    .round_rect(bx, by, 18, 18, 5, 0xFFFFFF, 255, [true; 4]);
                self.screen
                    .round_rect(bx + 2, by + 2, 14, 14, 4, badge, 255, [true; 4]);
            }
        }
        if let Some(hit) = self.hover {
            let small = self.face(FaceId::Regular13);
            let (label, slot) = match hit {
                DockHit::App(k) => (CATALOG[k].0, entries[..n].iter().position(|&p| p == k)),
                DockHit::Window(i) => (
                    self.apps[i].title.as_str(),
                    self.minis.iter().position(|&w| w == i).map(|j| n + j),
                ),
            };
            let lw = small.width(label).div_ceil(self.screen.s) + 20;
            let Some(&(ix, size)) = slot.and_then(|s| items.get(s)) else {
                return;
            };
            let lx = ((ix + size / 2.0) as usize).saturating_sub(lw / 2);
            let ly = (bottom - size) as usize - 10 - 26;
            self.screen
                .round_rect(lx, ly, lw, 24, 6, t.tooltip, 255, [true; 4]);
            self.screen.round_outline(lx, ly, lw, 24, 6, t.outline);
            self.screen
                .text(&small, lx + 10, ly + 17, label, t.tooltip_text, lx + lw);
        }
    }

    /// An application's dock icon: a rounded square in its colour with a
    /// simple white mark.
    /// Application `k`'s dock icon at (x, y), from its cached image.
    /// Application `k`'s dock icon, `size` logical pixels square with its
    /// top-left at (x, y): its cached image (drawn at `MAG_ICON`), scaled
    /// down with bilinear filtering (docs/u2g-design.md).
    fn draw_icon(&mut self, k: usize, x: f32, y: f32, size: f32) {
        let s = self.screen.s;
        let (bw, bh) = (ICON_BOX.0 * s, ICON_BOX.1 * s);
        let q = size / MAG_ICON as f32;
        let (ox, oy) = (x - ICON_AT.0 as f32 * q, y - ICON_AT.1 as f32 * q);
        let (vw, vh) = (ICON_BOX.0 as f32 * q, ICON_BOX.1 as f32 * q);
        if !self.screen.visible(
            ox.max(0.0) as usize,
            oy.max(0.0) as usize,
            vw as usize + 2,
            vh as usize + 2,
        ) {
            return;
        }
        if self.icons.len() != PROGRAMS || self.icons[k].len() != bw * bh {
            self.build_icons();
        }
        let sf = s as f32;
        let (dx0, dy0) = (ox * sf, oy * sf);
        let (dx1, dy1) = (dx0 + vw * sf, dy0 + vh * sf);
        let (cx0, cy0, cx1, cy1) = self.screen.clip;
        let sw = self.screen.w;
        let img = &self.icons[k];
        let px = |x: usize, y: usize| img[y.min(bh - 1) * bw + x.min(bw - 1)];
        for row in (dy0.max(0.0) as usize).max(cy0)..((dy1 + 0.999) as usize).min(cy1) {
            let v = ((row as f32 + 0.5 - dy0) / q - 0.5).max(0.0);
            let (y0, fy) = (v as usize, v - (v as usize) as f32);
            for col in (dx0.max(0.0) as usize).max(cx0)..((dx1 + 0.999) as usize).min(cx1) {
                let u = ((col as f32 + 0.5 - dx0) / q - 0.5).max(0.0);
                let (x0, fx) = (u as usize, u - (u as usize) as f32);
                if x0 >= bw || y0 >= bh {
                    continue;
                }
                let c = bilerp(
                    px(x0, y0),
                    px(x0 + 1, y0),
                    px(x0, y0 + 1),
                    px(x0 + 1, y0 + 1),
                    fx,
                    fy,
                );
                let d = &mut self.screen.back[row * sw + col];
                *d = over(*d, c);
            }
        }
    }

    /// Draws every application icon once, over black and over white, and
    /// keeps them premultiplied (as `build_layer` does for windows).
    fn build_icons(&mut self) {
        let s = self.screen.s;
        let (bw, bh) = (ICON_BOX.0 * s, ICON_BOX.1 * s);
        self.icons.clear();
        for k in 0..PROGRAMS {
            let mut black = alloc::vec![0u32; bw * bh];
            let mut white = alloc::vec![0xFF_FFFFu32; bw * bh];
            for buf in [&mut black, &mut white] {
                // SAFETY: `buf` stays borrowed for this block and nothing
                // else uses it; it is swapped back out of `screen` before
                // the block ends, so it never outlives `buf`.
                let tmp: &'static mut [u32] =
                    unsafe { core::slice::from_raw_parts_mut(buf.as_mut_ptr(), buf.len()) };
                let back = core::mem::replace(&mut self.screen.back, tmp);
                let saved = (self.screen.w, self.screen.h, self.screen.clip);
                (self.screen.w, self.screen.h, self.screen.clip) = (bw, bh, (0, 0, bw, bh));
                app_icon(&mut self.screen, k, ICON_AT.0, ICON_AT.1, MAG_ICON);
                (self.screen.w, self.screen.h, self.screen.clip) = saved;
                self.screen.back = back;
            }
            for (b, &w) in black.iter_mut().zip(&white) {
                let a = 255 - (((w >> 8) & 0xFF).saturating_sub((*b >> 8) & 0xFF));
                let ch = |shift: u32| ((*b >> shift) & 0xFF).min(a) << shift;
                *b = a << 24 | ch(16) | ch(8) | ch(0);
            }
            self.icons.push(black);
        }
        zoo_rt::log!("PeacockHandler: dock icons drawn at {}x", s);
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

    fn send_key(&mut self, key: Key) {
        let i = self.focus;
        let app = &mut self.apps[i];
        if app.running && app.visible() {
            let mut l = Line::new();
            key.encode(&mut l);
            // Behind any keys already held, so the order stays.
            app.held.push_back(l);
            if app.held.len() > HELD_KEYS {
                // Stuck rather than busy: the oldest go.
                app.held.pop_front();
            }
            self.deliver_keys(i);
        }
    }

    /// Sends app `i` the keys held for it while its queue has room. A full
    /// queue keeps them for the next pass of the main loop: a busy app (at
    /// desktop start the models are read from disk) loses no typing
    /// (docs/keys-design.md).
    fn deliver_keys(&mut self, i: usize) {
        let app = &mut self.apps[i];
        while let Some(l) = app.held.front() {
            match zoo_rt::send(app.chan, l.as_str().as_bytes()) {
                Ok(()) => {
                    app.held.pop_front();
                }
                Err(zoo_rt::Error::Full) | Err(zoo_rt::Error::NoMem) => {
                    app.held_burst = app.held_burst.max(app.held.len());
                    return;
                }
                Err(_) => {
                    app.held.clear();
                    return;
                }
            }
        }
        if app.held_burst > 0 {
            zoo_rt::log!(
                "PeacockHandler: {} was busy; held up to {} keys, all delivered",
                app.label,
                app.held_burst
            );
            app.held_burst = 0;
        }
    }

    /// Handles an application whose channel closed: it has ended.
    fn app_ended(&mut self, i: usize) {
        self.apps[i].asked_by = None;
        self.apps[i].held.clear();
        self.apps[i].held_burst = 0;
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
    // Monitors side by side, as wide as each other (docs/monitors-design.md).
    let monitors = (info.monitors as usize).clamp(1, 4);
    let mon_w = full_w / monitors;
    // 4K-class screens are drawn at 2x: the same layout with twice the
    // pixels in each direction, like a Retina display (docs/u2b-design.md).
    let sc = if mon_w >= 3200 && full_h >= 1800 {
        2
    } else {
        1
    };
    // The logical size, at most MAX_W x MAX_H a monitor, and the device
    // area used.
    let (w, h) = ((mon_w / sc).min(MAX_W) * monitors, (full_h / sc).min(MAX_H));
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
        layers: [const { Layer::new() }; WINDOWS],
        scratch: Vec::new(),
        icons: Vec::new(),
        hw_cursor: false,
        cursor_moved: false,
        cursor_hidden: false,
        cursor_switch: false,
        last_frame_tick: u64::MAX,
        pointer: pointer_image(sc),
        lw: w,
        lh: h,
        monitors,
        mw: w / monitors,
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
        // Each application's first window, then free slots (closed).
        apps: core::array::from_fn(|i| {
            let mut a = App::new(i % PROGRAMS);
            a.closed = i >= PROGRAMS;
            a
        }),
        home,
        focus: 0,
        recent: alloc::collections::VecDeque::new(),
        wins: {
            // On the first monitor (docs/monitors-design.md).
            let first = zoo_abi::ui::initial_windows(w / monitors, h);
            core::array::from_fn(|i| first[i % PROGRAMS])
        },
        order: [5, 6, 7, 8, 4, 3, 1, 2, 0],
        px: w / monitors / 2,
        py: h / 2,
        held: false,
        drag: None,
        resize: None,
        last_click: (0, 0, 0),
        last_line: (0, WINDOWS, 0),
        unzoomed: [None; WINDOWS],
        damage: [(0, 0, 0, 0); MAX_DAMAGE],
        damaged: 0,
        full: true,
        always_full: false,
        started: (cycles(), zoo_rt::ticks()),
        frames: [(0, 0); FRAME_SAMPLES],
        shows: [0; FRAME_SAMPLES],
        frame_count: 0,
        hover: None,
        anims: [const { None }; WINDOWS],
        per_ms: calibrate_ms(),
        frame_now: 0,
        shift: false,
        ctrl: false,
        // Network is in Applications, not the dock, at first.
        pinned: core::array::from_fn(|k| k != NETWORK),
        minis: Vec::new(),
        menu: None,
        rheld: false,
        paw: paw_mask(PAW_SIZE * sc),
        dock_off: DOCK_HIDDEN,
        dock_shown: false,
        dock_slide: None,
        mag: 0.0,
        mag_x: 0,
        frame_prev: 0,
        bounce: [None; PROGRAMS],
        dock_hide_at: None,
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
    desk.load_dock();
    desk.load_appearance();
    // Notes opens from the dock (docs/d4-design.md).
    desk.apps[NOTES].closed = true;
    desk.apps[NETWORK].closed = true;
    for app in desk.apps.iter_mut().filter(|a| !a.closed) {
        if !app.start(home) {
            zoo_rt::log!("PeacockHandler: cannot start {}", app.program);
        }
    }
    // The windows open with an animation (docs/u2d-design.md).
    for i in 0..WINDOWS {
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

        // Keys held for busy apps, before any new ones.
        for i in 0..desk.apps.len() {
            if !desk.apps[i].held.is_empty() {
                desk.deliver_keys(i);
            }
        }

        // Keyboard.
        let n = input_read(&mut kb).unwrap_or(0);
        for &byte in &kb[..n] {
            let Ok(Some(event)) = decoder.add_byte(byte) else {
                continue;
            };
            // Shift held: animations in slow motion, as on a Mac
            // (docs/u2d-design.md).
            if matches!(event.code, KeyCode::LShift | KeyCode::RShift) {
                desk.shift = event.state == KeyState::Down;
            }
            if matches!(event.code, KeyCode::LControl | KeyCode::RControl) {
                desk.ctrl = event.state == KeyState::Down;
            }
            match decoder.process_keyevent(event) {
                Some(DecodedKey::Unicode('\t')) => {
                    desk.focus_next();
                    dirty = true;
                }
                Some(DecodedKey::Unicode('\u{1b}')) if desk.menu.is_some() => {
                    desk.close_menu();
                    dirty = true;
                }
                Some(DecodedKey::Unicode('\u{1b}')) => {
                    let _ = log("PeacockHandler: leaving the desktop");
                    return 0;
                }
                // The appearance (docs/u1-design.md).
                Some(DecodedKey::RawKey(KeyCode::F8)) => {
                    desk.change_appearance(!desk.dark);
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
                // Ctrl+N: a new window of the focused application; other
                // Ctrl+letters go to the application (`key:ctrl:<c>`,
                // capital with Shift).
                Some(DecodedKey::Unicode(c)) if desk.ctrl && c.is_ascii_alphabetic() => {
                    let c = if desk.shift {
                        c.to_ascii_uppercase()
                    } else {
                        c.to_ascii_lowercase()
                    };
                    if c == 'n' {
                        desk.new_window();
                        dirty = true;
                    } else if c == 'w' || c == 'm' {
                        // As the Window menu (docs/u4a-design.md).
                        let i = desk.focus;
                        if desk.apps[i].visible() {
                            if c == 'w' {
                                desk.close_app(i);
                            } else {
                                desk.minimize(i);
                            }
                            dirty = true;
                        }
                    } else {
                        desk.send_key(Key::Ctrl(c));
                    }
                }
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
                        let changed = desk.apps[i].apply(&msg[..n]);
                        if let Some(req) = desk.apps[i].request.take() {
                            desk.app_request(i, req);
                            dirty = true;
                        }
                        if changed {
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
        } else if desk.apps.iter().any(|a| !a.held.is_empty()) {
            // Keys wait for a busy app: try again next tick.
            zoo_rt::sleep(1);
        } else {
            // Until input, a message from an application, or the next
            // second (the clock); docs/u1b-design.md.
            let now = zoo_rt::ticks();
            let _ = zoo_rt::input_wait((now / 100 + 1) * 100 - now);
        }
    }
}
