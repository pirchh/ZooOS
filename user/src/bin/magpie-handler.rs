//! MagpieHandler: Bella's index of the user's files (docs/vectors-design.md).
//! It reads the embedding model from the model disk, embeds every line of
//! the home folder's text files, keeps the vectors in `.bella-index`, and
//! answers `query <question>` with the lines nearest in meaning.
//!
//! Started by Bella with a channel and a copy of her home handle. A
//! background program: indexing never holds up the desktop. Lines of a
//! file are embedded again only when the file changed (its FNV-1a hash).
//!
//! Protocol, one request at a time:
//! - `query <question>`: refreshes the index, then sends up to
//!   `ANSWER_LINES` lines `| <path>: <line>` (best first), then `ok`, or
//!   `none` when no line is near enough.
//! - `quit`.
//!
//! It sends `ready <lines>` once the first index is built, and `err <why>`
//! if it cannot work (no embedding model).

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use zoo_infer::embed::{DIM, Embedder, similarity};
use zoo_infer::{Gguf, Model, Tokenizer};
use zoo_rt::app::send_wait;
use zoo_rt::{
    Handle, dir_list, dir_stat, file_create, file_read, file_truncate, file_write, log, recv,
};

zoo_rt::entry2!(main);

/// The index file, in the home folder (hidden, so no lookup reads it).
const INDEX: &str = ".bella-index";
const INDEX_MAGIC: &[u8; 8] = b"ZOOVIX01";
/// Files read, bytes read of each, lines kept, characters kept of a line.
const MAX_FILES: usize = 200;
const FILE_BYTES: usize = 16 * 1024;
const MAX_LINES: usize = 2000;
const LINE_CHARS: usize = 300;
const MAX_DEPTH: usize = 16;
/// Lines sent for a question, and how near they must be (cosine).
const ANSWER_LINES: usize = 3;
const MIN_SCORE: f32 = 0.45;
/// What Bella learned: `request<TAB>command` lines (docs/learning-design.md).
const MEMORY: &str = ".bella-memory";
const MEMORY_BYTES: usize = 16 * 1024;
/// Past requests sent for a new one, and how near they must be.
const EXAMPLES: usize = 3;
const MIN_EXAMPLE: f32 = 0.6;

/// A past request, the command that answered it, and the request's vector.
struct Example {
    request: String,
    command: String,
    vec: Vec<f32>,
}

/// One indexed line: the file it came from (and that file's hash), the
/// line, and its vector.
struct Entry {
    hash: u64,
    path: String,
    line: String,
    vec: Vec<f32>,
}

