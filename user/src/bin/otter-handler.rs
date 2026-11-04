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
/// The Trash, as Files keeps it (docs/u3h-design.md).
const TRASH: &str = ".Trash";

/// The assistant's memory of requests and the commands that answered them
/// (docs/a2-design.md): `request<TAB>command` lines, newest last.
const MEMORY: &str = ".bella-learned";
/// What she learned before 0.116.0, without confirmation: set aside at
/// start, never read (docs/one-bella-design.md).
const UNCONFIRMED: &str = ".bella-memory";
const SET_ASIDE: &str = ".bella-memory-unconfirmed";
/// What the user told Bella to remember, one fact per line
/// (docs/facts-design.md).
const FACTS: &str = ".bella-facts";
/// Largest facts file kept; the oldest facts go first.
const FACTS_BYTES: usize = 8 * 1024;
/// What Bella changed, one dated line per action (docs/audit-design.md).
const AUDIT: &str = ".bella-audit";
/// Largest audit file kept; the oldest lines go first.
const AUDIT_BYTES: usize = 16 * 1024;
/// Lines `audit-list` shows, newest last.
const AUDIT_SHOWN: usize = 8;
/// Requests that change the home folder or what Bella remembers: these
/// get a line in `AUDIT` as well as in the log.
const CHANGES: [&str; 7] = [
    "create",
    "write",
    "move",
    "trash",
    "undo",
    "fact-add",
    "fact-forget",
];
/// Largest file `write` makes (docs/save-design.md).
const WRITE_BYTES: usize = 900;
/// The memory file's name before 0.77.0, moved to `MEMORY` at start.
const OLD_MEMORY: &str = ".assistant-memory";
/// Largest memory file kept; the oldest pairs go first.
const MEMORY_BYTES: usize = 16 * 1024;
/// Words that do not change what a request means.
const FILLER: [&str; 14] = [
    "a", "an", "the", "my", "please", "me", "i", "to", "for", "of", "can", "you", "could", "would",
];

