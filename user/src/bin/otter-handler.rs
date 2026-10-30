//! OtterHandler: the action broker (docs/m5c1-design.md §4). It holds a
//! directory handle for the user's home and serves tool requests over a
//! channel. Whoever talks to it (later, the assistant model) never gets
//! file access of its own.
//!
//! Protocol: one request per message (`search <text>`, `list <path>`,
//! `inspect <path>`, `plan <path>`, `create <path>`, `undo`, `quit`). Each
//! reply is zero or more `| <line>` messages, then `ok` or `err <reason>`.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write as _;
use zoo_rt::app::send_wait;
use zoo_rt::{
    DirEntry, Error, Handle, Line, dir_create, dir_list, dir_remove, dir_stat, file_read, recv,
};

zoo_rt::entry2!(main);

const MAX_RESULTS: usize = 20;
const SEARCH_BYTES: usize = 64 * 1024;
const MAX_CREATE_DEPTH: usize = 8;
const MAX_WALK_DEPTH: usize = 16;

struct Otter {
    chan: Handle,
    home: Handle,
    /// Folders the last `create` made, in creation order.
    journal: Vec<String>,
}

fn main(chan: u64, home: u64) -> u8 {
    if chan == zoo_rt::NO_HANDLE || home == zoo_rt::NO_HANDLE {
        return 2;
    }
    let mut otter = Otter {
        chan,
        home,
        journal: Vec::new(),
    };
    let mut buf = [0u8; zoo_abi::MAX_MESSAGE];
    loop {
        let n = match recv(chan, &mut buf) {
            Ok(n) => n,
            Err(Error::PeerClosed) => return 0,
            Err(_) => return 3,
        };
        let Ok(req) = core::str::from_utf8(&buf[..n]) else {
            if otter.finish(Err("request is not text")).is_err() {
                return 4;
            }
            continue;
        };
        if req == "quit" {
            return 0;
        }
        let result = otter.handle(req);
        if otter.finish(result).is_err() {
            return 4;
        }
    }
}

/// Human-readable reason for a kernel error.
fn reason(e: Error) -> &'static str {
    match e {
        Error::NotFound => "not found",
        Error::Exists => "already exists",
        Error::NotEmpty => "not empty",
        Error::NotDir => "not a folder",
        Error::Rights => "not permitted",
        Error::Inval => "invalid path",
        Error::NoMem => "out of space",
        _ => e.name(),
    }
}

/// Checks a path relative to home. Absolute paths and `..` are refused here
/// with a clear reason; the kernel refuses them too.
fn clean(path: &str) -> Result<&str, &'static str> {
    let path = path.trim().trim_end_matches('/');
    if path.starts_with('/') || path.split('/').any(|c| c == "..") {
        return Err("path escapes home");
    }
    if path.split('/').any(|c| c.is_empty() || c == ".") && !path.is_empty() {
        return Err("invalid path");
    }
    Ok(path)
}

fn join(base: &str, name: &str) -> String {
    let mut s = String::from(base);
    if !s.is_empty() {
        s.push('/');
    }
    s.push_str(name);
    s
}

fn contains_ignore_case(hay: &[u8], needle: &[u8]) -> bool {
    needle.is_empty()
        || hay
            .windows(needle.len())
            .any(|w| w.eq_ignore_ascii_case(needle))
}

impl Otter {
    fn line(&self, args: core::fmt::Arguments) -> Result<(), Error> {
        let mut l = Line::new();
        let _ = l.write_str("| ");
        let _ = l.write_fmt(args);
        send_wait(self.chan, l.as_str().as_bytes())
    }

    fn finish(&self, result: Result<(), &str>) -> Result<(), Error> {
        let mut l = Line::new();
        let _ = match result {
            Ok(()) => l.write_str("ok"),
            Err(why) => write!(l, "err {}", why),
        };
        send_wait(self.chan, l.as_str().as_bytes())
    }

