//! BadgerHandler: the filesystem service (docs/s4-design.md; badgers dig
//! burrows of tunnels and chambers, like folders). Holds the folder tree in
//! its own memory: `/bin` (the initrd programs, read-only) and `/home/user`
//! (seeded from the initrd's `home/user/` files, writable). With a data
//! disk, `/home/user` is kept there by ZooFS 2 (docs/linux10c-design.md):
//! file contents live in disk blocks, changed blocks wait in memory until
//! the next save, and each save is one crash-safe generation.
//!
//! Every folder handle a program holds is a channel to BadgerHandler; this
//! side remembers which folder it names and with which rights. Requests and
//! replies use `zoo_abi::fsproto`. Started with a control channel: `root`
//! answers with a root folder channel (all rights) as a handle message
//! (tag `fsproto::TAG_ROOT`); `ping` answers `pong`.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use zoo_abi::{
    DirEntry, Error, KIND_DIR, KIND_FILE, MAX_DIR_NAME, WAIT_ANY_MAX, WAIT_FOREVER, dir_rights,
};
use zoo_rt::fsproto::{self, Op};
use zoo_rt::{Handle, channel_create, close, send, send_handle, try_recv, wait_any};

zoo_rt::entry2!(main);

const MAX_NAME: usize = 255;
const MAX_DEPTH: usize = 128;
const MAX_NODES: usize = 65536;
/// Largest file: 4 GiB (the block map of one costs 8 MiB of memory).
const MAX_FILE: u64 = 1 << 32;
/// Folder channels served at once (one handle each in this process).
const MAX_SESSIONS: usize = WAIT_ANY_MAX - 2;
const BLOCK: usize = zoo_abi::BLOCK_BYTES;
/// Changed blocks held in memory before a save is forced (8 MiB).
const PENDING_LIMIT: usize = 2048;
/// With saves failing (a full disk), writes stop at four times that.
const PENDING_HARD_LIMIT: usize = 4 * PENDING_LIMIT;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Dir,
    File,
    /// A symbolic link (docs/linux10d-design.md): `Content::Link`. The
    /// kernel follows links; BadgerHandler stores them.
    Link,
}

/// A file's contents.
enum Content {
    /// Seeded from the initrd (mapped read-only); a save stores it on disk.
    Static(&'static [u8]),
    /// `size` bytes in 4 KiB blocks: `map[i]` is the disk block holding
    /// file block `i` as last saved, or 0 (zeros, or not saved yet). Newer
    /// contents of a block wait in `Badger::pending`. Bytes past `size` in
    /// the last block are always zero.
    Blocks { size: u64, map: Vec<u64> },
    /// A symbolic link's target.
    Link(String),
}

fn blocks_for(size: u64) -> usize {
    size.div_ceil(BLOCK as u64) as usize
}

struct Node {
    name: String,
    parent: usize,
    kind: Kind,
    children: Vec<usize>,
    content: Content,
    writable: bool,
}

struct Tree {
    nodes: Vec<Option<Node>>,
    /// Slots of removed nodes, reused before the table grows.
    free: Vec<usize>,
}

struct Session {
    chan: Handle,
    node: usize,
    rights: u64,
    /// The client's shared buffer, mapped here (docs/f1-design.md).
    shm: Option<u64>,
}

/// Unmaps a session's shared buffer when it ends.
fn end_session(s: Session) {
    if let Some(addr) = s.shm {
        let _ = zoo_rt::shm_unmap(addr);
    }
    let _ = close(s.chan);
}

/// A name: 1 to 255 bytes of UTF-8 without `/` or NUL, not `.` or `..`,
/// as on Linux (docs/linux10d-design.md; Wine needs `c:` and `Program
/// Files (x86)`).
fn valid_component(c: &str) -> bool {
    !c.is_empty() && c.len() <= MAX_NAME && c != "." && c != ".." && !c.contains('\0')
}

/// Relative only; no empty, `.` or `..` components.
fn components(path: &str) -> Result<Vec<&str>, Error> {
    if path.is_empty() {
        return Ok(Vec::new());
    }
    if path.len() > zoo_abi::MAX_FS_PATH || path.starts_with('/') {
        return Err(Error::Inval);
    }
    let parts: Vec<&str> = path.split('/').collect();
    if parts.len() > MAX_DEPTH || !parts.iter().all(|c| valid_component(c)) {
        return Err(Error::Inval);
    }
    Ok(parts)
}

impl Tree {
    fn node(&self, id: usize) -> Option<&Node> {
        self.nodes.get(id).and_then(Option::as_ref)
    }

    fn node_mut(&mut self, id: usize) -> Option<&mut Node> {
        self.nodes.get_mut(id).and_then(Option::as_mut)
    }

    fn child(&self, dir: usize, name: &str) -> Option<usize> {
        let d = self.node(dir)?;
        // Children are sorted by name.
        d.children
            .binary_search_by(|&c| self.node(c).map_or("", |n| n.name.as_str()).cmp(name))
            .ok()
            .map(|k| d.children[k])
    }

    fn add(
        &mut self,
        parent: usize,
        name: &str,
        kind: Kind,
        content: Content,
    ) -> Result<usize, Error> {
        let writable = self.node(parent).is_none_or(|p| p.writable);
        let node = Node {
            name: String::from(name),
            parent,
            kind,
            children: Vec::new(),
            content,
            writable,
        };
        let id = match self.free.pop() {
            Some(id) => {
                self.nodes[id] = Some(node);
                id
            }
            None if self.nodes.len() < MAX_NODES => {
                self.nodes.push(Some(node));
                self.nodes.len() - 1
            }
            None => return Err(Error::NoMem),
        };
        if id != parent {
            self.link(parent, id);
        }
        Ok(id)
    }

    /// Inserts `id` into `parent`'s children, kept sorted by name.
    fn link(&mut self, parent: usize, id: usize) {
        let name = self.node(id).map(|n| n.name.clone()).unwrap_or_default();
        let at = self.node(parent).map_or(0, |p| {
            p.children
                .partition_point(|&c| self.node(c).is_some_and(|n| n.name < name))
        });
        if let Some(p) = self.node_mut(parent) {
            p.children.insert(at, id);
        }
    }

    fn unlink(&mut self, id: usize) {
        let parent = self.node(id).map_or(usize::MAX, |n| n.parent);
        if let Some(p) = self.node_mut(parent) {
            p.children.retain(|&c| c != id);
        }
    }

    /// Frees `id` and everything below it (already unlinked from its parent).
    fn free_subtree(&mut self, id: usize) {
        let mut stack = vec![id];
        while let Some(n) = stack.pop() {
            if let Some(node) = self.nodes.get_mut(n).and_then(Option::take) {
                stack.extend(node.children);
                self.free.push(n);
            }
        }
    }

    fn resolve(&self, base: usize, path: &str) -> Result<usize, Error> {
        let mut cur = base;
        self.node(cur).ok_or(Error::NotFound)?;
        for c in components(path)? {
            let n = self.node(cur).ok_or(Error::NotFound)?;
            if n.kind != Kind::Dir {
                return Err(Error::NotDir);
            }
            cur = self.child(cur, c).ok_or(Error::NotFound)?;
        }
        Ok(cur)
    }

