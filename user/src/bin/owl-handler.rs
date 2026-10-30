//! OwlHandler: the Files application (docs/m4-design.md §6). Lists the
//! initial filesystem; Enter shows a file's details, Backspace goes back.

#![no_std]
#![no_main]

use core::fmt::Write as _;
use zoo_rt::app::{Key, MAX_LINES, Window};
use zoo_rt::{Error, Line, fs_stat, recv};

zoo_rt::entry!(main);

struct File {
    name: [u8; zoo_abi::MAX_FS_NAME],
    name_len: usize,
    size: u64,
}

fn main(channel: u64) -> u8 {
    let win = Window::new(channel);
    let mut files: [Option<File>; MAX_LINES] = [const { None }; MAX_LINES];
    let mut count = 0;
    while count < MAX_LINES {
        match fs_stat(count as u64) {
            Ok(e) => {
                let n = (e.name_len as usize).min(e.name.len());
                files[count] = Some(File {
                    name: e.name,
                    name_len: n,
                    size: e.size,
                });
                count += 1;
            }
            Err(Error::NotFound) => break,
            Err(_) => return 3,
        }
    }

    let mut selected = 0usize;
    let mut details = false;
    let mut buf = [0u8; zoo_abi::MAX_MESSAGE];
    loop {
        if draw(&win, &files[..count], selected, details).is_err() {
            return 0; // the window server went away
        }
        let n = match recv(channel, &mut buf) {
            Ok(n) => n,
            Err(Error::PeerClosed) => return 0,
            Err(_) => return 4,
        };
        match Key::parse(&buf[..n]) {
            Some(Key::Up) if !details => selected = selected.saturating_sub(1),
            Some(Key::Down) if !details && selected + 1 < count => selected += 1,
            Some(Key::Enter) if count > 0 => details = true,
            Some(Key::Back) => details = false,
            _ => {}
        }
    }
}

fn name(f: &File) -> &str {
    core::str::from_utf8(&f.name[..f.name_len]).unwrap_or("?")
}

fn draw(win: &Window, files: &[Option<File>], selected: usize, details: bool) -> Result<(), Error> {
    win.title("Files")?;
    let mut l = Line::new();
    if details && let Some(Some(f)) = files.get(selected) {
        win.lines(5)?;
        let _ = write!(l, "Name: {}", name(f));
        win.line(0, l.as_str())?;
        let mut l = Line::new();
        let _ = write!(l, "Size: {} bytes", f.size);
        win.line(1, l.as_str())?;
        let mut l = Line::new();
        let _ = write!(
            l,
            "Entry {} of {} in the initial filesystem",
            selected + 1,
            files.len()
        );
        win.line(2, l.as_str())?;
        win.line(3, "")?;
        win.line(4, "Backspace: back to the list")?;
        win.select(None)?;
    } else {
        win.lines(files.len())?;
        for (i, f) in files.iter().enumerate() {
            if let Some(f) = f {
                let mut l = Line::new();
                let _ = write!(l, "{} ({} bytes)", name(f), f.size);
                win.line(i, l.as_str())?;
            }
        }
        win.select(Some(selected))?;
    }
    win.show()
}
