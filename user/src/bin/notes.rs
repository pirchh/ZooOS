//! Notes: a text editor (docs/d4-design.md, docs/u3f-design.md). It opens
//! `Documents/notes.txt` in the home folder, or the file Files asks it to
//! (`open:<path>`). Arrow keys move the cursor (docs/d10-design.md);
//! typing, Enter and Backspace work at the cursor. Program text is
//! coloured by its kind (by the file's extension). Changes are saved a
//! second after typing stops, at Ctrl+S, and when the window closes.

#![no_std]
#![no_main]

extern crate alloc;

use core::fmt::Write as _;
use zoo_abi::ui::mark;
use zoo_abi::{MAX_MESSAGE, WAIT_FOREVER};
use zoo_rt::app::{Event, Key, MAX_LINES, Window};
use zoo_rt::{Error, Line, file_create, file_read, file_truncate, file_write, try_recv, wait_any};

zoo_rt::entry2!(main);

const DEFAULT: &str = "Documents/notes.txt";
/// Largest file Notes edits.
const CAPACITY: usize = 8192;
/// Characters per shown line; longer lines wrap.
const WRAP: usize = 72;
/// Ticks after the last change before saving.
const SAVE_AFTER: u64 = 100;
/// Longest path kept.
const MAX_PATH: usize = 200;

/// The kind of text, for colours (docs/u3f-design.md).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Lang {
    Plain,
    Rust,
    C,
    Python,
    Shell,
    Js,
    Toml,
    Json,
    Markdown,
}

impl Lang {
    fn of(path: &str) -> Lang {
        let name = path.rsplit('/').next().unwrap_or(path);
        if !name.contains('.') {
            return Lang::Plain;
        }
        match name.rsplit('.').next().unwrap_or("") {
            "rs" => Lang::Rust,
            "c" | "h" | "cpp" | "hpp" => Lang::C,
            "py" => Lang::Python,
            "sh" => Lang::Shell,
            "js" | "ts" => Lang::Js,
            "toml" | "ini" | "conf" => Lang::Toml,
            "json" => Lang::Json,
            "md" => Lang::Markdown,
            _ => Lang::Plain,
        }
    }

    fn keywords(self) -> &'static [&'static str] {
        match self {
            Lang::Rust => &[
                "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else",
                "enum", "extern", "false", "fn", "for", "if", "impl", "in", "let", "loop", "match",
                "mod", "move", "mut", "pub", "ref", "return", "self", "Self", "static", "struct",
                "super", "trait", "true", "type", "unsafe", "use", "where", "while",
            ],
            Lang::C => &[
                "auto", "bool", "break", "case", "char", "const", "continue", "default", "do",
                "double", "else", "enum", "extern", "float", "for", "goto", "if", "int", "long",
                "return", "short", "signed", "sizeof", "static", "struct", "switch", "typedef",
                "union", "unsigned", "void", "volatile", "while",
            ],
            Lang::Python => &[
                "and", "as", "assert", "break", "class", "continue", "def", "del", "elif", "else",
                "except", "False", "finally", "for", "from", "global", "if", "import", "in", "is",
                "lambda", "None", "nonlocal", "not", "or", "pass", "raise", "return", "self",
                "True", "try", "while", "with", "yield",
            ],
            Lang::Shell => &[
                "case", "do", "done", "echo", "elif", "else", "esac", "export", "fi", "for",
                "function", "if", "in", "local", "return", "then", "while",
            ],
            Lang::Js => &[
                "async",
                "await",
                "break",
                "case",
                "catch",
                "class",
                "const",
                "continue",
                "default",
                "do",
                "else",
                "export",
                "extends",
                "false",
                "finally",
                "for",
                "function",
                "if",
                "import",
                "in",
                "let",
                "new",
                "null",
                "of",
                "return",
                "this",
                "throw",
                "true",
                "try",
                "typeof",
                "undefined",
                "var",
                "while",
            ],
            Lang::Toml | Lang::Json => &["true", "false", "null"],
            Lang::Plain | Lang::Markdown => &[],
        }
    }

    /// The line comment's start.
    fn comment(self) -> &'static [u8] {
        match self {
            Lang::Rust | Lang::C | Lang::Js => b"//",
            Lang::Python | Lang::Shell | Lang::Toml => b"#",
            _ => b"",
        }
    }
}

/// Rust's built-in types, coloured as types.
const PRIMITIVES: [&str; 17] = [
    "bool", "char", "str", "u8", "u16", "u32", "u64", "u128", "usize", "i8", "i16", "i32", "i64",
    "i128", "isize", "f32", "f64",
];

