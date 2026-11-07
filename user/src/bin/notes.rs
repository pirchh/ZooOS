//! Notes: a text editor (docs/d4-design.md, docs/u3f-design.md). It opens
//! `Documents/notes.txt` in the home folder, or the file Files asks it to
//! (`open:<path>`). Arrow keys move the cursor (docs/d10-design.md);
//! typing, Enter and Backspace work at the cursor. Program text is
//! coloured by its kind (by the file's extension). Changes are saved a
//! second after typing stops, at Ctrl+S, and when the window closes.
//!
//! A file is edited only when it was read whole (docs/s0-design.md, B1):
//! one over `CAPACITY`, or one that failed to read part-way, opens
//! read-only with the reason in its header, and is never saved.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use core::fmt::Write as _;
use zoo_abi::ui::mark;
use zoo_abi::{MAX_MESSAGE, WAIT_FOREVER};
use zoo_rt::app::{Event, Key, MAX_LINES, Window};
use zoo_rt::{
    Error, Line, dir_create, dir_list, file_create, file_read, file_truncate, file_write, try_recv,
    wait_any,
};

zoo_rt::entry2!(main);

const DEFAULT: &str = "Documents/notes.txt";
/// Largest file Notes edits; larger ones open read-only.
const CAPACITY: usize = 1 << 20;
/// Bytes read per request.
const CHUNK: usize = 4096;
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
    text: Vec<u8>,
    len: usize,
    /// Why the file cannot be edited, if it cannot: then it is never saved.
    read_only: Option<&'static str>,
    /// A byte offset into the text.
    cursor: usize,
    /// The first visual line shown.
    top: usize,
    editing: bool,
    /// The colour role of each byte (`colour`).
    roles: Vec<u8>,
}

fn main(channel: u64, home: u64) -> u8 {
    let win = Window::new(channel);
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
                    if let Some(Event::Do(action, text)) = Event::parse(&msg[..n]) {
                        // Bella's actions (docs/registry-design.md), answered
                        // with a status, her request id, and for several
                        // matching lines the lines to choose from
                        // (docs/targets-design.md).
                        use zoo_infer::registry::{Outcome, Status, split_arg};
                        let (slots, id, target) = split_arg(text);
                        // A titled note (docs/titled-design.md): opened
                        // first, made when written to.
                        let opened = match zoo_infer::registry::note_of(text) {
                            Some(title) => ed.open_note(title, action == "write"),
                            None => Ok(()),
                        };
                        let outcome = if let Err(said) = opened {
                            Outcome {
                                status: Status::Err,
                                id: None,
                                said,
                                choices: alloc::vec::Vec::new(),
                            }
                        } else if action == "write" {
                            let said = ed.append_line(slots.first().copied().unwrap_or(""));
                            let status = if said.starts_with("Error") {
                                Status::Err
                            } else {
                                Status::Ok
                            };
                            Outcome {
                                status,
                                id: None,
                                said,
                                choices: alloc::vec::Vec::new(),
                            }
                        } else if action == "remove" || action == "change" {
                            ed.edit(action, &slots, target)
                        } else {
                            Outcome {
                                status: Status::Err,
                                id: None,
                                said: alloc::format!("Error: Notes cannot {}.", action),
                                choices: alloc::vec::Vec::new(),
                            }
                        };
                        let outcome = Outcome {
                            id: id.map(alloc::string::String::from),
                            ..outcome
                        };
                        let _ = win.did(&outcome.text());
                    } else if let Some(Event::Open(path)) = Event::parse(&msg[..n]) {
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
        text: Vec::new(),
        len: 0,
        read_only: None,
        cursor: 0,
        top: 0,
        editing: false,
        roles: Vec::new(),
    }))
}

