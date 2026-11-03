//! Files: browses the home folder (docs/d7-design.md), Finder-style
//! (docs/u3f-design.md): a sidebar of places, an icon and size per entry.
//! A double-click (or Ctrl+O) opens a folder, an application, or a file in
//! Notes; Enter shows a file's first lines; Backspace goes back up.
//! Right-click for menus: Open, Add to Dock, New Folder, New Text File.
//! The first line shows where you are (`Home/Documents`).

#![no_std]
#![no_main]

use core::fmt::Write as _;
use zoo_abi::ui::{APPLICATIONS, mark};
use zoo_abi::{DirEntry, MAX_DIR_NAME, MAX_MESSAGE};
use zoo_rt::app::{Event, Key, MAX_LINES, Window};
use zoo_rt::{Error, Line, dir_create, dir_list, file_create, file_read, recv, rename};
extern crate alloc;
use alloc::string::String;

zoo_rt::entry2!(main);

/// Entries shown per folder (one line is the location).
const MAX_ENTRIES: usize = MAX_LINES - 1;
/// Deepest folder path kept, in bytes.
const MAX_PATH: usize = 200;
/// Bytes of a file shown as a preview.
const PREVIEW: usize = 1024;

/// The Applications folder at the end of Home (docs/u3-design.md): not on
/// disk, it lists the applications.
const APPS_FOLDER: &str = "Applications";

/// The Trash (docs/u3h-design.md): a hidden folder in home, so moving
/// there is a rename, and nothing is lost until it is emptied.
const TRASH: &str = ".Trash";

/// The sidebar: (label, icon, place); the first is a heading.
const PLACES: [(&str, u8, &str); 7] = [
    ("Favourites", 0, ""),
    ("Home", b'H', ""),
    ("Documents", b'O', "Documents"),
    ("Music", b'M', "Music"),
    ("Photos", b'P', "Photos"),
    ("Applications", b'A', APPS_FOLDER),
    ("Trash", b'T', TRASH),
];

struct Entry {
    name: [u8; MAX_DIR_NAME],
    len: usize,
    dir: bool,
    size: u64,
    /// In the Applications folder: the application (an index into
    /// `APPLICATIONS`).
    app: Option<usize>,
}

impl Entry {
    fn name(&self) -> &str {
        core::str::from_utf8(&self.name[..self.len]).unwrap_or("?")
    }
}

/// The folder being shown, relative to home (`""` is home itself).
#[derive(Clone, Copy)]
struct Place {
    path: [u8; MAX_PATH],
    len: usize,
}

impl Place {
    const fn home() -> Place {
        Place {
            path: [0; MAX_PATH],
            len: 0,
        }
    }

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

/// What a context menu was opened on.
#[derive(Clone, Copy)]
enum Target {
    App(usize),
    Folder(usize),
    File(usize),
    Empty,
}

struct Browser {
    home: u64,
    place: Place,
    entries: [Option<Entry>; MAX_ENTRIES],
    count: usize,
    selected: usize,
    /// The file being previewed (an index into `entries`).
    preview: Option<usize>,
    menu: Option<Target>,
    /// Entry being renamed, and the name typed so far.
    renaming: Option<(usize, String)>,
}

fn main(channel: u64, home: u64) -> u8 {
    let win = Window::new(channel);
    let mut b = Browser {
        home,
        place: Place::home(),
        entries: [const { None }; MAX_ENTRIES],
        count: 0,
        selected: 0,
        preview: None,
        menu: None,
        renaming: None,
    };
    b.relist();
    let mut buf = [0u8; MAX_MESSAGE];
    loop {
        if b.draw(&win).is_err() {
            return 0; // the window server went away
        }
        let n = match recv(channel, &mut buf) {
            Ok(n) => n,
            Err(Error::PeerClosed) => return 0,
            Err(_) => return 4,
        };
        if let Some(ev) = Event::parse(&buf[..n]) {
            b.event(&win, ev);
            continue;
        }
        let Some(key) = Key::parse(&buf[..n]) else {
            continue;
        };
        if b.preview.is_some() {
            if key == Key::Back {
                b.preview = None;
            }
            continue;
        }
        if let Some((k, name)) = b.renaming.as_mut() {
            match key {
                Key::Char(c) if !c.is_control() && c != '/' && name.len() < MAX_DIR_NAME => {
                    name.push(c)
                }
                Key::Back => {
                    name.pop();
                }
                Key::Enter => {
                    let (k, name) = (*k, core::mem::take(name));
                    b.renaming = None;
                    b.rename_to(k, &name);
                }
                _ => {}
            }
            continue;
        }
        match key {
            Key::Up => b.selected = b.selected.saturating_sub(1),
            Key::Down if b.selected + 1 < b.count => b.selected += 1,
            // Enter on a file: its first lines (Quick Look); otherwise as
            // a double-click.
            Key::Enter => match b.entries.get(b.selected) {
                Some(Some(e)) if !e.dir && e.app.is_none() => b.preview = Some(b.selected),
                _ => b.open(&win, b.selected),
            },
            Key::Ctrl('o') => b.open(&win, b.selected),
            Key::Ctrl('N') => b.new_folder(),
            Key::Back if b.place.len > 0 => b.back(),
            _ => {}
        }
    }
}

impl Browser {
    fn relist(&mut self) {
        self.count = list(self.home, self.place.as_str(), &mut self.entries);
        self.selected = self.selected.min(self.count.saturating_sub(1));
    }

