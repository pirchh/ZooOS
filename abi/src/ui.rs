//! Desktop geometry (docs/u1-design.md), shared by the window server and
//! the verifier so tests click where things are drawn. Rectangles are
//! (x, y, width, height) in pixels; a window's includes its title bar.

/// A window's sidebar width, when its application has one
/// (docs/u3f-design.md).
pub const SIDEBAR_W: usize = 180;

/// Bytes an application puts in a content line (docs/u3f-design.md).
pub mod mark {
    /// Followed by a role byte (`ROLES`): the text after it in that colour.
    pub const COLOUR: char = '\u{1}';
    /// Followed by an icon byte, at the start of a line: a 16-px icon.
    pub const ICON: char = '\u{2}';
    /// The text cursor, drawn as a bar here.
    pub const CARET: char = '\u{3}';
    /// The rest of the line, right-aligned and dim.
    pub const RIGHT: char = '\t';
    /// Colour roles: text, keyword, string, comment, number, type,
    /// function, macro, dim.
    pub const ROLES: &[u8] = b"0kscntfmd";
    /// Icons: folder, document, code document; sidebar symbols home,
    /// documents, music, photos, applications, the Trash; `0`-`4` an
    /// application.
    pub const ICONS: &[u8] = b"FDCHOMPAT01234";
}

/// The applications, in dock order: (label, program). Files lists them in
/// its Applications folder (docs/u3-design.md).
pub const APPLICATIONS: [(&str, &str); 5] = [
    ("Files", "files"),
    ("Crash Test", "crash-test"),
    ("Assistant", "assistant"),
    ("Notes", "notes"),
    ("Network", "network"),
];

/// Context menus (docs/u3-design.md): an item's height, a separator's,
/// and the padding above the first item and below the last.
pub const MENU_ITEM_H: usize = 24;
pub const MENU_SEP_H: usize = 9;
pub const MENU_PAD: usize = 6;

/// A context menu's height for items where `true` marks a separator.
pub fn menu_height(separators: &[bool]) -> usize {
    2 * MENU_PAD
        + separators
            .iter()
            .map(|&sep| if sep { MENU_SEP_H } else { MENU_ITEM_H })
            .sum::<usize>()
}

/// The top of item `k` in a menu whose top is at `top`.
pub fn menu_item_top(top: usize, separators: &[bool], k: usize) -> usize {
    top + MENU_PAD
        + separators[..k]
            .iter()
            .map(|&sep| if sep { MENU_SEP_H } else { MENU_ITEM_H })
            .sum::<usize>()
}

/// The menu bar's height.
pub const MENU_H: usize = 26;
/// A window's title bar.
pub const TITLE_H: usize = 28;
/// Window corner radius.
pub const RADIUS: usize = 10;
/// A content line's height, and the space above the first.
pub const LINE_H: usize = 22;
pub const CONTENT_TOP: usize = 8;
/// Where text starts inside a window, and its baseline within a line.
pub const TEXT_X: usize = 16;
pub const BASELINE: usize = 15;
/// Traffic lights: diameter, first centre's distance from the window's
/// left edge, distance between centres.
pub const LIGHT_D: usize = 12;
pub const LIGHT_X: usize = 19;
pub const LIGHT_GAP: usize = 20;
/// The resize grab area at a window's bottom-right corner.
pub const RESIZE_CORNER: usize = 14;
/// Dock: icon size, padding, gap between icons, bottom margin.
pub const DOCK_ICON: usize = 52;
pub const DOCK_PAD: usize = 8;
pub const DOCK_GAP: usize = 12;
pub const DOCK_BOTTOM: usize = 8;
/// Height of the dock and the space above it windows stay out of.
pub const DOCK_H: usize = DOCK_ICON + 2 * DOCK_PAD + 6;
pub const DOCK_AREA: usize = DOCK_H + DOCK_BOTTOM + 24;