impl Editor {
    /// Opens the titled note `said` names, in `zoo_infer::notes::FOLDER`;
    /// with `make`, a new one when none does. The sentence for Bella when
    /// it can't.
    fn open_note(&mut self, said: &str, make: bool) -> Result<(), alloc::string::String> {
        use zoo_infer::notes::{FOLDER, NotePick, new_title, pick_note};
        let mut titles: Vec<alloc::string::String> = Vec::new();
        let mut index = 0u64;
        while titles.len() < 64 {
            let Ok(e) = dir_list(self.home, FOLDER, index) else {
                break;
            };
            index += 1;
            let name = &e.name[..(e.name_len as usize).min(e.name.len())];
            if let Ok(name) = core::str::from_utf8(name)
                && let Some(title) = name.strip_suffix(".txt")
                && !e.is_dir()
                && !name.starts_with('.')
            {
                titles.push(alloc::string::String::from(title));
            }
        }
        let refs: Vec<&str> = titles.iter().map(alloc::string::String::as_str).collect();
        let title = match pick_note(&refs, said) {
            NotePick::One(i) => titles[i].clone(),
            NotePick::Several(found) => {
                let names: Vec<&str> = found.iter().map(|&i| refs[i]).collect();
                return Err(alloc::format!(
                    "Error: which note, {}? Say its whole title.",
                    names.join(" or ")
                ));
            }
            NotePick::NoNote if make => {
                let Some(t) = new_title(said) else {
                    return Err(alloc::format!(
                        "Error: \"{}\" can't be a note's title.",
                        said
                    ));
                };
                match dir_create(self.home, FOLDER) {
                    Ok(()) | Err(Error::Exists) => {}
                    Err(e) => {
                        return Err(alloc::format!(
                            "Error: cannot make {}: {}.",
                            FOLDER,
                            e.name()
                        ));
                    }
                }
                zoo_rt::log!("Notes: a new note, {}", t);
                t
            }
            NotePick::NoNote => {
                return Err(alloc::format!(
                    "Error: there is no note called \"{}\".",
                    said
                ));
            }
        };
        let path = alloc::format!("{}/{}.txt", FOLDER, title);
        if path != self.path() {
            if self.editing {
                self.save();
                self.editing = false;
            }
            if !self.load(&path) {
                return Err(alloc::format!("Error: cannot open the note {}.", title));
            }
        }
        Ok(())
    }

    fn path(&self) -> &str {
        core::str::from_utf8(&self.path[..self.path_len]).unwrap_or(DEFAULT)
    }

    /// Reads `path` (a missing file is empty); false if it cannot be read
    /// at all. A file read only in part is shown read-only.
    fn load(&mut self, path: &str) -> bool {
        let k = path.len().min(MAX_PATH);
        self.path[..k].copy_from_slice(&path.as_bytes()[..k]);
        self.path_len = k;
        self.lang = Lang::of(path);
        self.text.clear();
        self.read_only = None;
        let mut ok = true;
        // One byte past `CAPACITY` tells a file that is too large.
        while self.text.len() <= CAPACITY {
            let at = self.text.len();
            let want = CHUNK.min(CAPACITY + 1 - at);
            self.text.resize(at + want, 0);
            let path = core::str::from_utf8(&self.path[..self.path_len]).unwrap_or(DEFAULT);
            match file_read(self.home, path, at as u64, &mut self.text[at..]) {
                Ok(0) | Err(Error::NotFound) => {
                    self.text.truncate(at);
                    break;
                }
                Ok(n) => self.text.truncate(at + n),
                Err(e) => {
                    self.text.truncate(at);
                    zoo_rt::log!("Notes: cannot read {}: {}", path, e.name());
                    self.read_only = Some("could not be read whole");
                    ok = at > 0;
                    break;
                }
            }
        }
        if self.text.len() > CAPACITY {
            self.text.truncate(CAPACITY);
            self.read_only = Some("over 1 MiB, too large to edit");
        }
        self.len = self.text.len();
        self.roles.resize(self.len, TEXT);
        if let Some(why) = self.read_only {
            zoo_rt::log!("Notes: {} is read-only: {}", self.path(), why);
        }
        // It starts at the end, as before.
        self.cursor = self.len;
        self.top = 0;
        self.editing = false;
        zoo_rt::log!("Notes: opened {} ({} bytes)", self.path(), self.len);
        ok
    }