    /// Parent and final name of a new entry at `path`.
    fn new_entry<'p>(&self, base: usize, path: &'p str) -> Result<(usize, &'p str), Error> {
        let parts = components(path)?;
        let (last, dirs) = parts.split_last().ok_or(Error::Inval)?;
        let mut parent = base;
        for c in dirs {
            parent = self.child(parent, c).ok_or(Error::NotFound)?;
        }
        let p = self.node(parent).ok_or(Error::NotFound)?;
        if p.kind != Kind::Dir {
            return Err(Error::NotDir);
        }
        if !p.writable {
            return Err(Error::Rights);
        }
        if self.child(parent, last).is_some() {
            return Err(Error::Exists);
        }
        Ok((parent, last))
    }

    fn size(&self, id: usize) -> u64 {
        self.node(id).map_or(0, |n| match (n.kind, &n.content) {
            (Kind::Dir, _) => n.children.len() as u64,
            (Kind::File, Content::Static(d)) => d.len() as u64,
            (Kind::File, Content::Blocks { size, .. }) => *size,
            (_, Content::Link(t)) => t.len() as u64,
            (Kind::Link, _) => 0,
        })
    }

    fn entry(&self, id: usize) -> DirEntry {
        let mut e = DirEntry::default();
        if let Some(n) = self.node(id) {
            e.kind = match n.kind {
                Kind::Dir => KIND_DIR,
                Kind::File => KIND_FILE,
                Kind::Link => zoo_abi::KIND_LINK,
            };
            e.size = self.size(id);
            let k = n.name.len().min(MAX_DIR_NAME);
            e.name[..k].copy_from_slice(&n.name.as_bytes()[..k]);
            e.name_len = k as u64;
        }
        e
    }

    fn remove(&mut self, base: usize, path: &str, want: Kind) -> Result<usize, Error> {
        if components(path)?.is_empty() {
            return Err(Error::Inval);
        }
        let id = self.resolve(base, path)?;
        let n = self.node(id).ok_or(Error::NotFound)?;
        // A link goes the way a file does.
        if n.kind != want && !(want == Kind::File && n.kind == Kind::Link) {
            return Err(if want == Kind::Dir {
                Error::NotDir
            } else {
                Error::Inval
            });
        }
        if want == Kind::Dir && !n.children.is_empty() {
            return Err(Error::NotEmpty);
        }
        if !n.writable {
            return Err(Error::Rights);
        }
        self.unlink(id);
        self.free_subtree(id);
        Ok(id)
    }

    fn rename(&mut self, base: usize, from: &str, to: &str) -> Result<(), Error> {
        if components(from)?.is_empty() {
            return Err(Error::Inval);
        }
        let id = self.resolve(base, from)?;
        if !self.node(id).ok_or(Error::NotFound)?.writable {
            return Err(Error::Rights);
        }
        let (parent, name) = self.new_entry(base, to)?;
        let mut a = parent;
        loop {
            if a == id {
                return Err(Error::Inval); // into its own subtree
            }
            let up = self.node(a).ok_or(Error::NotFound)?.parent;
            if up == a {
                break;
            }
            a = up;
        }
        let name = String::from(name);
        self.unlink(id);
        if let Some(n) = self.node_mut(id) {
            n.name = name;
            n.parent = parent;
        }
        self.link(parent, id);
        Ok(())
    }
}

/// Builds `/bin` and `/home/user` from the initrd.
fn seed() -> Tree {
    let mut t = Tree {
        nodes: Vec::new(),
        free: Vec::new(),
    };
    let root = t
        .add(0, "", Kind::Dir, Content::Static(&[]))
        .expect("empty tree");
    let bin = t
        .add(root, "bin", Kind::Dir, Content::Static(&[]))
        .expect("bin");
    if let Some(b) = t.node_mut(bin) {
        b.writable = false;
    }
    let home = t
        .add(root, "home", Kind::Dir, Content::Static(&[]))
        .expect("home");
    let user = t
        .add(home, "user", Kind::Dir, Content::Static(&[]))
        .expect("user");
    let mut index = 0;
    while let Ok(e) = zoo_rt::fs_stat(index) {
        let name = String::from(e.name());
        let current = index;
        index += 1;
        let (dir, rest) = if let Some(r) = name.strip_prefix("bin/") {
            (bin, r)
        } else if let Some(r) = name.strip_prefix("home/user/") {
            (user, r)
        } else {
            continue;
        };
        let Ok(parts) = components(rest) else {
            continue;
        };
        let Some((last, dirs)) = parts.split_last() else {
            continue;
        };
        let mut cur = dir;
        for d in dirs {
            cur = match t.child(cur, d) {
                Some(c) => c,
                None => match t.add(cur, d, Kind::Dir, Content::Static(&[])) {
                    Ok(c) => c,
                    Err(_) => break,
                },
            };
        }
        let Ok(data) = zoo_rt::fs_map(current) else {
            continue;
        };
        if t.child(cur, last).is_none() {
            let _ = t.add(cur, last, Kind::File, Content::Static(data));
        }
    }
    t
}

/// Changed file blocks: buffers in fixed 256 KiB chunks (which never move,
/// so the slab grows without copying, and a block costs no allocation of
/// its own) and, per file, the slot of each block (slot + 1; 0 for none).
#[derive(Default)]
struct Pending {
    /// Each `CHUNK_BLOCKS` long, allocated on the heap directly.
    chunks: Vec<Vec<[u8; BLOCK]>>,
    /// Slots handed out so far (the rest of the last chunk is unused).
    used: usize,
    free: Vec<u32>,
    files: BTreeMap<usize, Vec<u32>>,
    count: usize,
}

/// Blocks per slab chunk.
const CHUNK_BLOCKS: usize = 64;

impl Pending {
    fn slot(&self, id: usize, idx: u64) -> Option<usize> {
        let s = *self.files.get(&id)?.get(idx as usize)?;
        (s != 0).then(|| s as usize - 1)
    }

    fn buf(&self, s: usize) -> &[u8; BLOCK] {
        &self.chunks[s / CHUNK_BLOCKS][s % CHUNK_BLOCKS]
    }

    fn buf_mut(&mut self, s: usize) -> &mut [u8; BLOCK] {
        &mut self.chunks[s / CHUNK_BLOCKS][s % CHUNK_BLOCKS]
    }

    fn get(&self, id: usize, idx: u64) -> Option<&[u8; BLOCK]> {
        self.slot(id, idx).map(|s| self.buf(s))
    }

    fn has(&self, id: usize, idx: u64) -> bool {
        self.slot(id, idx).is_some()
    }

    fn len(&self) -> usize {
        self.count
    }

    /// The buffer for block `idx` of file `id`, and whether it is new. A
    /// new buffer is zeroed unless `whole` (the caller overwrites it all).
    fn entry(&mut self, id: usize, idx: u64, whole: bool) -> (&mut [u8; BLOCK], bool) {
        if let Some(s) = self.slot(id, idx) {
            return (self.buf_mut(s), false);
        }
        let s = match self.free.pop() {
            Some(s) => s as usize,
            None => {
                if self.used == self.chunks.len() * CHUNK_BLOCKS {
                    self.chunks.push(vec![[0; BLOCK]; CHUNK_BLOCKS]);
                }
                self.used += 1;
                self.used - 1
            }
        };
        if !whole {
            self.buf_mut(s).fill(0);
        }
        let slots = self.files.entry(id).or_default();
        if slots.len() <= idx as usize {
            slots.resize(idx as usize + 1, 0);
        }
        slots[idx as usize] = s as u32 + 1;
        self.count += 1;
        (self.buf_mut(s), true)
    }

