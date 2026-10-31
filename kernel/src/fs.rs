//! The in-memory filesystem (docs/m5c1-design.md §1–§2): `/bin` holds the
//! initrd programs (read-only), `/home/user` is seeded from initrd entries
//! under `home/user/` and accepts new folders. Not persistent.
//!
//! Nodes are never reused: a removed node's slot stays empty, so a stale
//! directory handle can only ever see `NOTFOUND`.
//!
//! File contents (docs/posix4-design.md): seeded files point at initrd
//! bytes until first written; written files keep their bytes in whole
//! physical pages from the frame allocator (the kernel heap only holds the
//! page list).

use crate::initrd;
use crate::memory::Memory;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicUsize, Ordering};
use spin::Mutex;
use x86_64::structures::paging::PhysFrame;
use zoo_abi::Error;

/// Largest file, and most pages all written files may hold together.
pub const MAX_FILE: usize = 16 << 20;
pub const MAX_FILE_PAGES: usize = 16384;
const PAGE: usize = 4096;

/// Physical pages currently holding file contents (for the boot leak check).
static FILE_PAGES: AtomicUsize = AtomicUsize::new(0);

pub fn file_pages() -> usize {
    FILE_PAGES.load(Ordering::Relaxed)
}

enum Content {
    /// Seeded from the initrd; read-only bytes.
    Static(&'static [u8]),
    /// Written: `len` bytes in `pages` (missing trailing pages are zero).
    Pages { len: usize, pages: Vec<PhysFrame> },
}

impl Content {
    fn len(&self) -> usize {
        match self {
            Content::Static(d) => d.len(),
            Content::Pages { len, .. } => *len,
        }
    }
}

pub const MAX_NAME: usize = 64;
pub const MAX_PATH_LEN: usize = 256;
pub const MAX_DEPTH: usize = 16;
pub const MAX_NODES: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Dir,
    File,
}

struct Node {
    name: String,
    parent: usize,
    kind: Kind,
    /// Child node ids (directories only), kept sorted by name.
    children: Vec<usize>,
    /// File contents.
    content: Content,
    /// Whether new entries may be created here (false under `/bin`).
    writable: bool,
}

pub struct Fs {
    nodes: Vec<Option<Node>>,
}

pub const ROOT: usize = 0;

static FS: Mutex<Option<Fs>> = Mutex::new(None);

/// Runs `f` on the filesystem. Only called with interrupts disabled (system
/// calls, or kernel code inside `without_interrupts`), so the lock is never
/// contended; contention means a bug.
pub fn with<R>(f: impl FnOnce(&mut Fs) -> R) -> R {
    let mut g = FS.try_lock().expect("filesystem lock already held");
    f(g.as_mut().expect("filesystem not initialised"))
}

/// One path component: 1–64 bytes of `[A-Za-z0-9 ._-]`, not `.` or `..`.
fn valid_component(c: &str) -> bool {
    !c.is_empty()
        && c.len() <= MAX_NAME
        && c != "."
        && c != ".."
        && c.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b' ' | b'.' | b'_' | b'-'))
}

/// Splits a relative path into validated components. The empty path means
/// the directory itself. Absolute paths, `.`, `..`, empty components (`a//b`)
/// and bad characters are all `INVAL`: a path can never name anything outside
/// the directory it is resolved against.
pub fn components(path: &str) -> Result<Vec<&str>, Error> {
    if path.is_empty() {
        return Ok(Vec::new());
    }
    if path.len() > MAX_PATH_LEN || path.starts_with('/') {
        return Err(Error::Inval);
    }
    let parts: Vec<&str> = path.split('/').collect();
    if parts.len() > MAX_DEPTH || !parts.iter().all(|c| valid_component(c)) {
        return Err(Error::Inval);
    }
    Ok(parts)
}

impl Fs {
    fn node(&self, id: usize) -> Option<&Node> {
        self.nodes.get(id).and_then(Option::as_ref)
    }

