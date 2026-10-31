//! Notes: a small text editor for one file, `Documents/notes.txt` in the
//! home folder (docs/d4-design.md). Left and Right move the cursor
//! (docs/d10-design.md); typing, Enter and Backspace work at the cursor.
//! Changes are saved a second after typing stops, and when the window
//! closes.

#![no_std]
#![no_main]

use core::fmt::Write as _;
use zoo_abi::{MAX_MESSAGE, WAIT_FOREVER};
use zoo_rt::app::{Key, MAX_LINES, Window};
use zoo_rt::{Error, Line, file_create, file_read, file_truncate, file_write, try_recv, wait_any};

zoo_rt::entry2!(main);

const PATH: &str = "Documents/notes.txt";
/// Largest file Notes edits.
const CAPACITY: usize = 4096;
/// Characters per shown line; longer lines wrap.
const WRAP: usize = 72;
/// Ticks after the last change before saving.
const SAVE_AFTER: u64 = 100;

fn main(channel: u64, home: u64) -> u8 {
    let win = Window::new(channel);
    let mut text = [0u8; CAPACITY];
    let mut len = 0;
    while len < CAPACITY {
        match file_read(home, PATH, len as u64, &mut text[len..]) {
            Ok(0) | Err(Error::NotFound) => break,
            Ok(n) => len += n,
            Err(e) => {
                zoo_rt::log!("Notes: cannot read {}: {}", PATH, e.name());
                return 3;
            }
        }
    }
    let mut editing = false;
    // Cursor: a byte offset into the text (it starts at the end).
    let mut cursor = len;
    let mut msg = [0u8; MAX_MESSAGE];
    loop {
        if draw(&win, &text[..len], cursor, editing).is_err() {
            return 0; // the window server went away
        }
        let timeout = if editing { SAVE_AFTER } else { WAIT_FOREVER };
        match wait_any(&[channel], timeout) {
            Ok(_) => {}
            Err(Error::TimedOut) => {
                editing = !save(home, &text[..len]);
                continue;
            }
            Err(_) => return 4,
        }
        loop {
            match try_recv(channel, &mut msg) {
                Ok(n) => match Key::parse(&msg[..n]) {
                    Some(Key::Char(c)) if c.is_ascii() && !c.is_ascii_control() => {
                        if len < CAPACITY {
                            insert(&mut text, &mut len, &mut cursor, c as u8);
                            editing = true;
                        }
                    }
                    Some(Key::Enter) if len < CAPACITY => {
                        insert(&mut text, &mut len, &mut cursor, b'\n');
                        editing = true;
                    }
                    Some(Key::Back) if cursor > 0 => {
                        text.copy_within(cursor..len, cursor - 1);
                        len -= 1;
                        cursor -= 1;
                        editing = true;
                    }
                    Some(Key::Left) => cursor = cursor.saturating_sub(1),
                    Some(Key::Right) => cursor = (cursor + 1).min(len),
                    _ => {}
                },
                Err(Error::Again) => break,
                Err(Error::PeerClosed) => {
                    // Closed: keep what was typed.
                    if editing {
                        save(home, &text[..len]);
                    }
                    return 0;
                }
                Err(_) => return 4,
            }
        }
    }
}

/// Inserts `b` at the cursor (the caller checked there is room).
fn insert(text: &mut [u8; CAPACITY], len: &mut usize, cursor: &mut usize, b: u8) {
    text.copy_within(*cursor..*len, *cursor + 1);
    text[*cursor] = b;
    *len += 1;
    *cursor += 1;
}

/// Rewrites the file; true on success.
fn save(home: u64, text: &[u8]) -> bool {
    let result = match file_truncate(home, PATH, 0) {
        Err(Error::NotFound) => file_create(home, PATH),
        other => other,
    }
    .and_then(|()| file_write(home, PATH, 0, text));
    match result {
        Ok(_) => {
            zoo_rt::log!("Notes: saved {} bytes", text.len());
            true
        }
        Err(e) => {
            zoo_rt::log!("Notes: cannot save {}: {}", PATH, e.name());
            false
        }
    }
}

/// Visual lines: the text split at newlines (LF or CRLF) and wrapped at
/// `WRAP`.
fn visual_lines(text: &[u8]) -> impl Iterator<Item = &[u8]> {
    text.split(|&b| b == b'\n')
        .map(|l| l.strip_suffix(b"\r").unwrap_or(l))
        .flat_map(|l| {
            let pieces = l.len().div_ceil(WRAP).max(1);
            (0..pieces).map(move |k| &l[k * WRAP..((k + 1) * WRAP).min(l.len())])
        })
}

fn draw(win: &Window, text: &[u8], cursor: usize, editing: bool) -> Result<(), Error> {
    win.title("Notes")?;
    let mut head = Line::new();
    let _ = write!(
        head,
        "{} ({})",
        PATH,
        if editing { "editing" } else { "saved" }
    );
    win.line(0, head.as_str())?;
    // The text with the cursor drawn in it: `_` at the end, `|` inside.
    let mut shown_text = [0u8; CAPACITY + 1];
    let n = text.len();
    shown_text[..cursor].copy_from_slice(&text[..cursor]);
    shown_text[cursor] = if cursor == n { b'_' } else { b'|' };
    shown_text[cursor + 1..=n].copy_from_slice(&text[cursor..]);
    let text = &shown_text[..=n];
    // The last lines that fit under the heading.
    let total = visual_lines(text).count();
    let room = MAX_LINES - 1;
    let skip = total.saturating_sub(room);
    let mut shown = 0;
    for (k, l) in visual_lines(text).skip(skip).enumerate() {
        let mut line = Line::new();
        let _ = line.write_str(core::str::from_utf8(l).unwrap_or("?"));
        win.line(1 + k, line.as_str())?;
        shown = k + 1;
    }
    win.lines(1 + shown)?;
    win.select(None)?;
    win.show()
}