    fn go(&mut self, place: Place) {
        self.place = place;
        self.selected = 0;
        self.preview = None;
        self.relist();
        zoo_rt::log!("Files: showing Home/{}", self.place.as_str());
    }

    /// Shows `path` (from home): a folder is opened, a file is selected in
    /// its folder. How Bella's `open` reaches Files (docs/access-design.md).
    fn show(&mut self, path: &str) {
        let path = path.trim_matches('/');
        let is_dir = path.is_empty() || zoo_rt::dir_stat(self.home, path).is_ok_and(|e| e.is_dir());
        let (folder, name) = if is_dir {
            (path, "")
        } else {
            path.rsplit_once('/').unwrap_or(("", path))
        };
        let mut p = Place::home();
        for c in folder.split('/').filter(|c| !c.is_empty()) {
            if !p.enter(c) {
                return;
            }
        }
        self.go(p);
        if !name.is_empty() {
            self.selected = self.entries[..self.count]
                .iter()
                .position(|e| e.as_ref().is_some_and(|e| e.name() == name))
                .unwrap_or(0);
            zoo_rt::log!("Files: selected {}", name);
        }
    }

    /// Back up, with the folder we came from selected.
    fn back(&mut self) {
        let from = self.place;
        self.place.up();
        self.relist();
        let name = &from.as_str()[self.place.len + usize::from(self.place.len > 0)..];
        self.selected = self.entries[..self.count]
            .iter()
            .position(|e| e.as_ref().is_some_and(|e| e.name() == name))
            .unwrap_or(0);
    }

    /// Entry `k`'s path from home.
    fn path_of(&self, k: usize) -> Option<Place> {
        let e = self.entries.get(k)?.as_ref()?;
        let mut p = self.place;
        p.enter(e.name()).then_some(p)
    }

    /// Opens entry `k`: a folder, an application, or a file in Notes.
    fn open(&mut self, win: &Window, k: usize) {
        let Some(e) = self.entries.get(k).and_then(|e| e.as_ref()) else {
            return;
        };
        if let Some(a) = e.app {
            let _ = win.launch(APPLICATIONS[a].1);
        } else if e.dir {
            let mut p = self.place;
            if p.enter(e.name()) {
                self.go(p);
            }
        } else if let Some(p) = self.path_of(k) {
            zoo_rt::log!("Files: opening {} in Notes", p.as_str());
            let _ = win.open("notes", p.as_str());
        }
    }

    /// The first free `base`, `base 2`, ... in this folder.
    fn free_name(&self, base: &str, ext: &str) -> Line {
        for n in 1..100 {
            let mut l = Line::new();
            let _ = l.write_str(base);
            if n > 1 {
                let _ = write!(l, " {}", n);
            }
            let _ = l.write_str(ext);
            let taken = self.entries[..self.count]
                .iter()
                .any(|e| e.as_ref().is_some_and(|e| e.name() == l.as_str()));
            if !taken {
                return l;
            }
        }
        Line::new()
    }

    /// Selects entry `name` after a relist.
    fn select_named(&mut self, name: &str) {
        self.relist();
        if let Some(k) = self.entries[..self.count]
            .iter()
            .position(|e| e.as_ref().is_some_and(|e| e.name() == name))
        {
            self.selected = k;
        }
    }