/// Where the applications' first windows start: Files on the left, the
/// Assistant and Crash Test on the right, Notes and Network (opened later)
/// in the middle. Proportional, with space between them.
pub fn initial_windows(w: usize, h: usize) -> [(usize, usize, usize, usize); 5] {
    let top = MENU_H + 40;
    let usable = h.saturating_sub(top + DOCK_AREA);
    let right_x = w * 52 / 100;
    let right_w = w * 40 / 100;
    let ass_h = usable * 45 / 100;
    [
        (w * 6 / 100, top, w * 42 / 100, usable * 72 / 100),
        (right_x, top + ass_h + 32, right_w, usable * 30 / 100),
        (right_x, top, right_w, ass_h),
        (
            w * 25 / 100,
            top + usable * 12 / 100,
            w * 36 / 100,
            usable * 58 / 100,
        ),
        (
            w * 30 / 100,
            top + usable * 8 / 100,
            w * 40 / 100,
            usable * 62 / 100,
        ),
    ]
}

/// A zoomed window: the space between the menu bar and the dock.
pub fn zoomed(w: usize, h: usize) -> (usize, usize, usize, usize) {
    let top = MENU_H + 12;
    (24, top, w - 48, h.saturating_sub(top + DOCK_AREA - 12))
}

/// The centre of window `win`'s traffic light `k` (0 close, 1 minimise,
/// 2 zoom).
pub fn light_centre(win: (usize, usize, usize, usize), k: usize) -> (usize, usize) {
    (win.0 + LIGHT_X + k * LIGHT_GAP, win.1 + TITLE_H / 2)
}

/// The dock (x, y, w, h) for `apps` icons on a `w` x `h` screen.
pub fn dock_rect(w: usize, h: usize, apps: usize) -> (usize, usize, usize, usize) {
    dock_rect_with(w, h, apps, 0)
}

/// The divider between applications and minimised windows: a gap, a
/// one-pixel line, a gap (docs/u3-design.md).
pub const DOCK_DIVIDER: usize = 2 * DOCK_GAP + 1;

/// The dock with `apps` applications and `minis` minimised windows.
pub fn dock_rect_with(
    w: usize,
    h: usize,
    apps: usize,
    minis: usize,
) -> (usize, usize, usize, usize) {
    let mut dw = apps * DOCK_ICON + apps.saturating_sub(1) * DOCK_GAP + 2 * DOCK_PAD;
    if minis > 0 {
        dw += DOCK_DIVIDER + minis * DOCK_ICON + (minis - 1) * DOCK_GAP;
    }
    ((w - dw) / 2, h - DOCK_BOTTOM - DOCK_H, dw, DOCK_H)
}

/// The top-left of dock icon `k`.
pub fn dock_icon(w: usize, h: usize, apps: usize, k: usize) -> (usize, usize) {
    dock_icon_with(w, h, apps, 0, k)
}

/// The top-left of application icon `k` in a dock with minimised windows.
pub fn dock_icon_with(w: usize, h: usize, apps: usize, minis: usize, k: usize) -> (usize, usize) {
    let (x, y, _, _) = dock_rect_with(w, h, apps, minis);
    (x + DOCK_PAD + k * (DOCK_ICON + DOCK_GAP), y + DOCK_PAD)
}

/// The x of the divider line.
pub fn dock_divider_x(w: usize, h: usize, apps: usize, minis: usize) -> usize {
    let (x, _, _, _) = dock_rect_with(w, h, apps, minis);
    x + DOCK_PAD + apps * DOCK_ICON + apps.saturating_sub(1) * DOCK_GAP + DOCK_GAP
}

/// The top-left of minimised window `j`'s thumbnail.
pub fn dock_mini(w: usize, h: usize, apps: usize, minis: usize, j: usize) -> (usize, usize) {
    let (_, y, _, _) = dock_rect_with(w, h, apps, minis);
    let x = dock_divider_x(w, h, apps, minis) + 1 + DOCK_GAP;
    (x + j * (DOCK_ICON + DOCK_GAP), y + DOCK_PAD)
}

/// The top of content line `k` in window `win`.
pub fn line_top(win: (usize, usize, usize, usize), k: usize) -> usize {
    win.1 + TITLE_H + CONTENT_TOP + k * LINE_H
}

/// The appearance switch in the menu bar: its centre, `clock_w` being the
/// clock text's width.
pub fn appearance_switch(w: usize, clock_w: usize) -> (usize, usize) {
    (w - 16 - clock_w - 22, MENU_H / 2)
}