    fn handle(&mut self, req: &str) -> Result<(), &'static str> {
        let (tool, arg) = req.split_once(' ').unwrap_or((req, ""));
        match tool {
            "search" => self.search(arg.trim()),
            "list" => self.list(clean(arg)?),
            "inspect" => self.inspect(clean(arg)?),
            "plan" => self.create(clean(arg)?, false),
            "create" => self.create(clean(arg)?, true),
            "undo" if arg.is_empty() => self.undo(),
            _ => Err("unknown tool"),
        }
    }

    fn entries(&self, path: &str) -> Result<Vec<DirEntry>, Error> {
        let mut out = Vec::new();
        for i in 0.. {
            match dir_list(self.home, path, i) {
                Ok(e) => out.push(e),
                Err(Error::NotFound) if dir_stat(self.home, path).is_ok() => break,
                Err(e) => return Err(e),
            }
        }
        Ok(out)
    }

    fn list(&self, path: &str) -> Result<(), &'static str> {
        let entries = self.entries(path).map_err(reason)?;
        for e in &entries {
            let sent = if e.is_dir() {
                self.line(format_args!("folder {}", e.name()))
            } else {
                self.line(format_args!("file {} ({} bytes)", e.name(), e.size))
            };
            sent.map_err(|_| "reply failed")?;
        }
        Ok(())
    }

    fn inspect(&self, path: &str) -> Result<(), &'static str> {
        let e = dir_stat(self.home, path).map_err(reason)?;
        let shown = if path.is_empty() { "home" } else { path };
        let sent = if e.is_dir() {
            self.line(format_args!("{}: folder, {} entries", shown, e.size))
        } else {
            self.line(format_args!("{}: file, {} bytes", shown, e.size))
        };
        sent.map_err(|_| "reply failed")
    }

    fn search(&self, text: &str) -> Result<(), &'static str> {
        if text.is_empty() {
            return Err("nothing to search for");
        }
        let mut found = Vec::new();
        let mut truncated = false;
        self.walk("", 0, text.as_bytes(), &mut found, &mut truncated)
            .map_err(reason)?;
        for path in &found {
            self.line(format_args!("{}", path))
                .map_err(|_| "reply failed")?;
        }
        if truncated {
            self.line(format_args!(
                "(more than {} results; showing the first)",
                MAX_RESULTS
            ))
            .map_err(|_| "reply failed")?;
        }
        if found.is_empty() {
            self.line(format_args!("no matches"))
                .map_err(|_| "reply failed")?;
        }
        Ok(())
    }

    fn walk(
        &self,
        dir: &str,
        depth: usize,
        needle: &[u8],
        found: &mut Vec<String>,
        truncated: &mut bool,
    ) -> Result<(), Error> {
        if depth > MAX_WALK_DEPTH {
            return Ok(());
        }
        for e in self.entries(dir)? {
            let path = join(dir, e.name());
            let hit = contains_ignore_case(e.name().as_bytes(), needle)
                || (!e.is_dir() && self.file_contains(&path, needle));
            if hit {
                if found.len() == MAX_RESULTS {
                    *truncated = true;
                    return Ok(());
                }
                found.push(path.clone());
            }
            if e.is_dir() {
                self.walk(&path, depth + 1, needle, found, truncated)?;
            }
        }
        Ok(())
    }

    fn file_contains(&self, path: &str, needle: &[u8]) -> bool {
        let mut buf = alloc::vec![0u8; SEARCH_BYTES];
        match file_read(self.home, path, 0, &mut buf) {
            Ok(n) => contains_ignore_case(&buf[..n], needle),
            Err(_) => false,
        }
    }

    /// `plan` (`apply` false) reports what `create` would do; `create`
    /// makes each missing folder and journals it.
    fn create(&mut self, path: &str, apply: bool) -> Result<(), &'static str> {
        if path.is_empty() {
            return Err("no folder named");
        }
        let parts: Vec<&str> = path.split('/').collect();
        if parts.len() > MAX_CREATE_DEPTH {
            return Err("too many levels");
        }
        if apply {
            self.journal.clear();
        }
        let mut new = 0;
        for k in 1..=parts.len() {
            let prefix = parts[..k].join("/");
            match dir_stat(self.home, &prefix) {
                Ok(e) if e.is_dir() => {
                    self.line(format_args!("exists {}", prefix))
                        .map_err(|_| "reply failed")?;
                }
                Ok(_) => return Err("a file is in the way"),
                Err(Error::NotFound) => {
                    new += 1;
                    if apply {
                        dir_create(self.home, &prefix).map_err(reason)?;
                        self.journal.push(prefix.clone());
                        self.line(format_args!("created {}", prefix))
                    } else {
                        self.line(format_args!("would create {}", prefix))
                    }
                    .map_err(|_| "reply failed")?;
                }
                Err(e) => return Err(reason(e)),
            }
        }
        if new == 0 {
            return Err("already exists");
        }
        Ok(())
    }

    /// Removes what the last `create` made, deepest first, only while empty.
    fn undo(&mut self) -> Result<(), &'static str> {
        if self.journal.is_empty() {
            return Err("nothing to undo");
        }
        let journal = core::mem::take(&mut self.journal);
        for path in journal.iter().rev() {
            match dir_remove(self.home, path) {
                Ok(()) => self.line(format_args!("removed {}", path)),
                Err(Error::NotEmpty) => {
                    self.line(format_args!("not empty, left in place: {}", path))
                }
                Err(Error::NotFound) => self.line(format_args!("already gone: {}", path)),
                Err(e) => self.line(format_args!("{}: {}", path, reason(e))),
            }
            .map_err(|_| "reply failed")?;
        }
        Ok(())
    }
}
