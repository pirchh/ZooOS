//! BadgerHandler: the filesystem service (docs/s4-design.md; badgers dig
//! burrows of tunnels and chambers, like folders). Holds the file tree in
//! its own memory: `/bin` (the initrd programs, read-only) and `/home/user`
//! (seeded from the initrd's `home/user/` files, writable). Not persistent.
//!
//! Every folder handle a program holds is a channel to BadgerHandler; this
//! side remembers which folder it names and with which rights. Requests and
//! replies use `zoo_abi::fsproto`. Started with a control channel: `root`
//! answers with a root folder channel (all rights) as a handle message
//! (tag `fsproto::TAG_ROOT`); `ping` answers `pong`.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use zoo_abi::{
    DirEntry, Error, KIND_DIR, KIND_FILE, MAX_DIR_NAME, WAIT_ANY_MAX, WAIT_FOREVER, dir_rights,
};
use zoo_rt::fsproto::{self, Op};
use zoo_rt::{Handle, channel_create, close, send, send_handle, try_recv, wait_any};

zoo_rt::entry2!(main);

const MAX_NAME: usize = 64;
const MAX_DEPTH: usize = 16;
const MAX_NODES: usize = 4096;
const MAX_FILE: usize = 16 << 20;
/// Folder channels served at once (one handle each in this process).
const MAX_SESSIONS: usize = WAIT_ANY_MAX - 2;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Dir,
    File,
}

enum Content {
    /// Seeded from the initrd (mapped read-only, never copied until written).
    Static(&'static [u8]),
    Owned(Vec<u8>),
}

impl Content {
    fn bytes(&self) -> &[u8] {
        match self {
            Content::Static(d) => d,
            Content::Owned(v) => v,
        }
    }
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

fn valid_component(c: &str) -> bool {
    !c.is_empty()
        && c.len() <= MAX_NAME
        && c != "."
        && c != ".."
        && c.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b' ' | b'.' | b'_' | b'-'))
}

