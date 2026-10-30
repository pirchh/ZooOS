//! The in-memory filesystem (docs/m5c1-design.md §1–§2): `/bin` holds the
//! initrd programs (read-only), `/home/user` is seeded from initrd entries
//! under `home/user/` and accepts new folders. Not persistent.
//!
//! Nodes are never reused: a removed node's slot stays empty, so a stale
//! directory handle can only ever see `NOTFOUND`.

use crate::initrd;
use alloc::string::String;
use alloc::vec::Vec;
use spin::Mutex;
use zoo_abi::Error;

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
    /// File contents (initrd-backed, read-only).
    data: &'static [u8],
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
        data: &'static [u8],
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
            data,
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
                n.data.len()
            } else {
                n.children.len()
            },
        ))
    }

    pub fn size(&self, id: usize) -> usize {
        self.node(id).map_or(0, |n| {
            if n.kind == Kind::File {
                n.data.len()
            } else {
                n.children.len()
            }
        })
    }

    pub fn data(&self, id: usize) -> Result<&'static [u8], Error> {
        let n = self.node(id).ok_or(Error::NotFound)?;
        if n.kind != Kind::File {
            return Err(Error::Inval);
        }
        Ok(n.data)
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
        self.add(parent, last, Kind::Dir, &[])
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
    let root = fs.add(ROOT, "", Kind::Dir, &[]).expect("empty tree");
    // `/bin` is read-only; `/home` and below are writable.
    let bin = fs.add(root, "bin", Kind::Dir, &[]).expect("bin");
    if let Some(Some(b)) = fs.nodes.get_mut(bin) {
        b.writable = false;
    }
    let home = fs.add(root, "home", Kind::Dir, &[]).expect("home");
    let user = fs.add(home, "user", Kind::Dir, &[]).expect("user");
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
                None => match fs.add(cur, d, Kind::Dir, &[]) {
                    Ok(c) => c,
                    Err(_) => break,
                },
            };
        }
        if fs.child(cur, last).is_none() && fs.add(cur, last, Kind::File, data).is_ok() {
            files += 1;
        }
    }
    let nodes = fs.nodes.len();
    *FS.lock() = Some(fs);
    (files, nodes)
}
