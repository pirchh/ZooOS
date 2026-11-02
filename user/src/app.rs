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

    /// Asks for application `program` to be opened.
    pub fn launch(&self, program: &str) -> Result<(), Error> {
        let mut l = Line::new();
        let _ = write!(l, "launch:{}", program);
        self.send_line(&l)
    }
}

/// A message from the window server other than a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// The right button went down on content line `n`.
    RightClick(usize),
    /// Item `n` of the menu this application asked for was chosen.
    MenuChoose(usize),
}

impl Event {
    pub fn parse(msg: &[u8]) -> Option<Event> {
        let num = |s: &[u8]| core::str::from_utf8(s).ok()?.parse().ok();
        if let Some(n) = msg.strip_prefix(b"rclick:") {
            return num(n).map(Event::RightClick);
        }
        msg.strip_prefix(b"menu-choose:")
            .and_then(num)
            .map(Event::MenuChoose)
    }
}