    /// Drops file `id`'s blocks from index `keep` on.
    fn drop_from(&mut self, id: usize, keep: u64) {
        let Some(slots) = self.files.get_mut(&id) else {
            return;
        };
        for s in slots.iter_mut().skip(keep as usize) {
            if *s != 0 {
                self.free.push(*s - 1);
                self.count -= 1;
                *s = 0;
            }
        }
        slots.truncate(keep as usize);
        if slots.is_empty() {
            self.files.remove(&id);
        }
    }

    /// Empties it, keeping up to `PENDING_LIMIT` blocks of chunks for reuse.
    fn clear(&mut self) {
        self.files.clear();
        self.free.clear();
        self.count = 0;
        self.used = 0;
        self.chunks.truncate(PENDING_LIMIT / CHUNK_BLOCKS);
    }
}

struct Badger {
    tree: Tree,
    sessions: Vec<Session>,
    /// The data disk, when the supervisor gave us one.
    store: Option<Store>,
    /// Newer contents of file blocks, waiting for the next save (or,
    /// without a disk, the only copy).
    pending: Pending,
    /// Tick of the first unsaved change, if any.
    changed_since: Option<u64>,
    /// Test hook: the next save stops before its header (docs/s5-design.md §4).
    crash_next_save: bool,
}

impl Badger {
    /// A new folder channel for `node` with `rights`; returns the client end.
    fn open_session(&mut self, node: usize, rights: u64) -> Result<Handle, Error> {
        if self.sessions.len() >= MAX_SESSIONS {
            return Err(Error::NoMem);
        }
        let (mine, theirs) = channel_create()?;
        self.sessions.push(Session {
            chan: mine,
            node,
            rights,
            shm: None,
        });
        Ok(theirs)
    }

    /// Copies bytes `start..start + out.len()` of file `id` into `out`
    /// (the caller limited the range to the file).
    fn read_range(&self, id: usize, start: u64, out: &mut [u8]) -> Result<(), Error> {
        let n = self.tree.node(id).ok_or(Error::NotFound)?;
        let map = match &n.content {
            Content::Static(d) => {
                let s = start as usize;
                out.copy_from_slice(&d[s..s + out.len()]);
                return Ok(());
            }
            Content::Blocks { map, .. } => map,
            Content::Link(_) => return Err(Error::Inval),
        };
        let mut done = 0usize;
        while done < out.len() {
            let pos = start + done as u64;
            let idx = pos / BLOCK as u64;
            let off = (pos % BLOCK as u64) as usize;
            if let Some(b) = self.pending.get(id, idx) {
                let k = (BLOCK - off).min(out.len() - done);
                out[done..done + k].copy_from_slice(&b[off..off + k]);
                done += k;
                continue;
            }
            let disk_block = map.get(idx as usize).copied().unwrap_or(0);
            if disk_block == 0 {
                let k = (BLOCK - off).min(out.len() - done);
                out[done..done + k].fill(0);
                done += k;
                continue;
            }
            // A run of saved blocks, consecutive on disk too, in one read.
            let last_idx = (start + out.len() as u64 - 1) / BLOCK as u64;
            let mut run = 1u64;
            while run < zoo_abi::BLOCK_RUN_MAX as u64
                && idx + run <= last_idx
                && map.get((idx + run) as usize) == Some(&(disk_block + run))
                && !self.pending.has(id, idx + run)
            {
                run += 1;
            }
            let store = self.store.as_ref().ok_or(Error::Fault)?;
            let bytes = run as usize * BLOCK;
            if off == 0 && out.len() - done >= bytes {
                // Whole blocks: straight into the caller's buffer.
                store.read_run(disk_block, &mut out[done..done + bytes])?;
                done += bytes;
                continue;
            }
            let mut buf = vec![0u8; bytes];
            store.read_run(disk_block, &mut buf)?;
            let k = (buf.len() - off).min(out.len() - done);
            out[done..done + k].copy_from_slice(&buf[off..off + k]);
            done += k;
        }
        Ok(())
    }

    /// Makes file `id` writable block by block: initrd contents become
    /// pending blocks.
    fn own_blocks(&mut self, id: usize) -> Result<(), Error> {
        let n = self.tree.node_mut(id).ok_or(Error::NotFound)?;
        if n.kind != Kind::File {
            return Err(Error::Inval);
        }
        if !n.writable {
            return Err(Error::Rights);
        }
        if let Content::Static(d) = n.content {
            n.content = Content::Blocks {
                size: d.len() as u64,
                map: vec![0; blocks_for(d.len() as u64)],
            };
            for (k, chunk) in d.chunks(BLOCK).enumerate() {
                self.pending.entry(id, k as u64, false).0[..chunk.len()].copy_from_slice(chunk);
            }
        }
        Ok(())
    }

    /// The pending copy of block `idx` of file `id`, made from its saved
    /// contents (or zeros) when there is none yet.
    fn pending_block(&mut self, id: usize, idx: u64) -> Result<&mut [u8; BLOCK], Error> {
        let disk_block = match self.tree.node(id).map(|n| &n.content) {
            Some(Content::Blocks { map, .. }) => map.get(idx as usize).copied().unwrap_or(0),
            _ => 0,
        };
        let (b, new) = self.pending.entry(id, idx, false);
        if new && disk_block != 0 {
            let store = self.store.as_ref().ok_or(Error::Fault)?;
            store.read_run(disk_block, &mut b[..])?;
        }
        Ok(b)
    }

    /// Too much is waiting for a disk that keeps refusing saves.
    fn pending_full(&self) -> bool {
        self.store.is_some() && self.pending.len() >= PENDING_HARD_LIMIT
    }

    /// Writes `data` at `offset` into file `id`, growing it.
    fn write_range(&mut self, id: usize, offset: u64, data: &[u8]) -> Result<(), Error> {
        let end = offset
            .checked_add(data.len() as u64)
            .filter(|&e| e <= MAX_FILE)
            .ok_or(Error::Inval)?;
        if self.pending_full() {
            return Err(Error::NoMem);
        }
        self.own_blocks(id)?;
        if let Some(Content::Blocks { size, map }) = self.tree.node_mut(id).map(|n| &mut n.content)
            && end > *size
        {
            *size = end;
            map.resize(blocks_for(end), 0);
        }
        let mut done = 0usize;
        while done < data.len() {
            let pos = offset + done as u64;
            let idx = pos / BLOCK as u64;
            let off = (pos % BLOCK as u64) as usize;
            let k = (BLOCK - off).min(data.len() - done);
            let b = if off == 0 && k == BLOCK {
                // A whole block: no need to read the old one.
                self.pending.entry(id, idx, true).0
            } else {
                self.pending_block(id, idx)?
            };
            b[off..off + k].copy_from_slice(&data[done..done + k]);
            done += k;
        }
        Ok(())
    }