fn main(chan: u64, home: u64) -> u8 {
    if chan == zoo_rt::NO_HANDLE || home == zoo_rt::NO_HANDLE {
        return 2;
    }
    // Indexing may take minutes the first time: only when nothing else,
    // not even the chat model, wants the processor.
    let _ = zoo_rt::set_idle();
    let Some(file) = embedder_bytes() else {
        let _ = send_wait(chan, b"err no embedding model");
        return 3;
    };
    let t = zoo_rt::now_us();
    let Ok(g) = Gguf::parse(file) else { return 4 };
    let (Ok(tok), Ok(model)) = (Tokenizer::from_gguf(&g), Model::from_gguf(&g)) else {
        return 5;
    };
    zoo_rt::log!(
        "MagpieHandler: embedding model ready (parsed in {} ms, engine {})",
        (zoo_rt::now_us() - t) / 1000,
        zoo_infer::cpu::backend()
    );
    let mut emb = Embedder::new(&model, &tok);
    let mut index = load(home);
    if !refresh(home, chan, &mut emb, &mut index) {
        return 0;
    }
    let mut examples: Vec<Example> = Vec::new();
    learn(home, &mut emb, &mut examples);
    let mut l = zoo_rt::Line::new();
    let _ = core::fmt::Write::write_fmt(&mut l, format_args!("ready {}", index.len()));
    if send_wait(chan, l.as_str().as_bytes()).is_err() {
        return 0;
    }
    let mut buf = [0u8; zoo_abi::MAX_MESSAGE];
    loop {
        let n = match recv(chan, &mut buf) {
            Ok(n) => n,
            Err(_) => return 0,
        };
        let msg = core::str::from_utf8(&buf[..n]).unwrap_or("");
        if msg == "quit" {
            return 0;
        }
        if let Some(request) = msg.strip_prefix("example ") {
            if !examples_for(chan, home, &mut emb, &mut examples, request) {
                return 0;
            }
            continue;
        }
        let Some(question) = msg.strip_prefix("query ") else {
            let _ = send_wait(chan, b"err unknown request");
            continue;
        };
        if !refresh(home, chan, &mut emb, &mut index) {
            return 0;
        }
        let t = zoo_rt::now_us();
        let Ok(q) = emb.query(question) else {
            let _ = send_wait(chan, b"err model error");
            continue;
        };
        // The nearest lines, best first.
        let mut best: Vec<(f32, usize)> = Vec::new();
        for (k, e) in index.iter().enumerate() {
            let s = similarity(&q, &e.vec);
            if s < MIN_SCORE {
                continue;
            }
            let at = best.iter().position(|(b, _)| *b < s).unwrap_or(best.len());
            if at < ANSWER_LINES {
                best.insert(at, (s, k));
                best.truncate(ANSWER_LINES);
            }
        }
        zoo_rt::log!(
            "MagpieHandler: query: {} of {} lines near enough in {} ms; best {}",
            best.len(),
            index.len(),
            (zoo_rt::now_us() - t) / 1000,
            best.first().map_or(0, |(s, _)| (s * 1000.0) as u32)
        );
        let mut failed = false;
        for &(_, k) in &best {
            let mut l = zoo_rt::Line::new();
            let _ = core::fmt::Write::write_fmt(
                &mut l,
                format_args!("| {}: {}", index[k].path, index[k].line),
            );
            failed |= send_wait(chan, l.as_str().as_bytes()).is_err();
        }
        let end: &[u8] = if best.is_empty() { b"none" } else { b"ok" };
        if failed || send_wait(chan, end).is_err() {
            return 0;
        }
    }
}

/// Brings the past requests up to date with `.bella-memory`: a request
/// already embedded keeps its vector; new ones are embedded
/// (docs/learning-design.md).
fn learn(home: Handle, emb: &mut Embedder, examples: &mut Vec<Example>) {
    let mut buf = alloc::vec![0u8; MEMORY_BYTES];
    let n = file_read(home, MEMORY, 0, &mut buf).unwrap_or(0);
    let text = core::str::from_utf8(&buf[..n]).unwrap_or("");
    let old = core::mem::take(examples);
    let mut embedded = 0;
    for line in text.lines() {
        let Some((request, command)) = line.split_once('\t') else {
            continue;
        };
        let (request, command) = (request.trim(), command.trim());
        if request.is_empty() || command.is_empty() {
            continue;
        }
        // The newest line for a request wins: drop an older one.
        examples.retain(|e| e.request != request);
        let vec = match old.iter().find(|e| e.request == request) {
            Some(e) => e.vec.clone(),
            None => {
                let Ok(v) = emb.query(request) else { continue };
                embedded += 1;
                v
            }
        };
        examples.push(Example {
            request: String::from(request),
            command: String::from(command),
            vec,
        });
    }
    if embedded > 0 {
        zoo_rt::log!(
            "MagpieHandler: learned: {} past requests ({} embedded)",
            examples.len(),
            embedded
        );
    }
}

/// `example <request>`: the past requests nearest it in meaning, best
/// first, as `| <score in thousandths> <request><TAB><command>`, then `ok`
/// or `none`. False when the channel failed.
fn examples_for(
    chan: Handle,
    home: Handle,
    emb: &mut Embedder,
    examples: &mut Vec<Example>,
    request: &str,
) -> bool {
    learn(home, emb, examples);
    let t = zoo_rt::now_us();
    let Ok(q) = emb.query(request) else {
        return send_wait(chan, b"err model error").is_ok();
    };
    let mut best: Vec<(f32, usize)> = Vec::new();
    for (k, e) in examples.iter().enumerate() {
        let s = similarity(&q, &e.vec);
        if s < MIN_EXAMPLE {
            continue;
        }
        let at = best.iter().position(|(b, _)| *b < s).unwrap_or(best.len());
        if at < EXAMPLES {
            best.insert(at, (s, k));
            best.truncate(EXAMPLES);
        }
    }
    zoo_rt::log!(
        "MagpieHandler: examples: {} of {} near enough in {} ms; best {}",
        best.len(),
        examples.len(),
        (zoo_rt::now_us() - t) / 1000,
        best.first().map_or(0, |(s, _)| (s * 1000.0) as u32)
    );
    for &(s, k) in &best {
        let mut l = zoo_rt::Line::new();
        let _ = core::fmt::Write::write_fmt(
            &mut l,
            format_args!(
                "| {} {}\t{}",
                (s * 1000.0) as u32,
                examples[k].request,
                examples[k].command
            ),
        );
        if send_wait(chan, l.as_str().as_bytes()).is_err() {
            return false;
        }
    }
    send_wait(chan, if best.is_empty() { b"none" } else { b"ok" }).is_ok()
}