/// Same rules as the kernel had: relative only; no empty, `.` or `..`.
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
        self.node(dir)?
            .children
            .iter()
            .copied()
            .find(|&c| self.node(c).is_some_and(|n| n.name == name))
    }

    fn add(
        &mut self,
        parent: usize,
        name: &str,
        kind: Kind,
        content: Content,
    ) -> Result<usize, Error> {
        if self.nodes.len() >= MAX_NODES {
            return Err(Error::NoMem);
        }
        let writable = self.node(parent).is_none_or(|p| p.writable);
        let id = self.nodes.len();
        self.nodes.push(Some(Node {
            name: String::from(name),
            parent,
            kind,
            children: Vec::new(),
            content,
            writable,
        }));
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
        self.node(id).map_or(0, |n| match n.kind {
            Kind::File => n.content.bytes().len() as u64,
            Kind::Dir => n.children.len() as u64,
        })
    }

    fn entry(&self, id: usize) -> DirEntry {
        let mut e = DirEntry::default();
        if let Some(n) = self.node(id) {
            e.kind = if n.kind == Kind::Dir {
                KIND_DIR
            } else {
                KIND_FILE
            };
            e.size = self.size(id);
            let k = n.name.len().min(MAX_DIR_NAME);
            e.name[..k].copy_from_slice(&n.name.as_bytes()[..k]);
            e.name_len = k as u64;
        }
        e
    }

    fn file_mut(&mut self, id: usize) -> Result<&mut Vec<u8>, Error> {
        let n = self.node_mut(id).ok_or(Error::NotFound)?;
        if n.kind != Kind::File {
            return Err(Error::Inval);
        }
        if !n.writable {
            return Err(Error::Rights);
        }
        if let Content::Static(d) = n.content {
            n.content = Content::Owned(d.to_vec());
        }
        match &mut n.content {
            Content::Owned(v) => Ok(v),
            Content::Static(_) => unreachable!("converted above"),
        }
    }

    fn remove(&mut self, base: usize, path: &str, want: Kind) -> Result<(), Error> {
        if components(path)?.is_empty() {
            return Err(Error::Inval);
        }
        let id = self.resolve(base, path)?;
        let n = self.node(id).ok_or(Error::NotFound)?;
        if n.kind != want {
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
        self.nodes[id] = None;
        Ok(())
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
    let mut t = Tree { nodes: Vec::new() };
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

struct Badger {
    tree: Tree,
    sessions: Vec<Session>,
    /// The data disk, when the supervisor gave us one.
    store: Option<Store>,
    /// Tick of the first unsaved change, if any.
    dirty: Option<u64>,
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
            Op::CreateDir | Op::CreateFile => dir_rights::CREATE,
            Op::RemoveDir | Op::RemoveFile => dir_rights::REMOVE,
            Op::Write | Op::Truncate | Op::WriteBulk => dir_rights::WRITE,
            Op::Rename => dir_rights::CREATE | dir_rights::REMOVE,
            Op::List | Op::Stat | Op::Read | Op::ReadBulk => dir_rights::READ,
        };
        if rights & need != need {
            return fsproto::reply(chan, Err(Error::Rights), &[]).is_ok();
        }
        let path = req.path;
        let t = &mut self.tree;
        let mut out: Vec<u8> = Vec::new();
        let result: Result<u64, Error> = match req.op {
            Op::Open => {
                let new_rights = req.arg;
                if new_rights & !dir_rights::ALL != 0 || new_rights & !rights != 0 {
                    Err(Error::Rights)
                } else {
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
            Op::List => t.resolve(base, path).and_then(|id| {
                let n = t.node(id).ok_or(Error::NotFound)?;
                if n.kind != Kind::Dir {
                    return Err(Error::NotDir);
                }
                let c = *n.children.get(req.arg as usize).ok_or(Error::NotFound)?;
                fsproto::encode_entry(&t.entry(c), &mut out);
                Ok(0)
            }),
            Op::Stat => t.resolve(base, path).map(|id| {
                let mut e = t.entry(id);
                e.name_len = 0;
                fsproto::encode_entry(&e, &mut out);
                0
            }),
            Op::Read => t.resolve(base, path).and_then(|id| {
                let n = t.node(id).ok_or(Error::NotFound)?;
                if n.kind != Kind::File {
                    return Err(Error::Inval);
                }
                let data = n.content.bytes();
                let start = (req.arg as usize).min(data.len());
                let want = (req.count as usize).min(fsproto::MAX_DATA);
                let end = (start + want).min(data.len());
                out.extend_from_slice(&data[start..end]);
                Ok((end - start) as u64)
            }),
            Op::CreateDir => t
                .new_entry(base, path)
                .and_then(|(p, name)| t.add(p, name, Kind::Dir, Content::Static(&[])))
                .map(|_| 0),
            Op::CreateFile => t
                .new_entry(base, path)
                .and_then(|(p, name)| t.add(p, name, Kind::File, Content::Owned(Vec::new())))
                .map(|_| 0),
            Op::RemoveDir => t.remove(base, path, Kind::Dir).map(|_| 0),
            Op::RemoveFile => t.remove(base, path, Kind::File).map(|_| 0),
            Op::Write => {
                let offset = req.arg as usize;
                match offset
                    .checked_add(req.data.len())
                    .filter(|&e| e <= MAX_FILE)
                {
                    None => Err(Error::Inval),
                    Some(end) => t.resolve(base, path).and_then(|id| {
                        let v = t.file_mut(id)?;
                        if v.len() < end {
                            v.resize(end, 0);
                        }
                        v[offset..end].copy_from_slice(req.data);
                        Ok(req.data.len() as u64)
                    }),
                }
            }
            Op::Truncate => {
                let len = req.arg as usize;
                if len > MAX_FILE {
                    Err(Error::Inval)
                } else {
                    t.resolve(base, path).and_then(|id| {
                        t.file_mut(id)?.resize(len, 0);
                        Ok(0)
                    })
                }
            }
            Op::Rename => match core::str::from_utf8(req.data) {
                Ok(to) => t.rename(base, path, to).map(|_| 0),
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
                        let offset = req.arg as usize;
                        if req.op == Op::ReadBulk {
                            t.resolve(base, path).and_then(|id| {
                                let n = t.node(id).ok_or(Error::NotFound)?;
                                if n.kind != Kind::File {
                                    return Err(Error::Inval);
                                }
                                let data = n.content.bytes();
                                let start = offset.min(data.len());
                                let end = (start + len).min(data.len());
                                // SAFETY: the client waits for this reply
                                // before touching the buffer again; `end -
                                // start <= len <= BULK_SIZE` fits in it.
                                unsafe {
                                    core::ptr::copy_nonoverlapping(
                                        data[start..end].as_ptr(),
                                        shared as *mut u8,
                                        end - start,
                                    )
                                };
                                Ok((end - start) as u64)
                            })
                        } else {
                            match offset.checked_add(len).filter(|&e| e <= MAX_FILE) {
                                None => Err(Error::Inval),
                                Some(end) => t.resolve(base, path).and_then(|id| {
                                    let v = t.file_mut(id)?;
                                    if v.len() < end {
                                        v.resize(end, 0);
                                    }
                                    // SAFETY: the client filled the buffer
                                    // and waits for this reply; `len` fits.
                                    let src = unsafe {
                                        core::slice::from_raw_parts(shared as *const u8, len)
                                    };
                                    v[offset..end].copy_from_slice(src);
                                    Ok(len as u64)
                                }),
                            }
                        }
                    }
                }
            }
        };
        let changes = matches!(
            req.op,
            Op::CreateDir
                | Op::CreateFile
                | Op::RemoveDir
                | Op::RemoveFile
                | Op::Write
                | Op::WriteBulk
                | Op::Truncate
                | Op::Rename
        );
        if changes && result.is_ok() && self.store.is_some() && self.dirty.is_none() {
            self.dirty = Some(zoo_rt::ticks());
        }
        fsproto::reply(chan, result, &out).is_ok()
    }

    /// Writes a snapshot once changes have been quiet for `SAVE_DELAY`.
    /// Returns false when the test hook stops the process mid-save.
    fn maybe_save(&mut self) -> bool {
        let Some(since) = self.dirty else { return true };
        if zoo_rt::ticks() < since + SAVE_DELAY {
            return true;
        }
        self.dirty = None;
        let payload = self.tree.serialize_home();
        let crash = core::mem::take(&mut self.crash_next_save);
        match self.store.as_mut() {
            Some(store) => store.save(&payload, crash) || !crash,
            None => true,
        }
    }
}

