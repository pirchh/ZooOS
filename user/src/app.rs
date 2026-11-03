//! The desktop application protocol (docs/m4-design.md §5): short text
//! messages between an application and the window server.

use crate::{Error, Handle, Line, send, sleep};
use core::fmt::Write as _;

/// Content lines a window can show.
pub const MAX_LINES: usize = 24;

/// A key delivered to the focused application.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Up,
    Down,
    Left,
    Right,
    Enter,
    Back,
    Char(char),
    /// Ctrl and a letter (capital with Shift), other than Ctrl+N, which
    /// the window server keeps (docs/u3f-design.md).
    Ctrl(char),
}

impl Key {
    pub fn parse(msg: &[u8]) -> Option<Key> {
        Some(match msg {
            b"key:up" => Key::Up,
            b"key:down" => Key::Down,
            b"key:left" => Key::Left,
            b"key:right" => Key::Right,
            b"key:enter" => Key::Enter,
            b"key:back" => Key::Back,
            _ if msg.starts_with(b"key:ctrl:") && msg.len() == 10 => Key::Ctrl(char::from(msg[9])),
            _ => {
                let rest = msg.strip_prefix(b"key:char:")?;
                let s = core::str::from_utf8(rest).ok()?;
                let mut chars = s.chars();
                let c = chars.next()?;
                if chars.next().is_some() {
                    return None;
                }
                Key::Char(c)
            }
        })
    }

    /// The message the server sends for this key.
    pub fn encode(self, out: &mut Line) {
        let _ = match self {
            Key::Up => out.write_str("key:up"),
            Key::Down => out.write_str("key:down"),
            Key::Left => out.write_str("key:left"),
            Key::Right => out.write_str("key:right"),
            Key::Enter => out.write_str("key:enter"),
            Key::Back => out.write_str("key:back"),
            Key::Char(c) => write!(out, "key:char:{}", c),
            Key::Ctrl(c) => write!(out, "key:ctrl:{}", c),
        };
    }
}

/// Sends `msg`, waiting (bounded) while the receiver's queue is full.
pub fn send_wait(h: Handle, msg: &[u8]) -> Result<(), Error> {
    for _ in 0..1000 {
        match send(h, msg) {
            Err(Error::Full) => sleep(1),
            other => return other,
        }
    }
    Err(Error::Full)
}

/// The application's side of its window.
pub struct Window {
    h: Handle,
}

impl Window {
    pub fn new(channel: Handle) -> Window {
        Window { h: channel }
    }

    fn send_line(&self, line: &Line) -> Result<(), Error> {
        send_wait(self.h, line.as_str().as_bytes())
    }

    pub fn title(&self, title: &str) -> Result<(), Error> {
        let mut l = Line::new();
        let _ = write!(l, "title:{}", title);
        self.send_line(&l)
    }

    pub fn lines(&self, n: usize) -> Result<(), Error> {
        let mut l = Line::new();
        let _ = write!(l, "lines:{}", n.min(MAX_LINES));
        self.send_line(&l)
    }

    pub fn line(&self, i: usize, text: &str) -> Result<(), Error> {
        let mut l = Line::new();
        let _ = write!(l, "line:{}:{}", i, text);
        self.send_line(&l)
    }

    pub fn select(&self, i: Option<usize>) -> Result<(), Error> {
        let mut l = Line::new();
        let _ = match i {
            Some(i) => write!(l, "sel:{}", i),
            None => l.write_str("sel:-"),
        };
        self.send_line(&l)
    }

    pub fn show(&self) -> Result<(), Error> {
        send_wait(self.h, b"show")
    }

    /// Shows a context menu of `items` at the pointer (docs/u3-design.md);
    /// the choice comes back as `menu-choose:<index>`.
    pub fn menu(&self, items: &[&str]) -> Result<(), Error> {
        let mut l = Line::new();
        let _ = l.write_str("menu:");
        for (k, item) in items.iter().enumerate() {
            let _ = write!(l, "{}{}", if k > 0 { "|" } else { "" }, item);
        }
        self.send_line(&l)
    }

    /// Asks for application `program` to be kept in the dock.
    pub fn dock_add(&self, program: &str) -> Result<(), Error> {
        let mut l = Line::new();
        let _ = write!(l, "dock-add:{}", program);
        self.send_line(&l)
    }

    /// Sidebar item `k` (docs/u3f-design.md); markup as in lines.
    pub fn side(&self, k: usize, text: &str) -> Result<(), Error> {
        let mut l = Line::new();
        let _ = write!(l, "side:{}:{}", k, text);
        self.send_line(&l)
    }

    /// How many sidebar items there are; 0: no sidebar.
    pub fn sides(&self, n: usize) -> Result<(), Error> {
        let mut l = Line::new();
        let _ = write!(l, "sides:{}", n);
        self.send_line(&l)
    }

    pub fn side_select(&self, k: Option<usize>) -> Result<(), Error> {
        let mut l = Line::new();
        let _ = match k {
            Some(k) => write!(l, "side-sel:{}", k),
            None => l.write_str("side-sel:-"),
        };
        self.send_line(&l)
    }

    /// Asks for file `path` (from home) to be opened in a window of
    /// application `program`, which gets `open:<path>`.
    pub fn open(&self, program: &str, path: &str) -> Result<(), Error> {
        let mut l = Line::new();
        let _ = write!(l, "open:{}:{}", program, path);
        self.send_line(&l)
    }

    /// Asks the window server what is on the screen; it answers with a
    /// `context:` message (docs/context-design.md).
    pub fn context(&self) -> Result<(), Error> {
        let mut l = Line::new();
        let _ = l.write_str("context?");
        self.send_line(&l)
    }

    /// Asks for application `program` to be opened.
    pub fn launch(&self, program: &str) -> Result<(), Error> {
        let mut l = Line::new();
        let _ = write!(l, "launch:{}", program);
        self.send_line(&l)
    }
}

/// A message from the window server other than a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event<'a> {
    /// The right button went down on content line `n`.
    RightClick(usize),
    /// The right button went down in the content, below the lines.
    RightClickEmpty,
    /// Item `n` of the menu this application asked for was chosen.
    MenuChoose(usize),
    /// A click on content line `n` (docs/u3f-design.md).
    Click(usize),
    /// The second click of a double-click on content line `n`.
    DoubleClick(usize),
    /// A click on sidebar item `n`.
    SideClick(usize),
    /// Open this file (a path from home).
    Open(&'a str),
}

impl Event<'_> {
    pub fn parse(msg: &[u8]) -> Option<Event<'_>> {
        let num = |s: &[u8]| core::str::from_utf8(s).ok()?.parse().ok();
        if msg == b"rclick:none" {
            return Some(Event::RightClickEmpty);
        }
        type Make = fn(usize) -> Event<'static>;
        let numbered: [(&[u8], Make); 5] = [
            (b"rclick:", Event::RightClick),
            (b"menu-choose:", Event::MenuChoose),
            (b"click:", Event::Click),
            (b"dclick:", Event::DoubleClick),
            (b"sclick:", Event::SideClick),
        ];
        for (prefix, make) in numbered {
            if let Some(n) = msg.strip_prefix(prefix) {
                return num(n).map(make);
            }
        }
        let path = msg.strip_prefix(b"open:")?;
        core::str::from_utf8(path).ok().map(Event::Open)
    }
}