/// Colour roles, as markup bytes (`mark::ROLES`).
const TEXT: u8 = b'0';
const KEYWORD: u8 = b'k';
const STRING: u8 = b's';
const COMMENT: u8 = b'c';
const NUMBER: u8 = b'n';
const TYPE: u8 = b't';
const FUNCTION: u8 = b'f';
const MACRO: u8 = b'm';
const DIM: u8 = b'd';

struct Editor {
    home: u64,
    path: [u8; MAX_PATH],
    path_len: usize,
    lang: Lang,
    text: [u8; CAPACITY],
    len: usize,
    /// A byte offset into the text.
    cursor: usize,
    /// The first visual line shown.
    top: usize,
    editing: bool,
    /// The colour role of each byte (`colour`).
    roles: [u8; CAPACITY],
}

fn main(channel: u64, home: u64) -> u8 {
    let win = Window::new(channel);
    // On the heap: 16 KiB of arrays is a lot for a stack.
    let ed = alloc_editor(home);
    if !ed.load(DEFAULT) {
        return 3;
    }
    let mut msg = [0u8; MAX_MESSAGE];
    loop {
        if ed.draw(&win).is_err() {
            return 0; // the window server went away
        }
        let timeout = if ed.editing { SAVE_AFTER } else { WAIT_FOREVER };
        match wait_any(&[channel], timeout) {
            Ok(_) => {}
            Err(Error::TimedOut) => {
                ed.editing = !ed.save();
                continue;
            }
            Err(_) => return 4,
        }
        loop {
            match try_recv(channel, &mut msg) {
                Ok(n) => {
                    if let Some(Event::Open(path)) = Event::parse(&msg[..n]) {
                        if ed.editing {
                            ed.save();
                        }
                        let mut p = [0u8; MAX_PATH];
                        let k = path.len().min(MAX_PATH);
                        p[..k].copy_from_slice(&path.as_bytes()[..k]);
                        ed.load(core::str::from_utf8(&p[..k]).unwrap_or(DEFAULT));
                    } else if let Some(key) = Key::parse(&msg[..n]) {
                        ed.key(key);
                    }
                }
                Err(Error::Again) => break,
                Err(Error::PeerClosed) => {
                    // Closed: keep what was typed.
                    if ed.editing {
                        ed.save();
                    }
                    return 0;
                }
                Err(_) => return 4,
            }
        }
    }
}

fn alloc_editor(home: u64) -> &'static mut Editor {
    alloc::boxed::Box::leak(alloc::boxed::Box::new(Editor {
        home,
        path: [0; MAX_PATH],
        path_len: 0,
        lang: Lang::Plain,
        text: [0; CAPACITY],
        len: 0,
        cursor: 0,
        top: 0,
        editing: false,
        roles: [TEXT; CAPACITY],
    }))
}

impl Editor {
    fn path(&self) -> &str {
        core::str::from_utf8(&self.path[..self.path_len]).unwrap_or(DEFAULT)
    }

    /// Reads `path` (a missing file is empty); false if it cannot be read.
    fn load(&mut self, path: &str) -> bool {
        let k = path.len().min(MAX_PATH);
        self.path[..k].copy_from_slice(&path.as_bytes()[..k]);
        self.path_len = k;
        self.lang = Lang::of(path);
        self.len = 0;
        while self.len < CAPACITY {
            let path = core::str::from_utf8(&self.path[..self.path_len]).unwrap_or(DEFAULT);
            match file_read(self.home, path, self.len as u64, &mut self.text[self.len..]) {
                Ok(0) | Err(Error::NotFound) => break,
                Ok(n) => self.len += n,
                Err(e) => {
                    zoo_rt::log!("Notes: cannot read {}: {}", path, e.name());
                    return false;
                }
            }
        }
        // It starts at the end, as before.
        self.cursor = self.len;
        self.top = 0;
        self.editing = false;
        zoo_rt::log!("Notes: opened {} ({} bytes)", self.path(), self.len);
        true
    }

    /// Rewrites the file; true on success.
    fn save(&self) -> bool {
        let path = self.path();
        let text = &self.text[..self.len];
        let result = match file_truncate(self.home, path, 0) {
            Err(Error::NotFound) => file_create(self.home, path),
            other => other,
        }
        .and_then(|()| file_write(self.home, path, 0, text));
        match result {
            Ok(_) => {
                zoo_rt::log!("Notes: saved {} bytes", text.len());
                true
            }
            Err(e) => {
                zoo_rt::log!("Notes: cannot save {}: {}", path, e.name());
                false
            }
        }
    }