/// Words of a question that say how it is asked, not what about: left
/// out of a lookup (docs/lookup-design.md).
const ASKING: [&str; 24] = [
    "what",
    "when",
    "where",
    "who",
    "why",
    "how",
    "which",
    "does",
    "did",
    "do",
    "say",
    "says",
    "said",
    "notes",
    "note",
    "file",
    "files",
    "documents",
    "about",
    "write",
    "wrote",
    "written",
    "tell",
    "there",
];
/// Files a lookup reads, the bytes it reads of each, and the lines it
/// sends back.
const LOOKUP_FILES: usize = 200;
const LOOKUP_BYTES: usize = 16 * 1024;
const LOOKUP_LINES: usize = 3;

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
    /// finishes with `none`, so Bella does not learn it (B3).
    empty: bool,
    /// Folders the last `create` made, in creation order.
    journal: Vec<String>,
    /// The last `move` (from, to), for `undo`; `create` and `move` each
    /// replace what `undo` undoes (docs/access-design.md).
    moved: Option<(String, String)>,
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
        moved: None,
    };
    // What she learned before 0.116.0 was not confirmed by the user (it
    // held wrong mappings): set aside, kept but never read.
    for old in [UNCONFIRMED, OLD_MEMORY] {
        if dir_stat(home, SET_ASIDE) == Err(Error::NotFound)
            && zoo_rt::rename(home, old, SET_ASIDE).is_ok()
        {
            zoo_rt::log!(
                "OtterHandler: set aside {} as {} (learned without confirmation)",
                old,
                SET_ASIDE
            );
        }
    }
    let mut buf = [0u8; zoo_abi::MAX_MESSAGE];
    // The heap's peak is logged each time it passes another 64 KiB
    // (docs/stage1-design.md).
    let mut peak_logged = 0;
    loop {
        let (_, peak, _) = zoo_rt::heap_stats();
        if peak >= peak_logged + 64 * 1024 {
            peak_logged = peak;
            zoo_rt::log!("OtterHandler: heap peak {} KiB", peak / 1024);
        }
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
        otter.audit(req, result);
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
    // The model writes `.` for the home folder itself.
    if path == "." || path == "~" {
        return Ok("");
    }
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
            "list" => self.list(&self.resolve_existing(clean(arg)?)),
            "inspect" => self.inspect(&self.resolve_existing(clean(arg)?)),
            "plan" => self.create(&self.resolve(clean(arg)?), false),
            "create" => self.create(&self.resolve(clean(arg)?), true),
            "undo" if arg.is_empty() => self.undo(),
            // Broad access (docs/access-design.md): Bella asks before
            // `trash`; the rest need no confirmation.
            "read" => self.read(&self.resolve_existing(clean(arg)?)),
            "move" => self.move_to(arg),
            // `write <path>\t<text>`: a new file; never overwrites
            // (docs/save-design.md).
            "write" => self.write_new(arg),
            "trash" => self.trash(&self.resolve_existing(clean(arg)?)),
            "info" if arg.is_empty() => self.info(),
            // Facts the user told Bella (docs/facts-design.md).
            // Lines of the user's files that answer a question
            // (docs/lookup-design.md).
            "lookup" => self.lookup(arg.trim()),
            "fact-add" => self.fact_add(arg.trim()),
            "fact-find" => self.fact_find(arg.trim()),
            "fact-list" if arg.is_empty() => self.fact_list(),
            "fact-forget" => self.fact_forget(arg.trim()),
            "audit-list" if arg.is_empty() => self.audit_list(),
            "remember" => self.remember(arg),
            "recall" => self.recall(arg),
            _ => Err("unknown tool"),
        }
    }

    /// One line per request in the log, and for a change that worked, a
    /// dated line in `AUDIT` (docs/audit-design.md).
    fn audit(&self, req: &str, result: Result<(), &str>) {
        let tool = req.split(' ').next().unwrap_or("");
        if tool == "audit-list" {
            return;
        }
        let outcome = match result {
            Ok(()) if self.empty => "nothing found",
            Ok(()) => "done",
            Err(why) => why,
        };
        let what = req.split('\t').next().unwrap_or(req).trim();
        zoo_rt::log!("OtterHandler: audit: {}: {}", what, outcome);
        if result.is_err() || !CHANGES.contains(&tool) {
            return;
        }
        let mut lines = self.lines_of(AUDIT, AUDIT_BYTES).unwrap_or_default();
        let mut l = String::new();
        if let Ok(ns) = zoo_rt::clock(zoo_abi::CLOCK_REALTIME) {
            let secs = ns / 1_000_000_000;
            let (days, rem) = (secs / 86_400, secs % 86_400);
            let (y, m, d) = civil_from_days(days as i64);
            let _ = write!(
                l,
                "{:04}-{:02}-{:02} {:02}:{:02} ",
                y,
                m,
                d,
                rem / 3600,
                rem / 60 % 60
            );
        }
        let _ = l.write_str(what);
        lines.push(l);
        // The oldest lines go first once the file would pass its size.
        let mut size: usize = lines.iter().map(|l| l.len() + 1).sum();
        let mut drop = 0;
        while size > AUDIT_BYTES && drop < lines.len() {
            size -= lines[drop].len() + 1;
            drop += 1;
        }
        if self.write_lines(AUDIT, &lines[drop..]).is_err() {
            zoo_rt::log!("OtterHandler: could not write the audit file");
        }
    }

    /// The last `AUDIT_SHOWN` changes, newest last.
    fn audit_list(&mut self) -> Result<(), &'static str> {
        let lines = self.lines_of(AUDIT, AUDIT_BYTES)?;
        if lines.is_empty() {
            self.empty = true;
        }
        let from = lines.len().saturating_sub(AUDIT_SHOWN);
        for l in &lines[from..] {
            zoo_rt::log!("OtterHandler: shows change: {}", l);
            self.line(format_args!("{}", l))
                .map_err(|_| "reply failed")?;
        }
        zoo_rt::log!(
            "OtterHandler: listed {} of {} changes",
            lines.len() - from,
            lines.len()
        );
        Ok(())
    }

    /// The facts file's lines.
    fn facts(&self) -> Result<Vec<String>, &'static str> {
        self.lines_of(FACTS, FACTS_BYTES)
    }

    fn write_facts(&self, facts: &[String]) -> Result<(), &'static str> {
        self.write_lines(FACTS, facts)
    }

    /// The non-blank lines of home file `name`, reading at most `cap`
    /// bytes; none if it does not exist.
    fn lines_of(&self, name: &str, cap: usize) -> Result<Vec<String>, &'static str> {
        let mut text = alloc::vec![0u8; cap];
        let mut len = 0;
        while len < text.len() {
            match file_read(self.home, name, len as u64, &mut text[len..]) {
                Ok(0) | Err(Error::NotFound) => break,
                Ok(n) => len += n,
                Err(e) => return Err(reason(e)),
            }
        }
        Ok(core::str::from_utf8(&text[..len])
            .unwrap_or("")
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(String::from)
            .collect())
    }

    /// Replaces home file `name` with `lines`.
    fn write_lines(&self, name: &str, lines: &[String]) -> Result<(), &'static str> {
        let mut text = String::new();
        for f in lines {
            text.push_str(f);
            text.push('\n');
        }
        let _ = file_create(self.home, name);
        file_truncate(self.home, name, 0)
            .and_then(|()| file_write(self.home, name, 0, text.as_bytes()).map(|_| ()))
            .map_err(reason)
    }

    fn fact_add(&mut self, fact: &str) -> Result<(), &'static str> {
        if fact.is_empty() || fact.contains(['\n', '\t']) || fact.len() > 300 {
            return Err("nothing to remember");
        }
        let mut facts = self.facts()?;
        facts.retain(|f| f != fact);
        facts.push(String::from(fact));
        while facts.iter().map(|f| f.len() + 1).sum::<usize>() > FACTS_BYTES {
            facts.remove(0);
        }
        self.write_facts(&facts)?;
        zoo_rt::log!("OtterHandler: remembered a fact ({} kept)", facts.len());
        Ok(())
    }

    /// The facts sharing the most words with `request` (filler dropped), at
    /// most three, as `| <shared words> <fact>`.
    fn fact_find(&mut self, request: &str) -> Result<(), &'static str> {
        let key = words(request);
        let mut scored: Vec<(usize, String)> = self
            .facts()?
            .into_iter()
            .map(|f| (words(&f).iter().filter(|w| key.contains(w)).count(), f))
            .filter(|(s, _)| *s > 0)
            .collect();
        scored.sort_by_key(|a| core::cmp::Reverse(a.0));
        for (s, f) in scored.iter().take(3) {
            self.line(format_args!("{} {}", s, f))
                .map_err(|_| "reply failed")?;
        }
        Ok(())
    }

    fn fact_list(&mut self) -> Result<(), &'static str> {
        let facts = self.facts()?;
        if facts.is_empty() {
            self.empty = true;
        }
        for f in &facts {
            self.line(format_args!("{}", f))
                .map_err(|_| "reply failed")?;
        }
        zoo_rt::log!("OtterHandler: listed {} facts", facts.len());
        Ok(())
    }

    /// Forgets every fact that shares at least half of its words with
    /// `about` (Bella asks first).
    fn fact_forget(&mut self, about: &str) -> Result<(), &'static str> {
        let key = words(about);
        let facts = self.facts()?;
        let (gone, kept): (Vec<String>, Vec<String>) = facts.into_iter().partition(|f| {
            let w = words(f);
            let shared = key.iter().filter(|k| w.contains(k)).count();
            !key.is_empty() && shared * 2 >= key.len()
        });
        if gone.is_empty() {
            self.empty = true;
            return self
                .line(format_args!("nothing remembered about that"))
                .map_err(|_| "reply failed");
        }
        self.write_facts(&kept)?;
        for f in &gone {
            self.line(format_args!("forgot: {}", f))
                .map_err(|_| "reply failed")?;
        }
        zoo_rt::log!("OtterHandler: forgot {} facts", gone.len());
        Ok(())
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

    /// `resolve`, then, for something that must already exist: when the
    /// path names nothing, the one file or folder anywhere in home whose
    /// name, or name without its extension, matches the last part without
    /// filler words (`notes` finds Documents/notes.txt, `my playlist` finds
    /// Music/playlist.txt). Only a single match counts
    /// (docs/access-design.md).
    fn resolve_existing(&self, path: &str) -> String {
        let out = self.resolve(path);
        if out.is_empty() || dir_stat(self.home, &out).is_ok() {
            return out;
        }
        let last = out.rsplit('/').next().unwrap_or(&out);
        let want: Vec<&str> = last
            .split([' ', '_'])
            .filter(|w| !w.is_empty() && !FILLER.iter().any(|f| f.eq_ignore_ascii_case(w)))
            .collect();
        if want.is_empty() {
            return out;
        }
        let want = want.join(" ");
        let mut found = Vec::new();
        self.find_named("", 0, &want, &mut found);
        match found.as_slice() {
            [one] => {
                zoo_rt::log!("OtterHandler: `{}` is `{}` (found by name)", path, one);
                one.clone()
            }
            _ => out,
        }
    }

    /// Paths below `dir` whose name, or name without extension, is `want`
    /// (letter case ignored); stops after two.
    fn find_named(&self, dir: &str, depth: usize, want: &str, found: &mut Vec<String>) {
        if depth > MAX_WALK_DEPTH || found.len() > 1 {
            return;
        }
        for e in self.entries(dir).unwrap_or_default() {
            let name = e.name();
            let stem = name.rsplit_once('.').map_or(name, |(s, _)| s);
            let path = join(dir, name);
            if name.eq_ignore_ascii_case(want) || stem.eq_ignore_ascii_case(want) {
                found.push(path.clone());
            }
            if e.is_dir() {
                self.find_named(&path, depth + 1, want, found);
            }
        }
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
        // Filler words ("my", "the") are not searched for: "my music"
        // finds Music (docs/talk-design.md).
        let words: Vec<&[u8]> = text
            .split_whitespace()
            .filter(|w| !FILLER.iter().any(|f| f.eq_ignore_ascii_case(w)))
            .map(str::as_bytes)
            .collect();
        if words.is_empty() {
            return Err("nothing to search for");
        }
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

    /// `lookup <question>`: the lines of the home folder's text files
    /// that share the most words with the question, best first, as
    /// `path: line`. Hidden files (Bella's own) are not read.
    fn lookup(&mut self, question: &str) -> Result<(), &'static str> {
        let key: Vec<String> = words(question)
            .into_iter()
            .filter(|w| w.len() >= 3 && !ASKING.contains(&w.as_str()))
            .collect();
        if key.is_empty() {
            return Err("nothing to look up");
        }
        let mut best: Vec<(usize, String)> = Vec::new();
        let mut read = 0;
        self.look_in("", 0, &key, &mut best, &mut read);
        for (_, l) in &best {
            self.line(format_args!("{}", l))
                .map_err(|_| "reply failed")?;
        }
        if best.is_empty() {
            self.empty = true;
        }
        zoo_rt::log!(
            "OtterHandler: lookup `{}` found {} lines in {} files read",
            key.join(" "),
            best.len(),
            read
        );
        Ok(())
    }

    fn look_in(
        &self,
        dir: &str,
        depth: usize,
        key: &[String],
        best: &mut Vec<(usize, String)>,
        read: &mut usize,
    ) {
        if depth > MAX_WALK_DEPTH {
            return;
        }
        let Ok(entries) = self.entries(dir) else {
            return;
        };
        for e in entries {
            if *read >= LOOKUP_FILES {
                return;
            }
            let path = join(dir, e.name());
            if e.is_dir() {
                self.look_in(&path, depth + 1, key, best, read);
                continue;
            }
            let mut buf = alloc::vec![0u8; LOOKUP_BYTES];
            let Ok(n) = file_read(self.home, &path, 0, &mut buf) else {
                continue;
            };
            *read += 1;
            let Ok(text) = core::str::from_utf8(&buf[..n]) else {
                continue;
            };
            if text.contains('\0') {
                continue;
            }
            for line in text.lines() {
                let lower = line.to_ascii_lowercase();
                let score = key.iter().filter(|k| lower.contains(k.as_str())).count();
                if score == 0 {
                    continue;
                }
                let mut l = String::from(&path);
                l.push_str(": ");
                l.extend(line.trim().chars().take(160));
                // Best first; among equals, the first found stays first.
                let at = best
                    .iter()
                    .position(|(s, _)| *s < score)
                    .unwrap_or(best.len());
                if at < LOOKUP_LINES {
                    best.insert(at, (score, l));
                    best.truncate(LOOKUP_LINES);
                }
            }
        }
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
            self.moved = None;
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

    /// Shows a text file's first lines: at most 12 lines of its first
    /// 4 KiB (docs/access-design.md).
    fn read(&self, path: &str) -> Result<(), &'static str> {
        if path.is_empty() {
            return Err("no file named");
        }
        let e = dir_stat(self.home, path).map_err(reason)?;
        if e.is_dir() {
            return Err("a folder, not a file");
        }
        let mut buf = alloc::vec![0u8; 4096];
        let n = file_read(self.home, path, 0, &mut buf).map_err(reason)?;
        let Ok(text) = core::str::from_utf8(&buf[..n]) else {
            return Err("not a text file");
        };
        if text.contains('\0') {
            return Err("not a text file");
        }
        let mut shown = 0;
        for line in text.lines().take(12) {
            shown += line.len() + 1;
            let cut: String = line.chars().take(200).collect();
            self.line(format_args!("{}", cut))
                .map_err(|_| "reply failed")?;
        }
        let rest = e.size.saturating_sub(shown as u64);
        if rest > 0 {
            self.line(format_args!("({} more bytes)", rest))
                .map_err(|_| "reply failed")?;
        }
        Ok(())
    }

    /// `write <path>\t<text>`: makes a new text file. An existing file is
    /// never replaced; its folder must exist.
    fn write_new(&mut self, arg: &str) -> Result<(), &'static str> {
        let (path, text) = arg.split_once('\t').ok_or("expected <path> and text")?;
        if text.is_empty() || text.len() > WRITE_BYTES || text.contains('\0') {
            return Err("nothing to write");
        }
        let path = self.resolve(clean(path)?);
        if path.is_empty() {
            return Err("no file named");
        }
        match dir_stat(self.home, &path) {
            Ok(_) => return Err("already exists"),
            Err(Error::NotFound) => {}
            Err(e) => return Err(reason(e)),
        }
        if let Some((folder, _)) = path.rsplit_once('/')
            && !dir_stat(self.home, folder).is_ok_and(|e| e.is_dir())
        {
            return Err("no such folder");
        }
        file_create(self.home, &path).map_err(reason)?;
        file_write(self.home, &path, 0, text.as_bytes()).map_err(reason)?;
        zoo_rt::log!("OtterHandler: wrote {} ({} bytes)", path, text.len());
        self.line(format_args!("saved {}", path))
            .map_err(|_| "reply failed")
    }

    /// `move <from> to <to>`: renames, or moves into `<to>` when it is a
    /// folder. Undone by `undo`.
    fn move_to(&mut self, arg: &str) -> Result<(), &'static str> {
        let (from, to) = arg.split_once(" to ").ok_or("expected <from> to <to>")?;
        let from = self.resolve_existing(clean(from)?);
        let mut to = self.resolve(clean(to)?);
        if from.is_empty() || to.is_empty() {
            return Err("no file or folder named");
        }
        dir_stat(self.home, &from).map_err(reason)?;
        if dir_stat(self.home, &to).is_ok_and(|e| e.is_dir()) {
            let name = from.rsplit('/').next().unwrap_or(&from);
            to = join(&to, name);
        }
        if to == from || to.starts_with(&join(&from, "")) {
            return Err("cannot move a folder into itself");
        }
        if dir_stat(self.home, &to).is_ok() {
            return Err("already exists");
        }
        zoo_rt::rename(self.home, &from, &to).map_err(reason)?;
        zoo_rt::log!("OtterHandler: moved {} to {}", from, to);
        self.line(format_args!("moved {} to {}", from, to))
            .map_err(|_| "reply failed")?;
        self.journal.clear();
        self.moved = Some((from, to));
        Ok(())
    }

    /// Moves a file or folder into the Trash (`.Trash`, as Files does), so
    /// it can be put back. Bella asks first.
    fn trash(&mut self, path: &str) -> Result<(), &'static str> {
        if path.is_empty() || path == TRASH || path.starts_with(&join(TRASH, "")) {
            return Err("not something to throw away");
        }
        dir_stat(self.home, path).map_err(reason)?;
        let _ = dir_create(self.home, TRASH);
        let name = path.rsplit('/').next().unwrap_or(path);
        let mut to = join(TRASH, name);
        for n in 2..100 {
            if dir_stat(self.home, &to).is_err() {
                break;
            }
            to = join(TRASH, name);
            let _ = write!(to, " {}", n);
        }
        zoo_rt::rename(self.home, path, &to).map_err(reason)?;
        zoo_rt::log!("OtterHandler: moved {} to the Trash as {}", path, to);
        self.line(format_args!("moved {} to the Trash", path))
            .map_err(|_| "reply failed")?;
        Ok(())
    }

    /// About this computer: the version, the time, how long it has run, and
    /// what the home folder holds.
    fn info(&self) -> Result<(), &'static str> {
        let say = |args: core::fmt::Arguments| self.line(args).map_err(|_| "reply failed");
        say(format_args!("ZooOS {}", env!("CARGO_PKG_VERSION")))?;
        if let Ok(ns) = zoo_rt::clock(zoo_abi::CLOCK_REALTIME) {
            let secs = ns / 1_000_000_000;
            let (days, rem) = (secs / 86_400, secs % 86_400);
            let (y, m, d) = civil_from_days(days as i64);
            say(format_args!(
                "{:04}-{:02}-{:02} {:02}:{:02} UTC",
                y,
                m,
                d,
                rem / 3600,
                rem / 60 % 60
            ))?;
        }
        let up = zoo_rt::ticks() / 100;
        say(format_args!("running for {} min {} s", up / 60, up % 60))?;
        let (mut files, mut folders, mut bytes) = (0u64, 0u64, 0u64);
        self.count("", 0, &mut files, &mut folders, &mut bytes);
        zoo_rt::log!(
            "OtterHandler: info: home holds {} files, {} folders",
            files,
            folders
        );
        say(format_args!(
            "home: {} files, {} folders, {} bytes",
            files, folders, bytes
        ))
    }

    fn count(&self, dir: &str, depth: usize, files: &mut u64, folders: &mut u64, bytes: &mut u64) {
        if depth > MAX_WALK_DEPTH {
            return;
        }
        for e in self.entries(dir).unwrap_or_default() {
            if e.is_dir() {
                *folders += 1;
                self.count(&join(dir, e.name()), depth + 1, files, folders, bytes);
            } else {
                *files += 1;
                *bytes += e.size;
            }
        }
    }

    /// Undoes the last `create` (removes what it made, deepest first, only
    /// while empty) or the last `move` (moves it back).
    fn undo(&mut self) -> Result<(), &'static str> {
        if let Some((from, to)) = self.moved.take() {
            zoo_rt::rename(self.home, &to, &from).map_err(reason)?;
            zoo_rt::log!("OtterHandler: moved {} back to {}", to, from);
            return self
                .line(format_args!("moved {} back to {}", to, from))
                .map_err(|_| "reply failed");
        }
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

/// Year, month, day of `days` since 1970-01-01 (Howard Hinnant's
/// `civil_from_days`).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}
