//! Files: browses the home folder (docs/d7-design.md). Folders open with
//! Enter, text files show their first lines; Backspace goes back up.
//! The first line shows where you are (`Home/Documents`).

#![no_std]
#![no_main]

use core::fmt::Write as _;
use zoo_abi::{DirEntry, MAX_DIR_NAME, MAX_MESSAGE};
use zoo_rt::app::{Key, MAX_LINES, Window};
use zoo_rt::{Error, Line, dir_list, file_read, recv};

zoo_rt::entry2!(main);

/// Entries shown per folder (one line is the location).
const MAX_ENTRIES: usize = MAX_LINES - 1;
/// Deepest folder path kept, in bytes.
const MAX_PATH: usize = 200;
/// Bytes of a file shown as a preview.
const PREVIEW: usize = 1024;

struct Entry {
    name: [u8; MAX_DIR_NAME],
    len: usize,
    dir: bool,
    size: u64,
}

impl Entry {
    fn name(&self) -> &str {
        core::str::from_utf8(&self.name[..self.len]).unwrap_or("?")
    }
}

/// The folder being shown, relative to home (`""` is home itself).
struct Place {
    path: [u8; MAX_PATH],
    len: usize,
}

impl Place {
    fn as_str(&self) -> &str {
        core::str::from_utf8(&self.path[..self.len]).unwrap_or("")
    }

    /// `path/name`, or false if it does not fit.
    fn enter(&mut self, name: &str) -> bool {
        let extra = name.len() + usize::from(self.len > 0);
        if self.len + extra > MAX_PATH {
            return false;
        }
        if self.len > 0 {
            self.path[self.len] = b'/';
            self.len += 1;
        }
        self.path[self.len..self.len + name.len()].copy_from_slice(name.as_bytes());
        self.len += name.len();
        true
    }

    fn up(&mut self) {
        self.len = self.path[..self.len]
            .iter()
            .rposition(|&b| b == b'/')
            .unwrap_or(0);
    }
}

fn main(channel: u64, home: u64) -> u8 {
    let win = Window::new(channel);
    let mut place = Place {
        path: [0; MAX_PATH],
        len: 0,
    };
    let mut entries: [Option<Entry>; MAX_ENTRIES] = [const { None }; MAX_ENTRIES];
    let mut count = list(home, place.as_str(), &mut entries);
    let mut selected = 0usize;
    // The file being previewed (an index into `entries`).
    let mut preview: Option<usize> = None;
    let mut buf = [0u8; MAX_MESSAGE];
    loop {
        let shown = match preview {
            Some(i) => draw_preview(&win, home, &place, &entries[i]),
            None => draw_list(&win, &place, &entries[..count], selected),
        };
        if shown.is_err() {
            return 0; // the window server went away
        }
        let n = match recv(channel, &mut buf) {
            Ok(n) => n,
            Err(Error::PeerClosed) => return 0,
            Err(_) => return 4,
        };
        match (Key::parse(&buf[..n]), preview) {
            (Some(Key::Back), Some(_)) => preview = None,
            (_, Some(_)) => {}
            (Some(Key::Up), None) => selected = selected.saturating_sub(1),
            (Some(Key::Down), None) if selected + 1 < count => selected += 1,
            (Some(Key::Enter), None) => {
                if let Some(Some(e)) = entries.get(selected) {
                    if !e.dir {
                        preview = Some(selected);
                    } else if place.enter(e.name()) {
                        count = list(home, place.as_str(), &mut entries);
                        selected = 0;
                    }
                }
            }
            (Some(Key::Back), None) if place.len > 0 => {
                // Back up, with the folder we came from selected.
                let from = Place {
                    path: place.path,
                    len: place.len,
                };
                place.up();
                count = list(home, place.as_str(), &mut entries);
                let name = &from.as_str()[place.len + usize::from(place.len > 0)..];
                selected = entries[..count]
                    .iter()
                    .position(|e| e.as_ref().is_some_and(|e| e.name() == name))
                    .unwrap_or(0);
            }
            _ => {}
        }
    }
}

/// Fills `entries` with the folder at `path`; returns how many.
fn list(home: u64, path: &str, entries: &mut [Option<Entry>]) -> usize {
    let mut count = 0;
    while count < entries.len() {
        let e: DirEntry = match dir_list(home, path, count as u64) {
            Ok(e) => e,
            Err(Error::NotFound) => break,
            Err(e) => {
                zoo_rt::log!("Files: cannot list `{}`: {}", path, e.name());
                break;
            }
        };
        let len = (e.name_len as usize).min(MAX_DIR_NAME);
        entries[count] = Some(Entry {
            name: e.name,
            len,
            dir: e.is_dir(),
            size: e.size,
        });
        count += 1;
    }
    for e in &mut entries[count..] {
        *e = None;
    }
    count
}

fn location(place: &Place, file: Option<&str>) -> Line {
    let mut l = Line::new();
    let _ = l.write_str("Home");
    if place.len > 0 {
        let _ = write!(l, "/{}", place.as_str());
    }
    if let Some(f) = file {
        let _ = write!(l, "/{}", f);
    }
    l
}

fn draw_list(
    win: &Window,
    place: &Place,
    entries: &[Option<Entry>],
    selected: usize,
) -> Result<(), Error> {
    win.title("Files")?;
    win.lines(1 + entries.len().max(1))?;
    win.line(0, location(place, None).as_str())?;
    if entries.is_empty() {
        win.line(1, "(empty folder)")?;
        win.select(None)?;
        return win.show();
    }
    for (i, e) in entries.iter().enumerate() {
        if let Some(e) = e {
            let mut l = Line::new();
            if e.dir {
                let _ = write!(l, "{}/", e.name());
            } else {
                let _ = write!(l, "{} ({} bytes)", e.name(), e.size);
            }
            win.line(1 + i, l.as_str())?;
        }
    }
    win.select(Some(1 + selected))?;
    win.show()
}

/// The first lines of a text file, under its location.
fn draw_preview(
    win: &Window,
    home: u64,
    place: &Place,
    entry: &Option<Entry>,
) -> Result<(), Error> {
    win.title("Files")?;
    let Some(e) = entry else {
        return win.show();
    };
    let mut path = Place {
        path: place.path,
        len: place.len,
    };
    let mut text = [0u8; PREVIEW];
    let n = if path.enter(e.name()) {
        file_read(home, path.as_str(), 0, &mut text).unwrap_or(0)
    } else {
        0
    };
    win.line(0, location(place, Some(e.name())).as_str())?;
    let mut shown = 1;
    for line in text[..n].split(|&b| b == b'\n') {
        if shown == MAX_LINES - 1 {
            break;
        }
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        let mut l = Line::new();
        let _ = l.write_str(core::str::from_utf8(line).unwrap_or("(not text)"));
        win.line(shown, l.as_str())?;
        shown += 1;
    }
    win.line(shown, "Backspace: back to the folder")?;
    win.lines(shown + 1)?;
    win.select(None)?;
    win.show()
}