    fn insert(&mut self, b: u8) {
        if self.len >= CAPACITY {
            return;
        }
        self.text
            .copy_within(self.cursor..self.len, self.cursor + 1);
        self.text[self.cursor] = b;
        self.len += 1;
        self.cursor += 1;
        self.editing = true;
    }

    fn key(&mut self, key: Key) {
        match key {
            Key::Char(c) if c.is_ascii() && !c.is_ascii_control() => self.insert(c as u8),
            Key::Enter => self.insert(b'\n'),
            Key::Back if self.cursor > 0 => {
                self.text
                    .copy_within(self.cursor..self.len, self.cursor - 1);
                self.len -= 1;
                self.cursor -= 1;
                self.editing = true;
            }
            Key::Left => self.cursor = self.cursor.saturating_sub(1),
            Key::Right => self.cursor = (self.cursor + 1).min(self.len),
            Key::Up | Key::Down => self.vertical(key == Key::Up),
            Key::Ctrl('s') => self.editing = !self.save(),
            _ => {}
        }
    }

    /// Up or Down a visual line, keeping the column where it can.
    fn vertical(&mut self, up: bool) {
        let (mut prev, mut cur, mut next) = (None, None, None);
        let cursor = self.cursor;
        visual(&self.text[..self.len], |k, s, e, last| {
            if cur.is_none() && (s <= cursor && (cursor < e || (cursor == e && last))) {
                cur = Some((k, s, e));
            } else if cur.is_none() {
                prev = Some((s, e));
            } else if next.is_none() {
                next = Some((s, e));
            }
        });
        let Some((_, s, _)) = cur else { return };
        let col = cursor - s;
        if let Some((s2, e2)) = if up { prev } else { next } {
            self.cursor = (s2 + col).min(e2);
        } else if up {
            self.cursor = 0;
        } else {
            self.cursor = self.len;
        }
    }

    /// Colours every byte of the text (`roles`).
    fn colour(&mut self) {
        let lang = self.lang;
        let t = &self.text[..self.len];
        let r = &mut self.roles[..self.len];
        r.fill(TEXT);
        if lang == Lang::Plain {
            return;
        }
        let comment = lang.comment();
        let mut i = 0;
        let mut line_start = true;
        while i < t.len() {
            let c = t[i];
            if c == b'\n' {
                line_start = true;
                i += 1;
                continue;
            }
            let start_of_line = line_start;
            line_start = false;
            let eol = t[i..]
                .iter()
                .position(|&b| b == b'\n')
                .map_or(t.len(), |p| i + p);
            if lang == Lang::Markdown {
                if start_of_line && c == b'#' {
                    r[i..eol].fill(KEYWORD);
                    i = eol;
                } else if start_of_line && (c == b'-' || c == b'*') && t.get(i + 1) == Some(&b' ') {
                    r[i] = DIM;
                    i += 1;
                } else if c == b'`' {
                    let end = t[i + 1..eol]
                        .iter()
                        .position(|&b| b == b'`')
                        .map_or(eol, |p| i + p + 2);
                    r[i..end].fill(STRING);
                    i = end;
                } else {
                    i += 1;
                }
                continue;
            }
            if !comment.is_empty() && t[i..].starts_with(comment) {
                r[i..eol].fill(COMMENT);
                i = eol;
            } else if matches!(lang, Lang::Rust | Lang::C | Lang::Js) && t[i..].starts_with(b"/*") {
                let end = t[i + 2..]
                    .windows(2)
                    .position(|w| w == b"*/")
                    .map_or(t.len(), |p| i + p + 4);
                r[i..end].fill(COMMENT);
                i = end;
            } else if c == b'"' || (c == b'\'' && lang != Lang::Rust) {
                let mut j = i + 1;
                while j < eol && t[j] != c {
                    j += if t[j] == b'\\' { 2 } else { 1 };
                }
                let end = (j + 1).min(eol);
                r[i..end].fill(STRING);
                i = end;
            } else if (lang == Lang::Rust
                && c == b'#'
                && t.get(i + 1).is_some_and(|&b| b == b'[' || b == b'!'))
                || (lang == Lang::C && start_of_line && c == b'#')
            {
                let end = if lang == Lang::C {
                    eol
                } else {
                    t[i..eol]
                        .iter()
                        .position(|&b| b == b']')
                        .map_or(eol, |p| i + p + 1)
                };
                r[i..end].fill(MACRO);
                i = end;
            } else if lang == Lang::Toml && start_of_line && c == b'[' {
                r[i..eol].fill(KEYWORD);
                i = eol;
            } else if c.is_ascii_digit() {
                let mut j = i;
                while j < t.len() && (t[j].is_ascii_alphanumeric() || t[j] == b'_' || t[j] == b'.')
                {
                    j += 1;
                }
                r[i..j].fill(NUMBER);
                i = j;
            } else if c.is_ascii_alphabetic() || c == b'_' {
                let mut j = i;
                while j < t.len() && (t[j].is_ascii_alphanumeric() || t[j] == b'_') {
                    j += 1;
                }
                let word = core::str::from_utf8(&t[i..j]).unwrap_or("");
                let role = if lang.keywords().contains(&word) {
                    KEYWORD
                } else if lang == Lang::Rust && t.get(j) == Some(&b'!') {
                    j += 1;
                    MACRO
                } else if t.get(j) == Some(&b'(') {
                    FUNCTION
                } else if (matches!(lang, Lang::Rust | Lang::C | Lang::Js)
                    && c.is_ascii_uppercase())
                    || (lang == Lang::Rust && PRIMITIVES.contains(&word))
                {
                    TYPE
                } else if lang == Lang::Toml
                    && t[j..eol].iter().find(|&&b| b != b' ') == Some(&b'=')
                {
                    FUNCTION
                } else {
                    TEXT
                };
                r[i..j].fill(role);
                i = j;
            } else {
                i += 1;
            }
        }
    }

