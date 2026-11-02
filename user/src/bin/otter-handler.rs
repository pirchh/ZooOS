//! OtterHandler: the action broker (docs/m5c1-design.md §4). It holds a
//! directory handle for the user's home and serves tool requests over a
//! channel. Whoever talks to it (later, the assistant model) never gets
//! file access of its own.
//!
//! `search` matches an entry when every word of the query appears in its
//! path or (for files) its first 64 KiB, ignoring case.
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
    DirEntry, Error, Handle, Line, dir_create, dir_list, dir_remove, dir_stat, file_create,
    file_read, file_truncate, file_write, recv,
};

zoo_rt::entry2!(main);

const MAX_RESULTS: usize = 20;
const SEARCH_BYTES: usize = 64 * 1024;
const MAX_CREATE_DEPTH: usize = 8;
const MAX_WALK_DEPTH: usize = 16;

/// The assistant's memory of requests and the commands that answered them
/// (docs/a2-design.md): `request<TAB>command` lines, newest last.
const MEMORY: &str = ".assistant-memory";
/// Largest memory file kept; the oldest pairs go first.
const MEMORY_BYTES: usize = 16 * 1024;
/// Words that do not change what a request means.
const FILLER: [&str; 14] = [
    "a", "an", "the", "my", "please", "me", "i", "to", "for", "of", "can", "you", "could", "would",
];

/// A request's words, lower-cased, without punctuation or filler.
fn words(text: &str) -> Vec<String> {
    let mut out: Vec<String> = text
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '/' || c == '.' || c == '-'))
        .map(|w| w.trim_matches('.').to_ascii_lowercase())
        .filter(|w| !w.is_empty() && !FILLER.contains(&w.as_str()))
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// Names a request or command uses: its words, also split at `/`.
fn names(text: &str) -> Vec<String> {
    words(text)
        .iter()
        .flat_map(|w| w.split('/'))
        .filter(|w| !w.is_empty())
        .map(String::from)
        .collect()
}

/// Whether a remembered `command` fits `request`: every name its argument
/// took from the remembered request must be in the new request too, so
/// "make a folder called Receipts" never recalls `create Invoices` (B8).
fn names_agree(remembered: &str, command: &str, request: &str) -> bool {
    let arg = command.split_once(' ').map_or("", |(_, a)| a);
    let (old, new) = (names(remembered), names(request));
    names(arg)
        .iter()
        .all(|n| !old.contains(n) || new.contains(n))
}

/// How alike two requests are: shared words over all words, in percent.
fn likeness(a: &[String], b: &[String]) -> usize {
    let shared = a.iter().filter(|w| b.contains(w)).count();
    let all = a.len() + b.len() - shared;
    (shared * 100).checked_div(all).unwrap_or(0)
}