/// A desktop appearance (docs/u1-design.md §3), colours as 0xRRGGBB.
pub struct Palette {
    pub name: &'static str,
    pub wall_top: u32,
    pub wall_bottom: u32,
    /// Menu bar and dock: a tint laid over what is behind them.
    pub bar_tint: u32,
    pub bar_alpha: u8,
    pub bar_text: u32,
    pub dock_tint: u32,
    pub dock_alpha: u8,
    pub title: u32,
    pub title_blurred: u32,
    pub title_text: u32,
    pub title_text_blurred: u32,
    pub separator: u32,
    pub outline: u32,
    pub content: u32,
    pub text: u32,
    pub text_dim: u32,
    pub select: u32,
    pub select_text: u32,
    pub select_blurred: u32,
    pub light_off: u32,
    pub banner: u32,
    /// Shadow strength (of black) for the focused and other windows.
    pub shadow: u8,
    pub shadow_blurred: u8,
    pub tooltip: u32,
    pub tooltip_text: u32,
    /// A window's sidebar (docs/u3f-design.md) and its selected item.
    pub sidebar: u32,
    pub sidebar_select: u32,
    /// Sidebar symbols.
    pub accent: u32,
    /// Syntax colours, by role: keyword, string, comment, number, type,
    /// function, macro (docs/u3f-design.md).
    pub syntax: [u32; 7],
}

pub const LIGHT: Palette = Palette {
    name: "light",
    wall_top: 0x7FB3DD,
    wall_bottom: 0xC6B4E3,
    bar_tint: 0xF6F6F6,
    bar_alpha: 205,
    bar_text: 0x1D1D1F,
    dock_tint: 0xF5F5F7,
    dock_alpha: 150,
    title: 0xE8E8E8,
    title_blurred: 0xF6F6F6,
    title_text: 0x1D1D1F,
    title_text_blurred: 0xA1A1A6,
    separator: 0xD2D2D2,
    outline: 0xB0B0B4,
    content: 0xFFFFFF,
    text: 0x1D1D1F,
    text_dim: 0x86868B,
    select: 0x0A64D8,
    select_text: 0xFFFFFF,
    select_blurred: 0xDCDCDC,
    light_off: 0xCFCFD3,
    banner: 0xF5F5F7,
    shadow: 72,
    shadow_blurred: 36,
    tooltip: 0xEDEDED,
    tooltip_text: 0x1D1D1F,
    sidebar: 0xF0F0F2,
    sidebar_select: 0xDCDCE0,
    accent: 0x007AFF,
    syntax: [
        0x9B2393, 0xC41A16, 0x5D6C79, 0x1C00CF, 0x0B4F79, 0x326D74, 0x643820,
    ],
};

pub const DARK: Palette = Palette {
    name: "dark",
    wall_top: 0x1B2645,
    wall_bottom: 0x3B2B58,
    bar_tint: 0x1E1E1E,
    bar_alpha: 205,
    bar_text: 0xF5F5F7,
    dock_tint: 0x2C2C2E,
    dock_alpha: 170,
    title: 0x38383A,
    title_blurred: 0x2C2C2E,
    title_text: 0xF5F5F7,
    title_text_blurred: 0x8E8E93,
    separator: 0x1C1C1E,
    outline: 0x5A5A5E,
    content: 0x1E1E1E,
    text: 0xF5F5F7,
    text_dim: 0x98989D,
    select: 0x0A64D8,
    select_text: 0xFFFFFF,
    select_blurred: 0x464649,
    light_off: 0x4A4A4D,
    banner: 0x2C2C2E,
    shadow: 120,
    shadow_blurred: 70,
    tooltip: 0x3A3A3C,
    tooltip_text: 0xF5F5F7,
    sidebar: 0x262628,
    sidebar_select: 0x3A3A3D,
    accent: 0x0A84FF,
    syntax: [
        0xFF7AB2, 0xFF8170, 0x7F8C98, 0xD9C97C, 0x6BDFFF, 0x67B7A4, 0xFD8F3F,
    ],
};

/// The menu bar's colour: the appearance's tint over the wallpaper's top.
pub fn bar_color(p: &Palette) -> u32 {
    crate::font::blend(p.wall_top, p.bar_tint, p.bar_alpha)
}