    fn writable(&self) -> bool {
        self.place.as_str() != APPS_FOLDER && !self.in_trash()
    }

    fn in_trash(&self) -> bool {
        self.place.as_str() == TRASH
    }

    /// Starts renaming entry `k`: its line becomes an editable name.
    fn start_rename(&mut self, k: usize) {
        if let Some(Some(e)) = self.entries.get(k) {
            self.selected = k;
            zoo_rt::log!("Files: renaming {}", e.name());
            self.renaming = Some((k, String::from(e.name())));
        }
    }

    /// Renames entry `k` to `name` (in the same folder).
    fn rename_to(&mut self, k: usize, name: &str) {
        let name = name.trim();
        let Some(from) = self.path_of(k) else { return };
        if name.is_empty() || name.starts_with('.') {
            return;
        }
        let mut to = self.place;
        if !to.enter(name) || to.as_str() == from.as_str() {
            return;
        }
        match rename(self.home, from.as_str(), to.as_str()) {
            Ok(()) => {
                zoo_rt::log!("Files: renamed {} to {}", from.as_str(), to.as_str());
                self.select_named(name);
            }
            Err(e) => zoo_rt::log!("Files: cannot rename {}: {}", from.as_str(), e.name()),
        }
    }

    /// Moves entry `k` into the Trash (a free name there).
    fn move_to_trash(&mut self, k: usize) {
        let Some(from) = self.path_of(k) else { return };
        let Some(Some(e)) = self.entries.get(k) else {
            return;
        };
        let name = String::from(e.name());
        let _ = dir_create(self.home, TRASH);
        let mut to = Line::new();
        for n in 1..100 {
            to = Line::new();
            let _ = core::fmt::Write::write_fmt(&mut to, format_args!("{}/{}", TRASH, name));
            if n > 1 {
                let _ = core::fmt::Write::write_fmt(&mut to, format_args!(" {}", n));
            }
            if zoo_rt::dir_stat(self.home, to.as_str()).is_err() {
                break;
            }
        }
        match rename(self.home, from.as_str(), to.as_str()) {
            Ok(()) => {
                zoo_rt::log!("Files: moved {} to the Trash", from.as_str());
                self.relist();
            }
            Err(e) => zoo_rt::log!(
                "Files: cannot move {} to the Trash: {}",
                from.as_str(),
                e.name()
            ),
        }
    }

    /// Moves entry `k` of the Trash back to Home.
    fn put_back(&mut self, k: usize) {
        let Some(from) = self.path_of(k) else { return };
        let Some(Some(e)) = self.entries.get(k) else {
            return;
        };
        let name = String::from(e.name());
        match rename(self.home, from.as_str(), &name) {
            Ok(()) => {
                zoo_rt::log!("Files: put {} back in Home", name);
                self.relist();
            }
            Err(err) => zoo_rt::log!("Files: cannot put {} back: {}", name, err.name()),
        }
    }

    fn new_folder(&mut self) {
        if !self.writable() {
            return;
        }
        let name = self.free_name("untitled folder", "");
        let mut p = self.place;
        if !p.enter(name.as_str()) {
            return;
        }
        match dir_create(self.home, p.as_str()) {
            Ok(()) => {
                zoo_rt::log!("Files: created folder {}", p.as_str());
                self.select_named(name.as_str());
            }
            Err(e) => zoo_rt::log!("Files: cannot create {}: {}", p.as_str(), e.name()),
        }
    }

    fn new_text_file(&mut self, win: &Window) {
        if !self.writable() {
            return;
        }
        let name = self.free_name("untitled", ".txt");
        let mut p = self.place;
        if !p.enter(name.as_str()) {
            return;
        }
        match file_create(self.home, p.as_str()) {
            Ok(()) => {
                zoo_rt::log!("Files: created file {}", p.as_str());
                self.select_named(name.as_str());
                let _ = win.open("notes", p.as_str());
            }
            Err(e) => zoo_rt::log!("Files: cannot create {}: {}", p.as_str(), e.name()),
        }
    }