    /// Sets file `id`'s size to `len`.
    fn truncate(&mut self, id: usize, len: u64) -> Result<(), Error> {
        if len > MAX_FILE {
            return Err(Error::Inval);
        }
        self.own_blocks(id)?;
        let old = self.tree.size(id);
        if len < old {
            let keep = blocks_for(len) as u64;
            self.pending.drop_from(id, keep);
            let tail = (len % BLOCK as u64) as usize;
            if tail != 0 {
                // Bytes past the end of the last block read as zero.
                self.pending_block(id, len / BLOCK as u64)?[tail..].fill(0);
            }
        }
        if let Some(Content::Blocks { size, map }) = self.tree.node_mut(id).map(|n| &mut n.content)
        {
            *size = len;
            map.resize(blocks_for(len), 0);
        }
        Ok(())
    }

    /// Handles one request on session `k`. Returns false to drop the
    /// session (the client is gone).
    fn request(&mut self, k: usize, msg: &[u8]) -> bool {
        let chan = self.sessions[k].chan;
        let Some(req) = fsproto::Request::decode(msg) else {
            return fsproto::reply(chan, Err(Error::Inval), &[]).is_ok();
        };
        let (base, rights) = (self.sessions[k].node, self.sessions[k].rights);
        let need = match req.op {
            Op::Open => 0,
            Op::CreateDir | Op::CreateFile | Op::CreateLink => dir_rights::CREATE,
            Op::RemoveDir | Op::RemoveFile => dir_rights::REMOVE,
            Op::Write | Op::Truncate | Op::WriteBulk => dir_rights::WRITE,
            Op::Rename => dir_rights::CREATE | dir_rights::REMOVE,
            Op::List | Op::Stat | Op::Read | Op::ReadBulk | Op::ReadLink | Op::ListLinks => {
                dir_rights::READ
            }
        };
        if rights & need != need {
            return fsproto::reply(chan, Err(Error::Rights), &[]).is_ok();
        }
        let path = req.path;
        let mut out: Vec<u8> = Vec::new();
        let result: Result<u64, Error> = match req.op {
            Op::Open => {
                let new_rights = req.arg;
                if new_rights & !dir_rights::ALL != 0 || new_rights & !rights != 0 {
                    Err(Error::Rights)
                } else {
                    let t = &self.tree;
                    match t.resolve(base, path) {
                        Ok(id) if t.node(id).is_some_and(|n| n.kind == Kind::Dir) => {
                            match self.open_session(id, new_rights) {
                                Ok(client) => {
                                    // The status first, then the handle.
                                    if fsproto::reply(chan, Ok(0), &[]).is_err() {
                                        let _ = close(client);
                                        return false;
                                    }
                                    if send_handle(chan, client, fsproto::TAG_DIR).is_err() {
                                        let _ = close(client);
                                    }
                                    return true;
                                }
                                Err(e) => Err(e),
                            }
                        }
                        Ok(_) => Err(Error::NotDir),
                        Err(e) => Err(e),
                    }
                }
            }
            Op::List => {
                let t = &self.tree;
                t.resolve(base, path).and_then(|id| {
                    let n = t.node(id).ok_or(Error::NotFound)?;
                    if n.kind != Kind::Dir {
                        return Err(Error::NotDir);
                    }
                    let c = *n.children.get(req.arg as usize).ok_or(Error::NotFound)?;
                    fsproto::encode_entry(&t.entry(c), &mut out);
                    Ok(0)
                })
            }
            Op::Stat => self.tree.resolve(base, path).map(|id| {
                let mut e = self.tree.entry(id);
                e.name_len = 0;
                fsproto::encode_entry(&e, &mut out);
                0
            }),
            Op::Read => self.tree.resolve(base, path).and_then(|id| {
                if self.tree.node(id).is_none_or(|n| n.kind != Kind::File) {
                    return Err(Error::Inval);
                }
                let size = self.tree.size(id);
                let start = req.arg.min(size);
                let want = (req.count as u64).min(fsproto::MAX_DATA as u64);
                let end = (start + want).min(size);
                out.resize((end - start) as usize, 0);
                self.read_range(id, start, &mut out)?;
                Ok(end - start)
            }),
            Op::CreateDir => {
                let t = &mut self.tree;
                t.new_entry(base, path)
                    .and_then(|(p, name)| t.add(p, name, Kind::Dir, Content::Static(&[])))
                    .map(|_| 0)
            }
            Op::CreateFile => {
                let t = &mut self.tree;
                t.new_entry(base, path)
                    .and_then(|(p, name)| {
                        t.add(
                            p,
                            name,
                            Kind::File,
                            Content::Blocks {
                                size: 0,
                                map: Vec::new(),
                            },
                        )
                    })
                    .map(|_| 0)
            }
            Op::CreateLink => match core::str::from_utf8(req.data) {
                Ok(target) if !target.is_empty() && target.len() <= zoo_abi::MAX_FS_PATH => {
                    let t = &mut self.tree;
                    t.new_entry(base, path)
                        .and_then(|(p, name)| {
                            t.add(p, name, Kind::Link, Content::Link(String::from(target)))
                        })
                        .map(|_| 0)
                }
                _ => Err(Error::Inval),
            },
            Op::ReadLink => self.tree.resolve(base, path).and_then(|id| {
                match self.tree.node(id).map(|n| &n.content) {
                    Some(Content::Link(t)) => {
                        out.extend_from_slice(t.as_bytes());
                        Ok(t.len() as u64)
                    }
                    _ => Err(Error::Inval),
                }
            }),
            Op::ListLinks => self.tree.resolve(base, path).and_then(|dir| {
                // Depth-first, in name order: the same order every time.
                let mut stack = vec![(dir, String::new())];
                let mut seen = 0u64;
                while let Some((d, prefix)) = stack.pop() {
                    let Some(n) = self.tree.node(d) else { continue };
                    for &c in n.children.iter().rev() {
                        let Some(cn) = self.tree.node(c) else {
                            continue;
                        };
                        let mut p = prefix.clone();
                        if !p.is_empty() {
                            p.push('/');
                        }
                        p.push_str(&cn.name);
                        match &cn.content {
                            _ if cn.kind == Kind::Dir => stack.push((c, p)),
                            Content::Link(t) => {
                                if seen == req.arg {
                                    out.extend_from_slice(p.as_bytes());
                                    out.push(0);
                                    out.extend_from_slice(t.as_bytes());
                                    return Ok(out.len() as u64);
                                }
                                seen += 1;
                            }
                            _ => {}
                        }
                    }
                }
                Err(Error::NotFound)
            }),
            Op::RemoveDir => self.tree.remove(base, path, Kind::Dir).map(|_| 0),
            Op::RemoveFile => self.tree.remove(base, path, Kind::File).map(|id| {
                // Its waiting blocks go with it (the slot may be reused).
                self.pending.drop_from(id, 0);
                0
            }),
            Op::Write => self
                .tree
                .resolve(base, path)
                .and_then(|id| self.write_range(id, req.arg, req.data))
                .map(|_| req.data.len() as u64),
            Op::Truncate => self
                .tree
                .resolve(base, path)
                .and_then(|id| self.truncate(id, req.arg))
                .map(|_| 0),
            Op::Rename => match core::str::from_utf8(req.data) {
                Ok(to) => self.tree.rename(base, path, to).map(|_| 0),
                Err(_) => Err(Error::Inval),
            },
            // Bulk transfers through the session's shared buffer; without
            // one, AGAIN (docs/f1-design.md).
            Op::ReadBulk | Op::WriteBulk => {
                let len = req
                    .data
                    .get(..4)
                    .and_then(|b| b.try_into().ok())
                    .map_or(usize::MAX, |b| u32::from_le_bytes(b) as usize);
                match self.sessions[k].shm {
                    None => Err(Error::Again),
                    Some(_) if len > fsproto::BULK_SIZE => Err(Error::Inval),
                    Some(shared) => {
                        let offset = req.arg;
                        if req.op == Op::ReadBulk {
                            self.tree.resolve(base, path).and_then(|id| {
                                if self.tree.node(id).is_none_or(|n| n.kind != Kind::File) {
                                    return Err(Error::Inval);
                                }
                                let size = self.tree.size(id);
                                let start = offset.min(size);
                                let end = (start + len as u64).min(size);
                                // SAFETY: the client waits for this reply
                                // before touching the buffer again; `end -
                                // start <= len <= BULK_SIZE` fits in it.
                                let dst = unsafe {
                                    core::slice::from_raw_parts_mut(
                                        shared as *mut u8,
                                        (end - start) as usize,
                                    )
                                };
                                self.read_range(id, start, dst)?;
                                Ok(end - start)
                            })
                        } else {
                            // SAFETY: the client filled the buffer and waits
                            // for this reply; `len` fits in it.
                            let src =
                                unsafe { core::slice::from_raw_parts(shared as *const u8, len) };
                            self.tree
                                .resolve(base, path)
                                .and_then(|id| self.write_range(id, offset, src))
                                .map(|_| len as u64)
                        }
                    }
                }
            }
        };
        let changes = matches!(
            req.op,
            Op::CreateDir
                | Op::CreateFile
                | Op::CreateLink
                | Op::RemoveDir
                | Op::RemoveFile
                | Op::Write
                | Op::WriteBulk
                | Op::Truncate
                | Op::Rename
        );
        if changes && result.is_ok() && self.store.is_some() && self.changed_since.is_none() {
            self.changed_since = Some(zoo_rt::ticks());
        }
        fsproto::reply(chan, result, &out).is_ok()
    }