fn main(control: u64, disk: u64) -> u8 {
    let mut b = Badger {
        tree: seed(),
        sessions: Vec::new(),
        store: None,
        dirty: None,
        crash_next_save: false,
    };
    if disk != zoo_rt::NO_HANDLE {
        let size = zoo_rt::block_size(disk).unwrap_or(0);
        let slot_bytes = (size / 2) / BLOCK as u64 * BLOCK as u64;
        if slot_bytes >= 2 * BLOCK as u64 {
            let mut store = Store {
                disk,
                slot_bytes,
                current: None,
            };
            match store.load() {
                Some(payload) => {
                    if !b.tree.restore_home(&payload) {
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
            b.store = Some(store);
        }
    }
    let mut buf = vec![0u8; zoo_abi::MAX_MESSAGE];
    loop {
        // Control channel first, then every folder channel.
        let mut watch = Vec::with_capacity(1 + b.sessions.len());
        watch.push(control);
        watch.extend(b.sessions.iter().map(|s| s.chan));
        let timeout = match b.dirty {
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

// ---- Persistence (docs/s5-design.md) -------------------------------------

const MAGIC: &[u8; 8] = b"ZOOFS001";
const BLOCK: usize = zoo_abi::BLOCK_BYTES;
/// Ticks after the last change before a snapshot is written.
const SAVE_DELAY: u64 = 50;

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

/// Two snapshot slots on the data disk, each half of it: a header block
/// (magic, generation, payload length, CRC-32) then the payload blocks.
struct Store {
    disk: Handle,
    slot_bytes: u64,
    /// Slot holding the newest valid snapshot and its generation.
    current: Option<(u64, u64)>,
}

impl Store {
    fn read(&self, block: u64, buf: &mut [u8; BLOCK]) -> bool {
        zoo_rt::block_read(self.disk, block, buf).is_ok()
    }

    fn write(&self, block: u64, buf: &[u8; BLOCK]) -> bool {
        zoo_rt::block_write(self.disk, block, buf).is_ok()
    }

    /// The newest snapshot whose header and CRC are valid.
    fn load(&mut self) -> Option<Vec<u8>> {
        let mut best: Option<(u64, u64, Vec<u8>)> = None;
        for (name, slot) in [("A", 0u64), ("B", self.slot_bytes)] {
            let mut h = [0u8; BLOCK];
            if !self.read(slot / BLOCK as u64, &mut h) {
                zoo_rt::log!("BadgerHandler: slot {}: read failed", name);
                continue;
            }
            if &h[..8] != MAGIC {
                zoo_rt::log!("BadgerHandler: slot {}: empty", name);
                continue;
            }
            let generation = u64::from_le_bytes(h[8..16].try_into().unwrap_or_default());
            let len = u64::from_le_bytes(h[16..24].try_into().unwrap_or_default());
            let crc = u32::from_le_bytes(h[24..28].try_into().unwrap_or_default());
            if len > self.slot_bytes - BLOCK as u64 {
                zoo_rt::log!("BadgerHandler: slot {}: bad length, ignored", name);
                continue;
            }
            let mut payload = vec![0u8; len as usize];
            let first = slot / BLOCK as u64 + 1;
            let mut ok = true;
            for (k, chunk) in payload.chunks_mut(BLOCK).enumerate() {
                let mut b = [0u8; BLOCK];
                if !self.read(first + k as u64, &mut b) {
                    ok = false;
                    break;
                }
                let n = chunk.len();
                chunk.copy_from_slice(&b[..n]);
            }
            if !ok || crc32(&payload) != crc {
                zoo_rt::log!(
                    "BadgerHandler: slot {}: generation {} failed its check, ignored",
                    name,
                    generation
                );
                continue;
            }
            if best.as_ref().is_none_or(|(g, _, _)| generation > *g) {
                best = Some((generation, slot, payload));
            }
        }
        let (generation, slot, payload) = best?;
        self.current = Some((slot, generation));
        zoo_rt::log!(
            "BadgerHandler: loaded generation {} from slot {} ({} bytes)",
            generation,
            if slot == 0 { "A" } else { "B" },
            payload.len()
        );
        Some(payload)
    }

    /// Writes `payload` as the next generation into the other slot: payload
    /// first, header last. With `crash_before_header` it stops after the
    /// payload (a test of power loss mid-save) and returns false.
    fn save(&mut self, payload: &[u8], crash_before_header: bool) -> bool {
        if payload.len() as u64 > self.slot_bytes - BLOCK as u64 {
            let _ = zoo_rt::log("BadgerHandler: files too large for the disk slot; not saved");
            return false;
        }
        let (slot, generation) = match self.current {
            Some((s, g)) => (if s == 0 { self.slot_bytes } else { 0 }, g + 1),
            None => (0, 1),
        };
        let first = slot / BLOCK as u64 + 1;
        for (k, chunk) in payload.chunks(BLOCK).enumerate() {
            let mut b = [0u8; BLOCK];
            b[..chunk.len()].copy_from_slice(chunk);
            if !self.write(first + k as u64, &b) {
                let _ = zoo_rt::log("BadgerHandler: disk write failed; not saved");
                return false;
            }
        }
        if crash_before_header {
            zoo_rt::log!(
                "BadgerHandler: test: stopping after the payload of generation {} (simulated power loss)",
                generation
            );
            return false;
        }
        let mut h = [0u8; BLOCK];
        h[..8].copy_from_slice(MAGIC);
        h[8..16].copy_from_slice(&generation.to_le_bytes());
        h[16..24].copy_from_slice(&(payload.len() as u64).to_le_bytes());
        h[24..28].copy_from_slice(&crc32(payload).to_le_bytes());
        if !self.write(slot / BLOCK as u64, &h) {
            let _ = zoo_rt::log("BadgerHandler: header write failed; not saved");
            return false;
        }
        self.current = Some((slot, generation));
        zoo_rt::log!(
            "BadgerHandler: saved generation {} to slot {} ({} bytes)",
            generation,
            if slot == 0 { "A" } else { "B" },
            payload.len()
        );
        true
    }
}

const REC_DIR: u8 = 1;
const REC_FILE: u8 = 2;

impl Tree {
    /// `/home/user` as records: kind, path (u16 length), size (u32), data.
    fn serialize_home(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if let Ok(user) = self.resolve(0, "home/user") {
            self.serialize_dir(user, "", &mut out);
        }
        out
    }

    fn serialize_dir(&self, dir: usize, prefix: &str, out: &mut Vec<u8>) {
        let Some(d) = self.node(dir) else { return };
        for &c in &d.children {
            let Some(n) = self.node(c) else { continue };
            let mut path = String::from(prefix);
            if !path.is_empty() {
                path.push('/');
            }
            path.push_str(&n.name);
            let data: &[u8] = if n.kind == Kind::File {
                n.content.bytes()
            } else {
                &[]
            };
            out.push(if n.kind == Kind::Dir {
                REC_DIR
            } else {
                REC_FILE
            });
            out.extend_from_slice(&(path.len() as u16).to_le_bytes());
            out.extend_from_slice(path.as_bytes());
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(data);
            if n.kind == Kind::Dir {
                self.serialize_dir(c, &path, out);
            }
        }
    }

    /// Replaces `/home/user` with the records in `payload`.
    fn restore_home(&mut self, payload: &[u8]) -> bool {
        let Ok(user) = self.resolve(0, "home/user") else {
            return false;
        };
        // Detach the seeded contents (their slots stay unused).
        if let Some(u) = self.node_mut(user) {
            u.children.clear();
        }
        let mut at = 0;
        while at < payload.len() {
            let Some(&kind) = payload.get(at) else {
                return false;
            };
            let Some(plen) = payload.get(at + 1..at + 3) else {
                return false;
            };
            let plen = u16::from_le_bytes([plen[0], plen[1]]) as usize;
            let Some(path) = payload.get(at + 3..at + 3 + plen) else {
                return false;
            };
            let Ok(path) = core::str::from_utf8(path) else {
                return false;
            };
            let p = at + 3 + plen;
            let Some(dlen) = payload.get(p..p + 4) else {
                return false;
            };
            let dlen = u32::from_le_bytes([dlen[0], dlen[1], dlen[2], dlen[3]]) as usize;
            let Some(data) = payload.get(p + 4..p + 4 + dlen) else {
                return false;
            };
            at = p + 4 + dlen;
            let Ok((parent, name)) = self.new_entry(user, path) else {
                return false;
            };
            let content = Content::Owned(data.to_vec());
            let k = if kind == REC_DIR {
                Kind::Dir
            } else {
                Kind::File
            };
            if self.add(parent, name, k, content).is_err() {
                return false;
            }
        }
        true
    }
}