    fn event(&mut self, win: &Window, ev: Event) {
        // Content line `l` is entry `l - 1` (line 0 is the location).
        let count = self.count;
        let entry = |l: usize| l.checked_sub(1).filter(|&k| k < count);
        match ev {
            Event::Open(path) => self.show(path),
            Event::SideClick(k) => {
                if let Some(&(_, icon, path)) = PLACES.get(k)
                    && icon != 0
                {
                    let mut p = Place::home();
                    if !path.is_empty() {
                        p.enter(path);
                    }
                    self.go(p);
                }
            }
            _ if self.preview.is_some() => {}
            Event::Click(l) => {
                if let Some(k) = entry(l) {
                    self.selected = k;
                }
            }
            Event::DoubleClick(l) => {
                if let Some(k) = entry(l) {
                    self.selected = k;
                    self.open(win, k);
                }
            }
            Event::RightClick(l) => {
                let Some(k) = entry(l) else { return };
                let Some(e) = self.entries[k].as_ref() else {
                    return;
                };
                self.selected = k;
                let (target, items): (Target, &[&str]) = match (e.app, e.dir) {
                    (Some(a), _) => (Target::App(a), &["Open", "Add to Dock"]),
                    (None, true) if self.in_trash() => (Target::Folder(k), &["Put Back"]),
                    (None, false) if self.in_trash() => (Target::File(k), &["Put Back"]),
                    (None, true) if e.name() == APPS_FOLDER => (Target::Folder(k), &["Open"]),
                    (None, true) => (
                        Target::Folder(k),
                        &["Open", "New Folder", "Rename", "Move to Trash"],
                    ),
                    (None, false) => (
                        Target::File(k),
                        &["Open in Notes", "Quick Look", "Rename", "Move to Trash"],
                    ),
                };
                self.menu = Some(target);
                let _ = win.menu(items);
            }
            Event::RightClickEmpty => {
                if self.writable() {
                    self.menu = Some(Target::Empty);
                    let _ = win.menu(&["New Folder", "New Text File"]);
                }
            }
            Event::MenuChoose(choice) => match (self.menu.take(), choice) {
                (Some(Target::App(a)), 0) => {
                    let _ = win.launch(APPLICATIONS[a].1);
                }
                (Some(Target::App(a)), _) => {
                    let _ = win.dock_add(APPLICATIONS[a].1);
                }
                (Some(Target::Folder(k) | Target::File(k)), 0) if self.in_trash() => {
                    self.put_back(k)
                }
                (Some(Target::Folder(k) | Target::File(k)), 0) => self.open(win, k),
                (Some(Target::Folder(k) | Target::File(k)), 2) => self.start_rename(k),
                (Some(Target::Folder(k) | Target::File(k)), 3) => self.move_to_trash(k),
                (Some(Target::Folder(_)), _) | (Some(Target::Empty), 0) => self.new_folder(),
                (Some(Target::File(k)), _) => self.preview = Some(k),
                (Some(Target::Empty), _) => self.new_text_file(win),
                (None, _) => {}
            },
        }
    }

    fn draw(&self, win: &Window) -> Result<(), Error> {
        win.title("Files")?;
        self.draw_sidebar(win)?;
        match self.preview {
            Some(k) => draw_preview(win, self.home, &self.place, &self.entries[k]),
            None => {
                draw_list(win, &self.place, &self.entries[..self.count], self.selected)?;
                if let Some((k, name)) = &self.renaming {
                    let mut l = Line::new();
                    let _ = write!(l, "{}F{}{}", mark::ICON, name, mark::CARET);
                    win.line(1 + k, l.as_str())?;
                    win.show()?;
                }
                Ok(())
            }
        }
    }