    /// Saves once changes have been quiet for `SAVE_DELAY`, or at once when
    /// `PENDING_LIMIT` blocks are waiting. Returns false when the test hook
    /// stops the process mid-save.
    fn maybe_save(&mut self) -> bool {
        let Some(since) = self.changed_since else {
            return true;
        };
        if zoo_rt::ticks() < since + SAVE_DELAY && self.pending.len() < PENDING_LIMIT {
            return true;
        }
        let crash = core::mem::take(&mut self.crash_next_save);
        match self.save(crash) {
            Saved::Done => {
                self.changed_since = None;
                true
            }
            // Retried after the next quiet period.
            Saved::Failed => {
                self.changed_since = Some(zoo_rt::ticks());
                true
            }
            Saved::Stopped => false,
        }
    }
}

fn main(control: u64, disk: u64) -> u8 {
    let mut b = Badger {
        tree: seed(),
        sessions: Vec::new(),
        store: None,
        pending: Pending::default(),
        changed_since: None,
        crash_next_save: false,
    };
    if disk != zoo_rt::NO_HANDLE {
        let size = zoo_rt::block_size(disk).unwrap_or(0);
        let blocks = size / BLOCK as u64;
        if blocks >= MIN_DISK_BLOCKS {
            let (store, current) = Store::mount(disk, blocks);
            b.store = Some(store);
            match current {
                Some(meta) => {
                    if !b.restore_home(&meta) {
                        let _ =
                            zoo_rt::log("BadgerHandler: snapshot did not parse; using the seed");
                        b.tree = seed();
                    }
                }
                None => {
                    let _ = zoo_rt::log(
                        "BadgerHandler: no valid snapshot; starting from the initrd seed",
                    );
                }
            }
        } else {
            let _ = zoo_rt::log("BadgerHandler: data disk too small; files are not kept");
        }
    }
    let mut buf = vec![0u8; zoo_abi::MAX_MESSAGE];
    loop {
        // Control channel first, then every folder channel.
        let mut watch = Vec::with_capacity(1 + b.sessions.len());
        watch.push(control);
        watch.extend(b.sessions.iter().map(|s| s.chan));
        let timeout = match b.changed_since {
            Some(since) => (since + SAVE_DELAY).saturating_sub(zoo_rt::ticks()).max(1),
            None => WAIT_FOREVER,
        };
        let _ = wait_any(&watch, timeout);
        if !b.maybe_save() {
            return 41; // test hook: "power lost" mid-save
        }
        match try_recv(control, &mut buf) {
            Ok(n) if &buf[..n] == b"ping" => {
                if send(control, b"pong").is_err() {
                    return 0;
                }
            }
            Ok(n) if &buf[..n] == b"half-save" => b.crash_next_save = true,
            Ok(n) if &buf[..n] == b"root" => match b.open_session(0, dir_rights::ALL) {
                Ok(client) => {
                    if send_handle(control, client, fsproto::TAG_ROOT).is_err() {
                        let _ = close(client);
                    }
                }
                Err(_) => {
                    let _ = send(control, b"err no room");
                }
            },
            Ok(_) | Err(Error::Again) | Err(Error::WrongKind) => {}
            Err(_) => return 0, // whoever started us is gone
        }
        let mut k = 0;
        while k < b.sessions.len() {
            match try_recv(b.sessions[k].chan, &mut buf) {
                Ok(n) => {
                    let msg = buf[..n].to_vec();
                    if !b.request(k, &msg) {
                        end_session(b.sessions.swap_remove(k));
                        continue;
                    }
                    // A large write may need a save before the next one.
                    if b.pending.len() >= PENDING_LIMIT && !b.maybe_save() {
                        return 41;
                    }
                }
                Err(Error::Again) => {}
                Err(Error::WrongKind) => {
                    // A handle: the client's shared buffer.
                    if let Ok((h, tag)) = zoo_rt::try_recv_handle(b.sessions[k].chan) {
                        if tag == fsproto::TAG_SHM
                            && let Ok(addr) = zoo_rt::shm_map(h)
                            && let Some(old) = b.sessions[k].shm.replace(addr)
                        {
                            let _ = zoo_rt::shm_unmap(old);
                        }
                        // The mapping keeps the pages; the handle can go.
                        let _ = close(h);
                    }
                    continue;
                }
                Err(_) => {
                    // The client closed its folder handle.
                    end_session(b.sessions.swap_remove(k));
                    continue;
                }
            }
            k += 1;
        }
    }
}

// ---- ZooFS 2 on the data disk (docs/linux10c-design.md) ------------------
//
// Block 0 and block 1 are the two superblocks, A and B. Each names one
// generation: its number, and where its metadata (the `/home/user` tree,
// with each file's block runs) lies, with CRC-32s over both. Saves
// alternate between them; the superblock is written last, so a save cut
// short leaves the previous generation whole. File data and metadata are
// always written to blocks that neither the current nor the previous
// generation uses, so either can be loaded.