    fn child(&self, dir: usize, name: &str) -> Option<usize> {
        let d = self.node(dir)?;
        d.children
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
            let at = self.node(parent).map_or(0, |p| {
                p.children
                    .partition_point(|&c| self.node(c).is_some_and(|n| n.name.as_str() < name))
            });
            if let Some(Some(p)) = self.nodes.get_mut(parent) {
                p.children.insert(at, id);
            }
        }
        Ok(id)
    }

    /// Resolves `path` relative to directory `base`.
    pub fn resolve(&self, base: usize, path: &str) -> Result<usize, Error> {
        let mut cur = base;
        if self.node(cur).is_none() {
            return Err(Error::NotFound);
        }
        for c in components(path)? {
            let n = self.node(cur).ok_or(Error::NotFound)?;
            if n.kind != Kind::Dir {
                return Err(Error::NotDir);
            }
            cur = self.child(cur, c).ok_or(Error::NotFound)?;
        }
        Ok(cur)
    }

    pub fn kind(&self, id: usize) -> Option<Kind> {
        self.node(id).map(|n| n.kind)
    }

    /// (name, kind, size) of the `index`-th entry of directory `dir`.
    pub fn entry(&self, dir: usize, index: usize) -> Result<(&str, Kind, usize), Error> {
        let d = self.node(dir).ok_or(Error::NotFound)?;
        if d.kind != Kind::Dir {
            return Err(Error::NotDir);
        }
        let c = *d.children.get(index).ok_or(Error::NotFound)?;
        let n = self.node(c).ok_or(Error::NotFound)?;
        Ok((
            &n.name,
            n.kind,
            if n.kind == Kind::File {
                n.content.len()
            } else {
                n.children.len()
            },
        ))
    }

    pub fn size(&self, id: usize) -> usize {
        self.node(id).map_or(0, |n| {
            if n.kind == Kind::File {
                n.content.len()
            } else {
                n.children.len()
            }
        })
    }

    fn file_mut(&mut self, id: usize) -> Result<&mut Node, Error> {
        let n = self
            .nodes
            .get_mut(id)
            .and_then(Option::as_mut)
            .ok_or(Error::NotFound)?;
        if n.kind != Kind::File {
            return Err(Error::Inval);
        }
        Ok(n)
    }

    /// Copies up to `len` bytes from `offset` of file `id` to `out`;
    /// returns the count.
    ///
    /// # Safety
    /// `out` must be valid for `len` bytes of writes (a checked user range
    /// in the active address space).
    pub unsafe fn read(
        &self,
        mem: &Memory,
        id: usize,
        offset: usize,
        out: *mut u8,
        len: usize,
    ) -> Result<usize, Error> {
        let n = self.node(id).ok_or(Error::NotFound)?;
        if n.kind != Kind::File {
            return Err(Error::Inval);
        }
        let size = n.content.len();
        let start = offset.min(size);
        let count = (size - start).min(len);
        match &n.content {
            // SAFETY: `count` bytes fit `out` (caller contract) and the
            // initrd source; they do not overlap.
            Content::Static(d) => unsafe {
                core::ptr::copy_nonoverlapping(d[start..].as_ptr(), out, count)
            },
            Content::Pages { pages, .. } => {
                let mut done = 0;
                while done < count {
                    let pos = start + done;
                    let (pi, po) = (pos / PAGE, pos % PAGE);
                    let chunk = (PAGE - po).min(count - done);
                    match pages.get(pi) {
                        // SAFETY: the frame belongs to this file and is
                        // mapped in the physical window; `out` has room.
                        Some(f) => unsafe {
                            let src = mem.phys_to_virt(f.start_address()).add(po);
                            core::ptr::copy_nonoverlapping(src, out.add(done), chunk)
                        },
                        // SAFETY: as above; missing pages read as zero.
                        None => unsafe { core::ptr::write_bytes(out.add(done), 0, chunk) },
                    }
                    done += chunk;
                }
            }
        }
        Ok(count)
    }

    /// Turns a seeded file into a page-backed one (first write).
    fn make_paged(&mut self, mem: &mut Memory, id: usize) -> Result<(), Error> {
        let data = match &self.file_mut(id)?.content {
            Content::Static(d) => *d,
            Content::Pages { .. } => return Ok(()),
        };
        let mut pages = Vec::new();
        let r = grow(mem, &mut pages, data.len());
        if let Err(e) = r {
            free_pages(mem, &mut pages);
            return Err(e);
        }
        for (i, f) in pages.iter().enumerate() {
            let chunk = &data[i * PAGE..((i + 1) * PAGE).min(data.len())];
            // SAFETY: a fresh frame owned by this file, mapped in the
            // physical window; `chunk` is at most a page.
            unsafe {
                core::ptr::copy_nonoverlapping(
                    chunk.as_ptr(),
                    mem.phys_to_virt(f.start_address()),
                    chunk.len(),
                )
            };
        }
        self.file_mut(id)?.content = Content::Pages {
            len: data.len(),
            pages,
        };
        Ok(())
    }

    /// Writes `len` bytes from `src` at `offset` of file `id`, growing it
    /// (any gap reads as zero). Returns `len`.
    ///
    /// # Safety
    /// `src` must be valid for `len` bytes of reads (a checked user range in
    /// the active address space).
    pub unsafe fn write(
        &mut self,
        mem: &mut Memory,
        id: usize,
        offset: usize,
        src: *const u8,
        len: usize,
    ) -> Result<usize, Error> {
        let end = offset
            .checked_add(len)
            .filter(|&e| e <= MAX_FILE)
            .ok_or(Error::Inval)?;
        if !self.file_mut(id)?.writable {
            return Err(Error::Rights);
        }
        self.make_paged(mem, id)?;
        let n = self.file_mut(id)?;
        let Content::Pages { len: size, pages } = &mut n.content else {
            unreachable!("converted above")
        };
        grow(mem, pages, end)?;
        let mut done = 0;
        while done < len {
            let pos = offset + done;
            let (pi, po) = (pos / PAGE, pos % PAGE);
            let chunk = (PAGE - po).min(len - done);
            // SAFETY: `grow` made page `pi` exist; caller guarantees `src`.
            unsafe {
                let dst = mem.phys_to_virt(pages[pi].start_address()).add(po);
                core::ptr::copy_nonoverlapping(src.add(done), dst, chunk)
            };
            done += chunk;
        }
        *size = (*size).max(end);
        Ok(len)
    }

    /// Sets file `id`'s length; shrinking frees pages and zeroes the rest
    /// of the last page, growing reads as zero.
    pub fn truncate(&mut self, mem: &mut Memory, id: usize, new_len: usize) -> Result<(), Error> {
        if new_len > MAX_FILE {
            return Err(Error::Inval);
        }
        if !self.file_mut(id)?.writable {
            return Err(Error::Rights);
        }
        self.make_paged(mem, id)?;
        let n = self.file_mut(id)?;
        let Content::Pages { len: size, pages } = &mut n.content else {
            unreachable!("converted above")
        };
        if new_len < *size {
            let keep = new_len.div_ceil(PAGE);
            while pages.len() > keep {
                let f = pages.pop().expect("more pages than kept");
                let _ = mem.frames.deallocate(f);
                FILE_PAGES.fetch_sub(1, Ordering::Relaxed);
            }
            if let (Some(f), r) = (pages.last(), new_len % PAGE)
                && r != 0
            {
                // SAFETY: the file's own frame; zero its tail so a later
                // grow reads zeros.
                unsafe {
                    core::ptr::write_bytes(mem.phys_to_virt(f.start_address()).add(r), 0, PAGE - r)
                };
            }
        } else {
            grow(mem, pages, new_len)?;
        }
        *size = new_len;
        Ok(())
    }

    /// Pages actually held by files, to audit the `FILE_PAGES` counter.
    pub fn held_pages(&self) -> usize {
        self.nodes
            .iter()
            .flatten()
            .map(|n| match &n.content {
                Content::Pages { pages, .. } => pages.len(),
                Content::Static(_) => 0,
            })
            .sum()
    }

    /// Parent directory and final name of a new entry at `path`.
    fn new_entry<'p>(&self, base: usize, path: &'p str) -> Result<(usize, &'p str), Error> {
        let parts = components(path)?;
        let (last, parent_parts) = parts.split_last().ok_or(Error::Inval)?;
        let mut parent = base;
        for c in parent_parts {
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

    /// Creates an empty file at `path` under `base`.
    pub fn create_file(&mut self, base: usize, path: &str) -> Result<usize, Error> {
        let (parent, name) = self.new_entry(base, path)?;
        self.add(
            parent,
            name,
            Kind::File,
            Content::Pages {
                len: 0,
                pages: Vec::new(),
            },
        )
    }

    /// Removes the file at `path` under `base`, freeing its pages.
    pub fn remove_file(&mut self, mem: &mut Memory, base: usize, path: &str) -> Result<(), Error> {
        let id = self.resolve(base, path)?;
        let n = self.file_mut(id)?;
        if !n.writable {
            return Err(Error::Rights);
        }
        if let Content::Pages { pages, .. } = &mut n.content {
            free_pages(mem, pages);
        }
        let parent = n.parent;
        if let Some(Some(p)) = self.nodes.get_mut(parent) {
            p.children.retain(|&c| c != id);
        }
        self.nodes[id] = None;
        Ok(())
    }

    /// Moves the entry at `from` to `to` (both under `base`). `to` must not
    /// exist, and a directory cannot move into its own subtree.
    pub fn rename(&mut self, base: usize, from: &str, to: &str) -> Result<(), Error> {
        if components(from)?.is_empty() {
            return Err(Error::Inval);
        }
        let id = self.resolve(base, from)?;
        if !self.node(id).ok_or(Error::NotFound)?.writable {
            return Err(Error::Rights);
        }
        let (parent, name) = self.new_entry(base, to)?;
        // The new parent must not be the moved directory or inside it.
        let mut a = parent;
        loop {
            if a == id {
                return Err(Error::Inval);
            }
            let up = self.node(a).ok_or(Error::NotFound)?.parent;
            if up == a {
                break;
            }
            a = up;
        }
        let name = String::from(name);
        let old_parent = self.node(id).ok_or(Error::NotFound)?.parent;
        if let Some(Some(p)) = self.nodes.get_mut(old_parent) {
            p.children.retain(|&c| c != id);
        }
        let at = self.node(parent).map_or(0, |p| {
            p.children.partition_point(|&c| {
                self.node(c)
                    .is_some_and(|n| n.name.as_str() < name.as_str())
            })
        });
        if let Some(Some(p)) = self.nodes.get_mut(parent) {
            p.children.insert(at, id);
        }
        if let Some(Some(n)) = self.nodes.get_mut(id) {
            n.name = name;
            n.parent = parent;
        }
        Ok(())
    }

    /// Creates an empty directory at `path` under `base`; its parent must
    /// exist and be writable.
    pub fn create_dir(&mut self, base: usize, path: &str) -> Result<usize, Error> {
        let parts = components(path)?;
        let (last, parent_parts) = parts.split_last().ok_or(Error::Inval)?;
        let mut parent = base;
        for c in parent_parts {
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
        self.add(parent, last, Kind::Dir, Content::Static(&[]))
    }

    /// Removes the empty directory at `path` under `base`.
    pub fn remove_dir(&mut self, base: usize, path: &str) -> Result<(), Error> {
        if components(path)?.is_empty() {
            return Err(Error::Inval); // never the handle's own directory
        }
        let id = self.resolve(base, path)?;
        let n = self.node(id).ok_or(Error::NotFound)?;
        if n.kind != Kind::Dir {
            return Err(Error::NotDir);
        }
        if !n.children.is_empty() {
            return Err(Error::NotEmpty);
        }
        if !n.writable {
            return Err(Error::Rights);
        }
        let parent = n.parent;
        if let Some(Some(p)) = self.nodes.get_mut(parent) {
            p.children.retain(|&c| c != id);
        }
        self.nodes[id] = None;
        Ok(())
    }
}

/// Builds `/bin` from the initrd's `bin/` files and `/home/user` from its
/// `home/user/` files. Other initrd entries (models, test data) stay
/// reachable through `fs_map` only.
pub fn init() -> (usize, usize) {
    let mut fs = Fs { nodes: Vec::new() };
    let root = fs
        .add(ROOT, "", Kind::Dir, Content::Static(&[]))
        .expect("empty tree");
    // `/bin` is read-only; `/home` and below are writable.
    let bin = fs
        .add(root, "bin", Kind::Dir, Content::Static(&[]))
        .expect("bin");
    if let Some(Some(b)) = fs.nodes.get_mut(bin) {
        b.writable = false;
    }
    let home = fs
        .add(root, "home", Kind::Dir, Content::Static(&[]))
        .expect("home");
    let user = fs
        .add(home, "user", Kind::Dir, Content::Static(&[]))
        .expect("user");
    let mut files = 0;
    for (name, data) in initrd::files() {
        let (dir, rest) = if let Some(r) = name.strip_prefix("bin/") {
            (bin, r)
        } else if let Some(r) = name.strip_prefix("home/user/") {
            (user, r)
        } else {
            continue;
        };
        // Archive names that are not valid paths are skipped, not trusted.
        let Ok(parts) = components(rest) else {
            continue;
        };
        let Some((last, dirs)) = parts.split_last() else {
            continue;
        };
        let mut cur = dir;
        for d in dirs {
            cur = match fs.child(cur, d) {
                Some(c) => c,
                None => match fs.add(cur, d, Kind::Dir, Content::Static(&[])) {
                    Ok(c) => c,
                    Err(_) => break,
                },
            };
        }
        if fs.child(cur, last).is_none()
            && fs.add(cur, last, Kind::File, Content::Static(data)).is_ok()
        {
            files += 1;
        }
    }
    let nodes = fs.nodes.len();
    *FS.lock() = Some(fs);
    (files, nodes)
}

/// Adds zeroed pages to `pages` until they cover `len` bytes, within the
/// global limit.
fn grow(mem: &mut Memory, pages: &mut Vec<PhysFrame>, len: usize) -> Result<(), Error> {
    let want = len.div_ceil(PAGE);
    if want <= pages.len() {
        return Ok(());
    }
    let new = want - pages.len();
    if file_pages() + new > MAX_FILE_PAGES || mem.frames.available() < new + 64 {
        return Err(Error::NoMem);
    }
    while pages.len() < want {
        let f = mem.frames.allocate().ok_or(Error::NoMem)?;
        // SAFETY: a fresh frame, mapped in the physical window.
        unsafe { core::ptr::write_bytes(mem.phys_to_virt(f.start_address()), 0, PAGE) };
        pages.push(f);
        FILE_PAGES.fetch_add(1, Ordering::Relaxed);
    }
    Ok(())
}

fn free_pages(mem: &mut Memory, pages: &mut Vec<PhysFrame>) {
    for f in pages.drain(..) {
        let _ = mem.frames.deallocate(f);
        FILE_PAGES.fetch_sub(1, Ordering::Relaxed);
    }
}