    /// Rewrites the file; true on success. A read-only file is never
    /// written: what is shown may be only part of it.
    fn save(&self) -> bool {
        let path = self.path();
        if let Some(why) = self.read_only {
            zoo_rt::log!("Notes: not saving {}: {}", path, why);
            return false;
        }
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

    /// `text` as a line of its own at the end, saved at once; what it did,
    /// for the asker (the `write` action, docs/actions-design.md).
    fn append_line(&mut self, text: &str) -> alloc::string::String {
        if let Some(why) = self.read_only {
            return alloc::format!("Error: {} cannot be changed ({}).", self.path(), why);
        }
        let text = text.trim();
        if self.len + text.len() + 2 > CAPACITY {
            return alloc::format!("Error: {} is full.", self.path());
        }
        self.cursor = self.len;
        if self.len > 0 && self.text[self.len - 1] != b'\n' {
            self.insert(b'\n');
        }
        for b in text
            .bytes()
            .filter(|b| *b == b'\t' || (0x20..0x7F).contains(b))
        {
            self.insert(b);
        }
        self.insert(b'\n');
        self.editing = !self.save();
        zoo_rt::log!("Notes: wrote a line in {}", self.path());
        if self.editing {
            alloc::format!("Wrote it in {}, but saving failed.", self.path())
        } else {
            alloc::format!("Wrote \"{}\" in {}.", text, self.path())
        }
    }

    /// Bella's `remove` and `change` (docs/registry-design.md §4): the
    /// edit made on the whole text, saved at once; what it did, for her.
    fn edit(
        &mut self,
        action: &str,
        slots: &[&str],
        target: Option<&str>,
    ) -> zoo_infer::registry::Outcome {
        use zoo_infer::notes::{Edit, change, remove};
        use zoo_infer::registry::{Outcome, Status};
        let out = |status, said: alloc::string::String, choices| Outcome {
            status,
            id: None,
            said,
            choices,
        };
        if let Some(why) = self.read_only {
            return out(
                Status::Err,
                alloc::format!("Error: {} cannot be changed ({}).", self.path(), why),
                alloc::vec::Vec::new(),
            );
        }
        let text = alloc::string::String::from_utf8_lossy(&self.text[..self.len]).into_owned();
        let what = slots.first().copied().unwrap_or("");
        let done = match (action, slots) {
            ("remove", _) => remove(&text, what, target),
            ("change", [old, new]) => change(&text, old, new, target),
            _ => Edit::NotFound,
        };
        let (new_text, said) = match done {
            Edit::Done { text, said } => (text, said),
            Edit::Choose(lines) => {
                zoo_rt::log!(
                    "Notes: \"{}\" is in {} lines; asking which",
                    what,
                    lines.len()
                );
                return out(
                    Status::Choose,
                    alloc::format!("\"{}\" is in {} lines. Which one?", what, lines.len()),
                    lines,
                );
            }
            Edit::NotFound => {
                return out(
                    Status::Err,
                    alloc::format!("Error: \"{}\" is not in {}.", what, self.path()),
                    alloc::vec::Vec::new(),
                );
            }
        };
        if new_text.len() > CAPACITY {
            return out(
                Status::Err,
                alloc::format!("Error: {} is full.", self.path()),
                alloc::vec::Vec::new(),
            );
        }
        self.text.clear();
        self.text.extend_from_slice(new_text.as_bytes());
        self.len = self.text.len();
        self.roles.clear();
        self.roles.resize(self.len, TEXT);
        self.cursor = self.cursor.min(self.len);
        self.editing = !self.save();
        zoo_rt::log!("Notes: {} in {}", action, self.path());
        if self.editing {
            out(
                Status::Err,
                alloc::format!("{} {}, but saving failed.", said, self.path()),
                alloc::vec::Vec::new(),
            )
        } else {
            out(
                Status::Ok,
                alloc::format!("{} {}.", said, self.path()),
                alloc::vec::Vec::new(),
            )
        }
    }

    fn insert(&mut self, b: u8) {
        if self.len >= CAPACITY || self.read_only.is_some() {
            return;
        }
        self.text.insert(self.cursor, b);
        self.roles.push(TEXT);
        self.len += 1;
        self.cursor += 1;
        self.editing = true;
    }

    fn key(&mut self, key: Key) {
        match key {
            Key::Char(c) if c.is_ascii() && !c.is_ascii_control() => self.insert(c as u8),
            Key::Enter => self.insert(b'\n'),
            Key::Back if self.cursor > 0 && self.read_only.is_none() => {
                self.text.remove(self.cursor - 1);
                self.roles.pop();
                self.len -= 1;
                self.cursor -= 1;
                self.editing = true;
            }
            Key::Left => self.cursor = self.cursor.saturating_sub(1),
            Key::Right => self.cursor = (self.cursor + 1).min(self.len),
            Key::Up | Key::Down => self.vertical(key == Key::Up),
            Key::Ctrl('s') if self.read_only.is_none() => self.editing = !self.save(),
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
        let _ = match self.read_only {
            Some(why) => write!(head, "{} (read-only: {})", self.path(), why),
            None => write!(
                head,
                "{} ({})",
                self.path(),
                if self.editing { "editing" } else { "saved" }
            ),
        };
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
            // At most `WRAP` characters, broken after the last space in
            // them when there is one (docs/u4b-design.md).
            let mut b = (a + WRAP).min(shown_end);
            if b < shown_end
                && let Some(sp) = text[a..b].iter().rposition(|&c| c == b' ')
                && sp > 0
            {
                b = a + sp + 1;
            }
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