const SB_MAGIC: &[u8; 8] = b"ZOOFS002";
/// The format before ZooFS 2 (docs/s5-design.md); recognised, not read.
const OLD_MAGIC: &[u8; 8] = b"ZOOFS001";
/// Metadata format version.
const META_VERSION: u32 = 1;
/// Ticks after the last change before a save.
const SAVE_DELAY: u64 = 50;
/// Two superblocks and at least a few data blocks.
const MIN_DISK_BLOCKS: u64 = 16;
/// Metadata runs one superblock can list (12 bytes each, after a 40-byte
/// header, before the CRC in the last 4 bytes).
const MAX_META_RUNS: usize = (BLOCK - 44) / 12;

const REC_DIR: u8 = 1;
const REC_FILE: u8 = 2;
/// A symbolic link: its target follows the name (2-byte length, bytes).
const REC_LINK: u8 = 3;

/// Standard CRC-32 (IEEE, reflected).
fn crc32(data: &[u8]) -> u32 {
    let mut c = 0xFFFF_FFFFu32;
    for &b in data {
        c ^= u32::from(b);
        for _ in 0..8 {
            c = (c >> 1) ^ (0xEDB8_8320 & 0u32.wrapping_sub(c & 1));
        }
    }
    !c
}

fn slot_name(slot: u64) -> &'static str {
    if slot == 0 { "A" } else { "B" }
}

enum Saved {
    Done,
    /// A write failed or the disk is full: nothing changed on disk that the
    /// current generation needs; retried later.
    Failed,
    /// The test hook stopped the save before its superblock.
    Stopped,
}

/// A loaded generation's metadata and the blocks it uses.
struct Generation {
    slot: u64,
    number: u64,
    meta: Vec<u8>,
}

struct Store {
    disk: Handle,
    blocks: u64,
    /// The newest saved generation: (superblock slot, number).
    current: Option<(u64, u64)>,
    /// Blocks the current and the previous generation use.
    live: Vec<u64>,
    live_prev: Vec<u64>,
    /// One bit per block: in `live`, `live_prev`, or taken by the save in
    /// progress.
    used: Vec<u64>,
    /// Where the next allocation of the save in progress starts looking
    /// (each save starts at block 2).
    cursor: u64,
}

impl Store {
    fn read_run(&self, block: u64, buf: &mut [u8]) -> Result<(), Error> {
        for (k, chunk) in buf.chunks_mut(zoo_abi::BLOCK_RUN_MAX * BLOCK).enumerate() {
            let at = block + (k * zoo_abi::BLOCK_RUN_MAX) as u64;
            zoo_rt::block_read_run(self.disk, at, chunk)?;
        }
        Ok(())
    }

    fn write_run(&self, block: u64, buf: &[u8]) -> Result<(), Error> {
        for (k, chunk) in buf.chunks(zoo_abi::BLOCK_RUN_MAX * BLOCK).enumerate() {
            let at = block + (k * zoo_abi::BLOCK_RUN_MAX) as u64;
            zoo_rt::block_write_run(self.disk, at, chunk)?;
        }
        Ok(())
    }

    fn is_used(&self, b: u64) -> bool {
        self.used[(b / 64) as usize] >> (b % 64) & 1 != 0
    }

    fn mark(&mut self, b: u64) {
        self.used[(b / 64) as usize] |= 1 << (b % 64);
    }

    /// Recomputes `used` from the two live lists.
    fn rebuild_used(&mut self) {
        self.used = vec![0; self.blocks.div_ceil(64) as usize];
        self.mark(0);
        self.mark(1);
        for k in 0..self.live.len() {
            let b = self.live[k];
            self.mark(b);
        }
        for k in 0..self.live_prev.len() {
            let b = self.live_prev[k];
            self.mark(b);
        }
    }

    /// Up to `want` free consecutive blocks, the first free ones from
    /// `cursor` on; marks them used. Returns (first, count), or `None` when
    /// the disk is full. Whole used words of the bitmap are skipped.
    fn alloc(&mut self, want: usize) -> Option<(u64, usize)> {
        let mut b = self.cursor.max(2);
        loop {
            if b >= self.blocks {
                return None;
            }
            if b.is_multiple_of(64) && self.used[(b / 64) as usize] == u64::MAX {
                b += 64;
                continue;
            }
            if !self.is_used(b) {
                break;
            }
            b += 1;
        }
        let mut n = 1;
        while n < want && b + (n as u64) < self.blocks && !self.is_used(b + n as u64) {
            n += 1;
        }
        for k in 0..n as u64 {
            self.mark(b + k);
        }
        self.cursor = b + n as u64;
        Some((b, n))
    }

    /// Reads one superblock and its metadata; `None` (logged) when it is
    /// empty or fails its checks.
    fn load_slot(&self, slot: u64) -> Option<Generation> {
        let name = slot_name(slot);
        let mut h = [0u8; BLOCK];
        if zoo_rt::block_read(self.disk, slot, &mut h).is_err() {
            zoo_rt::log!("BadgerHandler: slot {}: read failed", name);
            return None;
        }
        if &h[..8] == OLD_MAGIC {
            zoo_rt::log!(
                "BadgerHandler: slot {}: older format (ZOOFS001), not read",
                name
            );
            return None;
        }
        if &h[..8] != SB_MAGIC {
            zoo_rt::log!("BadgerHandler: slot {}: empty", name);
            return None;
        }
        let word = |at: usize| u64::from_le_bytes(h[at..at + 8].try_into().unwrap_or_default());
        let half = |at: usize| u32::from_le_bytes(h[at..at + 4].try_into().unwrap_or_default());
        let number = word(8);
        let failed = || {
            zoo_rt::log!(
                "BadgerHandler: slot {}: generation {} failed its check, ignored",
                name,
                number
            );
            None
        };
        if half(BLOCK - 4) != crc32(&h[..BLOCK - 4]) || word(16) != self.blocks {
            return failed();
        }
        let (meta_len, meta_crc, runs) = (word(24), half(32), half(36) as usize);
        if runs > MAX_META_RUNS || meta_len > MAX_FILE {
            return failed();
        }
        let mut meta = vec![0u8; (meta_len as usize).div_ceil(BLOCK) * BLOCK];
        let mut at = 0usize;
        for r in 0..runs {
            let o = 40 + 12 * r;
            let (first, count) = (word(o), half(o + 8) as usize);
            let end = at + count * BLOCK;
            if first < 2
                || first.saturating_add(count as u64) > self.blocks
                || end > meta.len()
                || self.read_run(first, &mut meta[at..end]).is_err()
            {
                return failed();
            }
            at = end;
        }
        meta.truncate(meta_len as usize);
        if crc32(&meta) != meta_crc || parse_meta(&meta, self.blocks, &mut |_| {}).is_none() {
            return failed();
        }
        Some(Generation { slot, number, meta })
    }

    /// Opens the disk: the newest valid generation (its metadata) and,
    /// kept safe from new writes, the one before it.
    fn mount(disk: Handle, blocks: u64) -> (Store, Option<Vec<u8>>) {
        let mut s = Store {
            disk,
            blocks,
            current: None,
            live: Vec::new(),
            live_prev: Vec::new(),
            used: Vec::new(),
            cursor: 2,
        };
        let mut found: Vec<Generation> = [0, 1].iter().filter_map(|&k| s.load_slot(k)).collect();
        found.sort_by_key(|g| core::cmp::Reverse(g.number));
        let mut current = None;
        if let Some(g) = found.first() {
            let mut live = Vec::new();
            let files = parse_meta(&g.meta, blocks, &mut |b| live.push(b)).unwrap_or(0);
            s.live = live;
            s.live.extend(meta_blocks(&s, g.slot));
            s.current = Some((g.slot, g.number));
            zoo_rt::log!(
                "BadgerHandler: loaded generation {} from slot {} ({} files, {} blocks)",
                g.number,
                slot_name(g.slot),
                files,
                s.live.len()
            );
            current = Some(g.meta.clone());
        }
        if let Some(g) = found.get(1) {
            let mut prev = Vec::new();
            let _ = parse_meta(&g.meta, blocks, &mut |b| prev.push(b));
            prev.extend(meta_blocks(&s, g.slot));
            s.live_prev = prev;
        }
        s.rebuild_used();
        (s, current)
    }
}