/// The embedding model's bytes, read whole from the model disk into
/// page-aligned memory, as ElephantHandler reads its model.
fn embedder_bytes() -> Option<&'static [u8]> {
    let len = usize::try_from(zoo_rt::model_size_of(zoo_abi::MODEL_EMBED).ok()?).ok()?;
    let size = len.div_ceil(zoo_abi::BLOCK_BYTES) * zoo_abi::BLOCK_BYTES;
    let layout = core::alloc::Layout::from_size_align(size, 4096).ok()?;
    // SAFETY: `layout` has a non-zero size (`len` > 0, checked by the
    // kernel when it read the model disk's header).
    let p = unsafe { alloc::alloc::alloc(layout) };
    if p.is_null() {
        let _ = log("MagpieHandler: no memory for the embedding model");
        return None;
    }
    // SAFETY: `p` is a fresh allocation of `size` bytes, owned here for
    // the rest of the process (never freed).
    let buf: &'static mut [u8] = unsafe { core::slice::from_raw_parts_mut(p, size) };
    let t = zoo_rt::now_us();
    let run = zoo_abi::BLOCK_RUN_MAX * zoo_abi::BLOCK_BYTES;
    for (k, chunk) in buf.chunks_mut(run).enumerate() {
        let block = (k * zoo_abi::BLOCK_RUN_MAX) as u64;
        if zoo_rt::model_read_of(zoo_abi::MODEL_EMBED, block, chunk).is_err() {
            let _ = log("MagpieHandler: reading the model disk failed");
            return None;
        }
    }
    zoo_rt::log!(
        "MagpieHandler: read the embedding model from the model disk: {} MB in {} ms",
        len >> 20,
        (zoo_rt::now_us() - t) / 1000
    );
    Some(&buf[..len])
}

/// FNV-1a, 64 bits: whether a file changed since it was indexed.
fn fnv(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// The text files of the home folder (not hidden), with their contents.
fn files(home: Handle, dir: &str, depth: usize, out: &mut Vec<(String, Vec<u8>)>) {
    if depth > MAX_DEPTH {
        return;
    }
    for i in 0.. {
        if out.len() >= MAX_FILES {
            return;
        }
        let e = match dir_list(home, dir, i) {
            Ok(e) => e,
            Err(_) => return,
        };
        if e.name().starts_with('.') {
            continue;
        }
        let mut path = String::from(dir);
        if !path.is_empty() {
            path.push('/');
        }
        path.push_str(e.name());
        if e.is_dir() {
            files(home, &path, depth + 1, out);
            continue;
        }
        let mut buf = alloc::vec![0u8; FILE_BYTES];
        let Ok(n) = file_read(home, &path, 0, &mut buf) else {
            continue;
        };
        buf.truncate(n);
        if core::str::from_utf8(&buf).is_ok() && !buf.contains(&0) {
            out.push((path, buf));
        }
    }
}

/// Brings the index up to date: lines of unchanged files keep their
/// vectors; changed and new files are embedded; gone files drop out.
/// False when Bella has gone (no one holds the other end of `chan`): a
/// first index can take minutes, and must not outlive the desktop.
fn refresh(home: Handle, chan: Handle, emb: &mut Embedder, index: &mut Vec<Entry>) -> bool {
    let t = zoo_rt::now_us();
    let mut found = Vec::new();
    files(home, "", 0, &mut found);
    let old = core::mem::take(index);
    let (mut kept, mut embedded) = (0, 0);
    for (path, bytes) in &found {
        let hash = fnv(bytes);
        if old.iter().any(|e| e.path == *path && e.hash == hash) {
            for e in old.iter().filter(|e| e.path == *path && e.hash == hash) {
                index.push(Entry {
                    hash,
                    path: e.path.clone(),
                    line: e.line.clone(),
                    vec: e.vec.clone(),
                });
                kept += 1;
            }
            continue;
        }
        let text = core::str::from_utf8(bytes).unwrap_or("");
        for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
            if index.len() >= MAX_LINES {
                break;
            }
            if zoo_rt::channel_peer(chan).is_err() {
                let _ = log("MagpieHandler: Bella has gone; stopping");
                return false;
            }
            let line: String = line.chars().take(LINE_CHARS).collect();
            // The file's name goes with the line ("notes.txt: Call the
            // vet"); not its folders, which cost tokens and say little.
            let mut doc = String::from(path.rsplit('/').next().unwrap_or(path));
            doc.push_str(": ");
            doc.push_str(&line);
            let Ok(vec) = emb.document(&doc) else {
                continue;
            };
            index.push(Entry {
                hash,
                path: path.clone(),
                line,
                vec,
            });
            embedded += 1;
        }
    }
    if embedded > 0 || index.len() != old.len() {
        save(home, index);
    }
    zoo_rt::log!(
        "MagpieHandler: index: {} files, {} lines ({} embedded, {} kept) in {} ms",
        found.len(),
        index.len(),
        embedded,
        kept,
        (zoo_rt::now_us() - t) / 1000
    );
    true
}