    fn draw_sidebar(&self, win: &Window) -> Result<(), Error> {
        let here = self.place.as_str();
        for (k, &(label, icon, _)) in PLACES.iter().enumerate() {
            let mut l = Line::new();
            if icon == 0 {
                let _ = write!(l, "{}d{}", mark::COLOUR, label);
            } else {
                let _ = write!(l, "{}{}{}", mark::ICON, icon as char, label);
            }
            win.side(k, l.as_str())?;
        }
        win.sides(PLACES.len())?;
        let at = PLACES
            .iter()
            .position(|&(_, icon, path)| icon != 0 && path == here);
        win.side_select(at)
    }
}

fn named(name: &str, dir: bool, app: Option<usize>) -> Entry {
    let mut e = Entry {
        name: [0; MAX_DIR_NAME],
        len: name.len().min(MAX_DIR_NAME),
        dir,
        size: 0,
        app,
    };
    e.name[..e.len].copy_from_slice(&name.as_bytes()[..e.len]);
    e
}

/// Fills `entries` with the folder at `path`; returns how many.
fn list(home: u64, path: &str, entries: &mut [Option<Entry>]) -> usize {
    if path == APPS_FOLDER {
        for (k, e) in entries.iter_mut().enumerate() {
            *e = APPLICATIONS
                .get(k)
                .map(|&(label, _)| named(label, false, Some(k)));
        }
        return APPLICATIONS.len().min(entries.len());
    }
    let mut count = 0;
    let mut index = 0u64;
    while count < entries.len() {
        let e: DirEntry = match dir_list(home, path, index) {
            Ok(e) => e,
            Err(Error::NotFound) => break,
            Err(e) => {
                zoo_rt::log!("Files: cannot list `{}`: {}", path, e.name());
                break;
            }
        };
        index += 1;
        let len = (e.name_len as usize).min(MAX_DIR_NAME);
        // Hidden names (`.tmp`, Wine's `.wine`) are not shown, as in macOS.
        if len > 0 && e.name[0] == b'.' {
            continue;
        }
        entries[count] = Some(Entry {
            name: e.name,
            len,
            dir: e.is_dir(),
            size: e.size,
            app: None,
        });
        count += 1;
    }
    // Home ends with the Applications folder.
    if path.is_empty() && count < entries.len() {
        entries[count] = Some(named(APPS_FOLDER, true, None));
        count += 1;
    }
    for e in &mut entries[count..] {
        *e = None;
    }
    count
}

fn location(place: &Place, file: Option<&str>) -> Line {
    let mut l = Line::new();
    if let Some(rest) = place.as_str().strip_prefix(TRASH) {
        let _ = write!(l, "Trash{}", rest);
    } else {
        let _ = l.write_str("Home");
        if place.len > 0 {
            let _ = write!(l, "/{}", place.as_str());
        }
    }
    if let Some(f) = file {
        let _ = write!(l, "/{}", f);
    }
    l
}

/// Whether a file name looks like program text (a code icon).
fn is_code(name: &str) -> bool {
    let ext = name.rsplit('.').next().unwrap_or("");
    name.contains('.') && matches!(ext, "rs" | "c" | "h" | "py" | "js" | "sh" | "toml" | "json")
}

/// `n` bytes, as Finder writes sizes.
fn size(l: &mut Line, n: u64) {
    let _ = match n {
        0..1000 => write!(l, "{} bytes", n),
        1000..1_000_000 => write!(l, "{} KB", n.div_ceil(1000)),
        _ => write!(l, "{}.{} MB", n / 1_000_000, n % 1_000_000 / 100_000),
    };
}

fn draw_list(
    win: &Window,
    place: &Place,
    entries: &[Option<Entry>],
    selected: usize,
) -> Result<(), Error> {
    win.lines(1 + entries.len().max(1))?;
    win.line(0, location(place, None).as_str())?;
    if entries.is_empty() {
        let mut l = Line::new();
        let _ = write!(l, "{}d(empty folder)", mark::COLOUR);
        win.line(1, l.as_str())?;
        win.select(None)?;
        return win.show();
    }
    for (i, e) in entries.iter().enumerate() {
        if let Some(e) = e {
            let mut l = Line::new();
            let icon = match (e.app, e.dir) {
                (Some(a), _) => (b'0' + a as u8) as char,
                (None, true) if e.name() == APPS_FOLDER => 'A',
                (None, true) => 'F',
                (None, false) if is_code(e.name()) => 'C',
                (None, false) => 'D',
            };
            let _ = write!(l, "{}{}{}", mark::ICON, icon, e.name());
            if e.app.is_some() {
                let _ = write!(l, "{}Application", mark::RIGHT);
            } else if e.dir {
                let _ = write!(l, "{}Folder", mark::RIGHT);
            } else {
                let _ = l.write_char(mark::RIGHT);
                size(&mut l, e.size);
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
    let Some(e) = entry else {
        return win.show();
    };
    let mut path = *place;
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
        // Control bytes would be read as markup.
        for c in core::str::from_utf8(line).unwrap_or("(not text)").chars() {
            let _ = l.write_char(if c.is_control() { ' ' } else { c });
        }
        win.line(shown, l.as_str())?;
        shown += 1;
    }
    win.line(shown, "Backspace: back to the folder")?;
    win.lines(shown + 1)?;
    win.select(None)?;
    win.show()
}