/// The metadata blocks superblock `slot` lists (it was checked).
fn meta_blocks(s: &Store, slot: u64) -> Vec<u64> {
    let mut h = [0u8; BLOCK];
    let mut out = Vec::new();
    if zoo_rt::block_read(s.disk, slot, &mut h).is_err() {
        return out;
    }
    let word = |at: usize| u64::from_le_bytes(h[at..at + 8].try_into().unwrap_or_default());
    let half = |at: usize| u32::from_le_bytes(h[at..at + 4].try_into().unwrap_or_default());
    for r in 0..(half(36) as usize).min(MAX_META_RUNS) {
        let o = 40 + 12 * r;
        for k in 0..u64::from(half(o + 8)) {
            out.push(word(o) + k);
        }
    }
    out
}

/// Checks metadata: the version, then one directory record (`/home/user`)
/// and everything below it. Calls `block` for every data block a file uses
/// and returns the number of files, or `None` if it does not parse or names
/// a block outside the disk.
fn parse_meta(meta: &[u8], blocks: u64, block: &mut dyn FnMut(u64)) -> Option<usize> {
    let mut r = Reader { data: meta, at: 0 };
    if r.u32()? != META_VERSION {
        return None;
    }
    let mut files = 0;
    // Records still to read in each open directory.
    let mut stack = vec![1u32];
    while let Some(top) = stack.last_mut() {
        if *top == 0 {
            stack.pop();
            continue;
        }
        *top -= 1;
        let kind = r.u8()?;
        let name_len = r.u16()? as usize;
        let name = r.bytes(name_len)?;
        core::str::from_utf8(name).ok()?;
        match kind {
            REC_DIR => {
                let n = r.u32()?;
                if stack.len() > 4 * MAX_DEPTH {
                    return None;
                }
                stack.push(n);
            }
            REC_LINK => {
                let len = r.u16()? as usize;
                core::str::from_utf8(r.bytes(len)?).ok()?;
            }
            REC_FILE => {
                files += 1;
                let size = r.u64()?;
                if size > MAX_FILE {
                    return None;
                }
                let runs = r.u32()?;
                let mut total = 0u64;
                for _ in 0..runs {
                    let first = r.u64()?;
                    let count = u64::from(r.u32()?);
                    total = total.checked_add(count)?;
                    if first != 0 {
                        if first < 2 || first.checked_add(count)? > blocks {
                            return None;
                        }
                        for k in 0..count {
                            block(first + k);
                        }
                    }
                }
                if total != blocks_for(size) as u64 {
                    return None;
                }
            }
            _ => return None,
        }
    }
    (r.at == meta.len()).then_some(files)
}

struct Reader<'a> {
    data: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn bytes(&mut self, n: usize) -> Option<&[u8]> {
        let s = self.data.get(self.at..self.at.checked_add(n)?)?;
        self.at += n;
        Some(s)
    }
    fn u8(&mut self) -> Option<u8> {
        Some(self.bytes(1)?[0])
    }
    fn u16(&mut self) -> Option<u16> {
        Some(u16::from_le_bytes(self.bytes(2)?.try_into().ok()?))
    }
    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.bytes(4)?.try_into().ok()?))
    }
    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.bytes(8)?.try_into().ok()?))
    }
}

impl Badger {
    /// Writes the next generation: changed data blocks, then the metadata,
    /// then (last) the superblock in the other slot.
    fn save(&mut self, crash_before_header: bool) -> Saved {
        let Some(home) = self.tree.resolve(0, "home/user").ok() else {
            return Saved::Failed;
        };
        // Initrd files in the home folder are stored like any other.
        let mut stack = vec![home];
        let mut seeded = Vec::new();
        while let Some(d) = stack.pop() {
            for &c in self.tree.node(d).map_or(&[][..], |n| &n.children) {
                match self.tree.node(c).map(|n| (n.kind, &n.content)) {
                    Some((Kind::Dir, _)) => stack.push(c),
                    Some((Kind::File, Content::Static(_))) => seeded.push(c),
                    _ => {}
                }
            }
        }
        for id in seeded {
            let _ = self.own_blocks(id);
        }
        let Some(store) = self.store.as_mut() else {
            return Saved::Done;
        };
        let next = store.current.map_or(1, |(_, g)| g + 1);
        // First fit from the start of the disk: data stays compact, and
        // blocks freed by older generations are reused first.
        store.cursor = 2;
        let slot = match store.current {
            Some((0, _)) => 1,
            _ => 0,
        };
        let fail = |store: &mut Store, why: &str| {
            zoo_rt::log!("BadgerHandler: {}; generation {} not saved", why, next);
            store.rebuild_used();
            Saved::Failed
        };
        // 1. Data: each run of consecutive changed blocks of a file goes to
        //    free blocks, consecutive where possible.
        let keys: Vec<(usize, u64)> = self
            .pending
            .files
            .iter()
            .flat_map(|(&id, slots)| {
                slots
                    .iter()
                    .enumerate()
                    .filter(|(_, s)| **s != 0)
                    .map(move |(k, _)| (id, k as u64))
            })
            .collect();
        let mut written = 0usize;
        let mut k = 0;
        while k < keys.len() {
            let (id, first_idx) = keys[k];
            let mut run = 1;
            while k + run < keys.len() && keys[k + run] == (id, first_idx + run as u64) {
                run += 1;
            }
            let mut done = 0;
            while done < run {
                let Some((at, n)) = store.alloc(run - done) else {
                    return fail(store, "the disk is full");
                };
                let mut buf = Vec::with_capacity(n * BLOCK);
                for j in 0..n {
                    let (n_id, n_idx) = keys[k + done + j];
                    buf.extend_from_slice(&self.pending.get(n_id, n_idx).expect("pending")[..]);
                }
                if store.write_run(at, &buf).is_err() {
                    return fail(store, "a disk write failed");
                }
                if let Some(Content::Blocks { map, .. }) =
                    self.tree.node_mut(id).map(|n| &mut n.content)
                {
                    for j in 0..n {
                        let idx = (first_idx + (done + j) as u64) as usize;
                        if let Some(m) = map.get_mut(idx) {
                            *m = at + j as u64;
                        }
                    }
                }
                written += n;
                done += n;
            }
            k += run;
        }
        // 2. Metadata.
        let mut meta = Vec::new();
        meta.extend_from_slice(&META_VERSION.to_le_bytes());
        let mut live = Vec::new();
        self.tree.serialize(home, &mut meta, &mut live);
        let meta_blocks = meta.len().div_ceil(BLOCK);
        let mut padded = meta.clone();
        padded.resize(meta_blocks * BLOCK, 0);
        let mut runs: Vec<(u64, usize)> = Vec::new();
        let mut placed = 0;
        while placed < meta_blocks {
            let Some((at, n)) = store.alloc(meta_blocks - placed) else {
                return fail(store, "the disk is full");
            };
            if runs.len() == MAX_META_RUNS {
                return fail(store, "the metadata is too scattered");
            }
            if store
                .write_run(at, &padded[placed * BLOCK..(placed + n) * BLOCK])
                .is_err()
            {
                return fail(store, "a disk write failed");
            }
            runs.push((at, n));
            placed += n;
        }
        if crash_before_header {
            zoo_rt::log!(
                "BadgerHandler: test: stopping after the payload of generation {} (simulated power loss)",
                next
            );
            return Saved::Stopped;
        }
        // 3. The superblock: the commit point.
        let mut h = [0u8; BLOCK];
        h[..8].copy_from_slice(SB_MAGIC);
        h[8..16].copy_from_slice(&next.to_le_bytes());
        h[16..24].copy_from_slice(&store.blocks.to_le_bytes());
        h[24..32].copy_from_slice(&(meta.len() as u64).to_le_bytes());
        h[32..36].copy_from_slice(&crc32(&meta).to_le_bytes());
        h[36..40].copy_from_slice(&(runs.len() as u32).to_le_bytes());
        for (r, &(at, n)) in runs.iter().enumerate() {
            let o = 40 + 12 * r;
            h[o..o + 8].copy_from_slice(&at.to_le_bytes());
            h[o + 8..o + 12].copy_from_slice(&(n as u32).to_le_bytes());
        }
        let crc = crc32(&h[..BLOCK - 4]);
        h[BLOCK - 4..].copy_from_slice(&crc.to_le_bytes());
        if zoo_rt::block_write(store.disk, slot, &h).is_err() {
            return fail(store, "the superblock write failed");
        }
        // Saved: the new generation's blocks are live, the old current
        // becomes the protected previous one, and blocks only the
        // generation before that used are free again.
        for &(at, n) in &runs {
            live.extend(at..at + n as u64);
        }
        store.live_prev = core::mem::take(&mut store.live);
        store.live = live;
        store.rebuild_used();
        store.current = Some((slot, next));
        self.pending.clear();
        zoo_rt::log!(
            "BadgerHandler: saved generation {} to slot {} ({} data blocks written, {} bytes of metadata, {} blocks in use)",
            next,
            slot_name(slot),
            written,
            meta.len(),
            store.live.len()
        );
        Saved::Done
    }