struct Otter {
    chan: Handle,
    home: Handle,
    /// The last request found nothing (a search without matches): it
    /// finishes with `none`, so the Assistant does not learn it (B3).
    empty: bool,
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
        empty: false,
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
        otter.empty = false;
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
            Ok(()) if self.empty => l.write_str("none"),
            Ok(()) => l.write_str("ok"),
            Err(why) => write!(l, "err {}", why),
        };
        send_wait(self.chan, l.as_str().as_bytes())
    }

    fn handle(&mut self, req: &str) -> Result<(), &'static str> {
        let (tool, arg) = req.split_once(' ').unwrap_or((req, ""));
        match tool {
            "search" => self.search(arg.trim()),
            "list" => self.list(&self.resolve(clean(arg)?)),
            "inspect" => self.inspect(&self.resolve(clean(arg)?)),
            "plan" => self.create(&self.resolve(clean(arg)?), false),
            "create" => self.create(&self.resolve(clean(arg)?), true),
            "undo" if arg.is_empty() => self.undo(),
            "remember" => self.remember(arg),
            "recall" => self.recall(arg),
            _ => Err("unknown tool"),
        }
    }

    /// The memory file's text (empty if there is none yet).
    fn memory(&self) -> Result<Vec<u8>, &'static str> {
        let mut text = alloc::vec![0u8; MEMORY_BYTES];
        let mut len = 0;
        while len < text.len() {
            match file_read(self.home, MEMORY, len as u64, &mut text[len..]) {
                Ok(0) | Err(Error::NotFound) => break,
                Ok(n) => len += n,
                Err(e) => return Err(reason(e)),
            }
        }
        text.truncate(len);
        Ok(text)
    }

    /// `remember <request>\t<command>`: keeps the pair, replacing an
    /// earlier pair for the same request.
    fn remember(&mut self, arg: &str) -> Result<(), &'static str> {
        let (request, command) = arg.split_once('\t').ok_or("expected request and command")?;
        let (request, command) = (request.trim(), command.trim());
        let key = words(request);
        if key.is_empty() || command.is_empty() || request.contains('\n') || command.contains('\n')
        {
            return Err("nothing to remember");
        }
        let old = self.memory()?;
        let mut text = String::new();
        for line in core::str::from_utf8(&old).unwrap_or("").lines() {
            let same = line.split_once('\t').is_some_and(|(r, _)| words(r) == key);
            if !same && !line.is_empty() {
                text.push_str(line);
                text.push('\n');
            }
        }
        let _ = writeln!(text, "{}\t{}", request, command);
        // Over the limit: drop the oldest lines.
        while text.len() > MEMORY_BYTES {
            let cut = text.find('\n').map_or(text.len(), |i| i + 1);
            text.drain(..cut);
        }
        let _ = file_create(self.home, MEMORY);
        file_truncate(self.home, MEMORY, 0)
            .and_then(|()| file_write(self.home, MEMORY, 0, text.as_bytes()).map(|_| ()))
            .map_err(reason)?;
        zoo_rt::log!("OtterHandler: remembered `{}` -> `{}`", request, command);
        Ok(())
    }

    /// `recall <request>`: the remembered command for the most alike
    /// request, as `| <likeness> <command>`, if any is alike at all.
    fn recall(&mut self, request: &str) -> Result<(), &'static str> {
        let key = words(request);
        let text = self.memory()?;
        let mut best: Option<(usize, &str)> = None;
        for line in core::str::from_utf8(&text).unwrap_or("").lines() {
            let Some((r, c)) = line.split_once('\t') else {
                continue;
            };
            let score = likeness(&key, &words(r));
            if score > 0 && !names_agree(r, c, request) {
                zoo_rt::log!(
                    "OtterHandler: `{}` names other things than `{}`",
                    c,
                    request
                );
                continue;
            }
            // Ties go to the newer pair.
            if score > 0 && best.is_none_or(|(b, _)| score >= b) {
                best = Some((score, c));
            }
        }
        if let Some((score, command)) = best {
            self.line(format_args!("{} {}", score, command))
                .map_err(|_| "reply failed")?;
        }
        Ok(())
    }

    fn entries(&self, path: &str) -> Result<Vec<DirEntry>, Error> {
        let mut out = Vec::new();
        for i in 0.. {
            match dir_list(self.home, path, i) {
                // Hidden names (`.tmp`, Wine's `.wine`), as in macOS.
                Ok(e) if e.name().starts_with('.') => {}
                Ok(e) => out.push(e),
                Err(Error::NotFound) if dir_stat(self.home, path).is_ok() => break,
                Err(e) => return Err(e),
            }
        }
        Ok(out)
    }

    /// `path` with each name that exists only in other letter case
    /// replaced by the name on disk, as macOS treats names: the model often
    /// writes `music` for `Music`. A name that matches nothing, or more than
    /// one entry, and the names after it stay as written.
    fn resolve(&self, path: &str) -> String {
        let mut out = String::new();
        let mut parts = path.split('/').filter(|p| !p.is_empty());
        for part in parts.by_ref() {
            let exact = join(&out, part);
            let name = if dir_stat(self.home, &exact).is_ok() {
                None
            } else {
                let entries = self.entries(&out).unwrap_or_default();
                let mut alike = entries
                    .iter()
                    .filter(|e| e.name().eq_ignore_ascii_case(part));
                match (alike.next(), alike.next()) {
                    (Some(e), None) => Some(String::from(e.name())),
                    _ => {
                        out = exact;
                        break;
                    }
                }
            };
            out = match name {
                Some(n) => join(&out, &n),
                None => exact,
            };
        }
        for part in parts {
            out = join(&out, part);
        }
        if out != path {
            zoo_rt::log!("OtterHandler: `{}` is `{}`", path, out);
        }
        out
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

    fn search(&mut self, text: &str) -> Result<(), &'static str> {
        if text.is_empty() {
            return Err("nothing to search for");
        }
        let words: Vec<&[u8]> = text.split_whitespace().map(str::as_bytes).collect();
        let mut found = Vec::new();
        let mut truncated = false;
        self.walk("", 0, &words, &mut found, &mut truncated)
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
            self.empty = true;
            self.line(format_args!("no matches"))
                .map_err(|_| "reply failed")?;
        }
        Ok(())
    }

    fn walk(
        &self,
        dir: &str,
        depth: usize,
        words: &[&[u8]],
        found: &mut Vec<String>,
        truncated: &mut bool,
    ) -> Result<(), Error> {
        if depth > MAX_WALK_DEPTH {
            return Ok(());
        }
        for e in self.entries(dir)? {
            let path = join(dir, e.name());
            let hit = self.matches(&path, e.is_dir(), words);
            if hit {
                if found.len() == MAX_RESULTS {
                    *truncated = true;
                    return Ok(());
                }
                found.push(path.clone());
            }
            if e.is_dir() {
                self.walk(&path, depth + 1, words, found, truncated)?;
            }
        }
        Ok(())
    }

    /// Every word appears in the path, or in the file's first 64 KiB.
    fn matches(&self, path: &str, is_dir: bool, words: &[&[u8]]) -> bool {
        let mut content = None;
        for w in words {
            if contains_ignore_case(path.as_bytes(), w) {
                continue;
            }
            if is_dir {
                return false;
            }
            let data = content.get_or_insert_with(|| {
                let mut buf = alloc::vec![0u8; SEARCH_BYTES];
                let n = file_read(self.home, path, 0, &mut buf).unwrap_or(0);
                buf.truncate(n);
                buf
            });
            if !contains_ignore_case(data, w) {
                return false;
            }
        }
        true
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