    fn draw(&mut self, win: &Window) -> Result<(), Error> {
        let name = self.path().rsplit('/').next().unwrap_or(DEFAULT);
        let mut title = Line::new();
        let _ = title.write_str(if self.path() == DEFAULT {
            "Notes"
        } else {
            name
        });
        win.title(title.as_str())?;
        let mut head = Line::new();
        let _ = write!(
            head,
            "{} ({})",
            self.path(),
            if self.editing { "editing" } else { "saved" }
        );
        win.line(0, head.as_str())?;
        self.colour();
        // Keep the cursor's line in view.
        let room = MAX_LINES - 1;
        let cursor = self.cursor;
        let mut at = 0;
        let mut total = 0;
        visual(&self.text[..self.len], |k, s, e, last| {
            if s <= cursor && (cursor < e || (cursor == e && last)) {
                at = k;
            }
            total = k + 1;
        });
        if at < self.top {
            self.top = at;
        } else if at >= self.top + room {
            self.top = at + 1 - room;
        }
        self.top = self.top.min(total.saturating_sub(1));
        let (top, text, roles) = (self.top, &self.text[..self.len], &self.roles[..self.len]);
        let mut shown = 0;
        let mut result = Ok(());
        visual(text, |k, s, e, last| {
            if k < top || k >= top + room || result.is_err() {
                return;
            }
            let mut l = Line::new();
            let mut role = TEXT;
            for j in s..e {
                if j == cursor {
                    let _ = l.write_char(mark::CARET);
                }
                if roles[j] != role {
                    role = roles[j];
                    let _ = write!(l, "{}{}", mark::COLOUR, role as char);
                }
                let b = text[j];
                if b == 13 {
                    continue;
                }
                let _ = l.write_char(match b {
                    b'\t' => ' ',
                    0x20..=0x7E => b as char,
                    _ => '?',
                });
            }
            if cursor == e && last {
                let _ = l.write_char(mark::CARET);
            }
            result = win.line(1 + shown, l.as_str());
            shown += 1;
        });
        result?;
        win.lines(1 + shown)?;
        win.select(None)?;
        win.show()
    }
}

/// Calls `f(k, start, end, last)` for each visual line `k`: the text
/// split at newlines (a CR before LF left out) and wrapped at `WRAP`;
/// `last` is true for a logical line's last piece.
fn visual(text: &[u8], mut f: impl FnMut(usize, usize, usize, bool)) {
    let mut k = 0;
    let mut s = 0;
    loop {
        let nl = text[s..].iter().position(|&b| b == b'\n').map(|p| s + p);
        let end = nl.unwrap_or(text.len());
        let shown_end = if end > s && text[end - 1] == b'\r' {
            end - 1
        } else {
            end
        };
        let mut a = s;
        loop {
            let b = (a + WRAP).min(shown_end);
            let last = b == shown_end;
            // A cursor on the hidden CR shows at the line's end.
            f(k, a, if last { end.max(b) } else { b }, last);
            k += 1;
            a = b;
            if last {
                break;
            }
        }
        match nl {
            Some(p) => s = p + 1,
            None => return,
        }
    }
}