fn put_str(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(&(s.len() as u16).to_le_bytes());
    out.extend_from_slice(s.as_bytes());
}

/// Writes the index: magic, dimensions, count, then per line its file's
/// hash, path, line and vector (little-endian).
fn save(home: Handle, index: &[Entry]) {
    let mut out = Vec::with_capacity(16 + index.len() * (DIM * 4 + 128));
    out.extend_from_slice(INDEX_MAGIC);
    out.extend_from_slice(&(DIM as u32).to_le_bytes());
    out.extend_from_slice(&(index.len() as u32).to_le_bytes());
    for e in index {
        out.extend_from_slice(&e.hash.to_le_bytes());
        put_str(&mut out, &e.path);
        put_str(&mut out, &e.line);
        for x in &e.vec {
            out.extend_from_slice(&x.to_le_bytes());
        }
    }
    let _ = file_create(home, INDEX);
    if file_truncate(home, INDEX, 0).is_err() || file_write(home, INDEX, 0, &out) != Ok(out.len()) {
        let _ = log("MagpieHandler: could not write the index");
    }
}

/// Reads the index, or none when it is missing or not this format.
fn load(home: Handle) -> Vec<Entry> {
    let Ok(e) = dir_stat(home, INDEX) else {
        return Vec::new();
    };
    let mut b = alloc::vec![0u8; e.size as usize];
    match file_read(home, INDEX, 0, &mut b) {
        Ok(n) if n == b.len() => {}
        _ => return Vec::new(),
    }
    parse(&b).unwrap_or_default()
}

/// Reads the index format `save` writes.
struct Cursor<'a> {
    b: &'a [u8],
    at: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let s = self.b.get(self.at..self.at.checked_add(n)?)?;
        self.at += n;
        Some(s)
    }
    fn u16(&mut self) -> Option<usize> {
        Some(u16::from_le_bytes(self.take(2)?.try_into().ok()?) as usize)
    }
    fn u32(&mut self) -> Option<usize> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?) as usize)
    }
    fn string(&mut self) -> Option<String> {
        let n = self.u16()?;
        String::from_utf8(self.take(n)?.to_vec()).ok()
    }
}

fn parse(b: &[u8]) -> Option<Vec<Entry>> {
    let mut c = Cursor { b, at: 0 };
    if c.take(8)? != INDEX_MAGIC {
        return None;
    }
    let (dim, count) = (c.u32()?, c.u32()?);
    if dim != DIM || count > MAX_LINES {
        return None;
    }
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        let hash = u64::from_le_bytes(c.take(8)?.try_into().ok()?);
        let path = c.string()?;
        let line = c.string()?;
        let mut vec = Vec::with_capacity(DIM);
        for x in c.take(DIM * 4)?.as_chunks::<4>().0 {
            vec.push(f32::from_le_bytes(*x));
        }
        out.push(Entry {
            hash,
            path,
            line,
            vec,
        });
    }
    Some(out)
}