    /// Replaces `/home/user` with the tree in `meta` (already checked).
    fn restore_home(&mut self, meta: &[u8]) -> bool {
        let Ok(user) = self.tree.resolve(0, "home/user") else {
            return false;
        };
        // Drop the seeded contents.
        let children = self
            .tree
            .node_mut(user)
            .map(|u| core::mem::take(&mut u.children))
            .unwrap_or_default();
        for c in children {
            self.tree.free_subtree(c);
        }
        let mut r = Reader { data: meta, at: 4 };
        // The `/home/user` record itself.
        let (Some(REC_DIR), Some(len)) = (r.u8(), r.u16()) else {
            return false;
        };
        if r.bytes(len as usize).is_none() {
            return false;
        }
        let Some(n) = r.u32() else { return false };
        // (directory, records left in it)
        let mut stack = vec![(user, n)];
        while let Some(&mut (dir, ref mut left)) = stack.last_mut() {
            if *left == 0 {
                stack.pop();
                continue;
            }
            *left -= 1;
            let (Some(kind), Some(len)) = (r.u8(), r.u16()) else {
                return false;
            };
            let Some(name) = r
                .bytes(len as usize)
                .and_then(|b| core::str::from_utf8(b).ok())
            else {
                return false;
            };
            let name = String::from(name);
            if kind == REC_LINK {
                let Some(target) = r
                    .u16()
                    .and_then(|n| r.bytes(n as usize))
                    .and_then(|b| core::str::from_utf8(b).ok())
                else {
                    return false;
                };
                let content = Content::Link(String::from(target));
                if self.tree.add(dir, &name, Kind::Link, content).is_err() {
                    return false;
                }
            } else if kind == REC_DIR {
                let Some(n) = r.u32() else { return false };
                match self.tree.add(dir, &name, Kind::Dir, Content::Static(&[])) {
                    Ok(id) => stack.push((id, n)),
                    Err(_) => return false,
                }
            } else {
                let (Some(size), Some(runs)) = (r.u64(), r.u32()) else {
                    return false;
                };
                let mut map = Vec::with_capacity(blocks_for(size));
                for _ in 0..runs {
                    let (Some(first), Some(count)) = (r.u64(), r.u32()) else {
                        return false;
                    };
                    for k in 0..u64::from(count) {
                        map.push(if first == 0 { 0 } else { first + k });
                    }
                }
                let content = Content::Blocks { size, map };
                if self.tree.add(dir, &name, Kind::File, content).is_err() {
                    return false;
                }
            }
        }
        true
    }
}

impl Tree {
    /// Appends the metadata record of `id` and everything below it; adds
    /// the data blocks its files use to `live`.
    fn serialize(&self, id: usize, out: &mut Vec<u8>, live: &mut Vec<u64>) {
        let Some(n) = self.node(id) else { return };
        let name = n.name.as_bytes();
        match n.kind {
            Kind::Link => {
                let target = match &n.content {
                    Content::Link(t) => t.as_bytes(),
                    _ => &[],
                };
                out.push(REC_LINK);
                out.extend_from_slice(&(name.len() as u16).to_le_bytes());
                out.extend_from_slice(name);
                out.extend_from_slice(&(target.len() as u16).to_le_bytes());
                out.extend_from_slice(target);
            }
            Kind::Dir => {
                out.push(REC_DIR);
                out.extend_from_slice(&(name.len() as u16).to_le_bytes());
                out.extend_from_slice(name);
                out.extend_from_slice(&(n.children.len() as u32).to_le_bytes());
                for &c in &n.children {
                    self.serialize(c, out, live);
                }
            }
            Kind::File => {
                out.push(REC_FILE);
                out.extend_from_slice(&(name.len() as u16).to_le_bytes());
                out.extend_from_slice(name);
                let (size, map): (u64, &[u64]) = match &n.content {
                    Content::Blocks { size, map } => (*size, map),
                    // Converted before a save; an empty file otherwise.
                    Content::Static(_) | Content::Link(_) => (0, &[]),
                };
                out.extend_from_slice(&size.to_le_bytes());
                // Runs of consecutive blocks (first 0: a run of zeros).
                let mut runs: Vec<(u64, u32)> = Vec::new();
                for &b in map {
                    match runs.last_mut() {
                        Some((first, count))
                            if (*first == 0 && b == 0)
                                || (*first != 0 && b == *first + u64::from(*count)) =>
                        {
                            *count += 1
                        }
                        _ => runs.push((b, 1)),
                    }
                    if b != 0 {
                        live.push(b);
                    }
                }
                out.extend_from_slice(&(runs.len() as u32).to_le_bytes());
                for (first, count) in runs {
                    out.extend_from_slice(&first.to_le_bytes());
                    out.extend_from_slice(&count.to_le_bytes());
                }
            }
        }
    }
}
