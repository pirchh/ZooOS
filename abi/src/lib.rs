//! ZooOS system call ABI, executable format checks and initial-filesystem
//! format (docs/m3-design.md). `no_std`, no dependencies, no unsafe code.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

pub mod elf;
pub mod tar;

/// Returned by syscall 0. Bumped when calls are added or change meaning.
/// Version 2 (M4) added calls 11-16 and errors 13-14; version 3 (M5a)
/// added calls 17-18; version 4 (M5c-1) added calls 19-26 and errors 15-17.
pub const ABI_VERSION: u64 = 4;

/// System call numbers (in `rax`). Never reused.
pub mod sys {
    pub const ABI_VERSION: u64 = 0;
    pub const EXIT: u64 = 1;
    pub const YIELD: u64 = 2;
    pub const LOG: u64 = 3;
    pub const CHANNEL_CREATE: u64 = 4;
    pub const SPAWN: u64 = 5;
    pub const SEND: u64 = 6;
    pub const RECV: u64 = 7;
    pub const WAIT: u64 = 8;
    pub const CLOSE: u64 = 9;
    pub const KILL: u64 = 10;
    pub const TICKS: u64 = 11;
    pub const SLEEP: u64 = 12;
    pub const TRY_RECV: u64 = 13;
    pub const DISPLAY_ACQUIRE: u64 = 14;
    pub const INPUT_READ: u64 = 15;
    pub const FS_STAT: u64 = 16;
    pub const MEM_ALLOC: u64 = 17;
    pub const FS_MAP: u64 = 18;
    pub const FS_ROOT: u64 = 19;
    pub const DIR_OPEN: u64 = 20;
    pub const DIR_LIST: u64 = 21;
    pub const DIR_STAT: u64 = 22;
    pub const FILE_READ: u64 = 23;
    pub const DIR_CREATE: u64 = 24;
    pub const DIR_REMOVE: u64 = 25;
    pub const SPAWN_WITH: u64 = 26;
}

/// Error codes; system calls return `-(code as i64)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u64)]
pub enum Error {
    NoSys = 1,
    BadHandle = 2,
    Fault = 3,
    Inval = 4,
    Full = 5,
    PeerClosed = 6,
    NotFound = 7,
    NoMem = 8,
    Crashed = 9,
    Rights = 10,
    TooSmall = 11,
    Killed = 12,
    Busy = 13,
    Again = 14,
    Exists = 15,
    NotEmpty = 16,
    NotDir = 17,
}

impl Error {
    pub const fn as_return(self) -> i64 {
        -(self as u64 as i64)
    }

    /// Decodes a negative system call result.
    pub fn from_return(ret: i64) -> Option<Error> {
        Some(match ret {
            -1 => Error::NoSys,
            -2 => Error::BadHandle,
            -3 => Error::Fault,
            -4 => Error::Inval,
            -5 => Error::Full,
            -6 => Error::PeerClosed,
            -7 => Error::NotFound,
            -8 => Error::NoMem,
            -9 => Error::Crashed,
            -10 => Error::Rights,
            -11 => Error::TooSmall,
            -12 => Error::Killed,
            -13 => Error::Busy,
            -14 => Error::Again,
            -15 => Error::Exists,
            -16 => Error::NotEmpty,
            -17 => Error::NotDir,
            _ => return None,
        })
    }

    pub const fn name(self) -> &'static str {
        match self {
            Error::NoSys => "NOSYS",
            Error::BadHandle => "BADHANDLE",
            Error::Fault => "FAULT",
            Error::Inval => "INVAL",
            Error::Full => "FULL",
            Error::PeerClosed => "PEERCLOSED",
            Error::NotFound => "NOTFOUND",
            Error::NoMem => "NOMEM",
            Error::Crashed => "CRASHED",
            Error::Rights => "RIGHTS",
            Error::TooSmall => "TOOSMALL",
            Error::Killed => "KILLED",
            Error::Busy => "BUSY",
            Error::Again => "AGAIN",
            Error::Exists => "EXISTS",
            Error::NotEmpty => "NOTEMPTY",
            Error::NotDir => "NOTDIR",
        }
    }
}

/// Largest `log` and channel message, in bytes.
pub const MAX_MESSAGE: usize = 256;
/// Messages queued per channel direction.
pub const CHANNEL_DEPTH: usize = 8;
/// Handle slots per process.
pub const MAX_HANDLES: usize = 16;
/// Largest program-name length accepted by `spawn`.
pub const MAX_PATH: usize = 64;
/// "No handle" for `spawn`'s third argument.
pub const NO_HANDLE: u64 = u64::MAX;

/// Where `display_acquire` maps the framebuffer in the caller.
pub const DISPLAY_BASE: u64 = 0x0000_4000_0000_0000;

/// Pixel layouts reported in `DisplayInfo::format`.
pub const FORMAT_RGB: u64 = 0;
pub const FORMAT_BGR: u64 = 1;

/// Filled in by `display_acquire` (docs/m4-design.md §2).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DisplayInfo {
    pub base: u64,
    pub width: u64,
    pub height: u64,
    /// Pixels per row in memory (may exceed `width`).
    pub stride: u64,
    pub bytes_per_pixel: u64,
    pub format: u64,
}

/// Where `mem_alloc` places a process's heap, and its per-process limit.
pub const HEAP_BASE: u64 = 0x0000_1000_0000_0000;
pub const HEAP_LIMIT: u64 = 1 << 30;
/// Where `fs_map` places read-only file mappings.
pub const MAP_BASE: u64 = 0x0000_2000_0000_0000;

/// Filled in by `fs_map`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MapInfo {
    pub addr: u64,
    pub len: u64,
}

/// Filled in by `fs_stat`.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FsEntry {
    pub size: u64,
    pub name_len: u64,
    pub name: [u8; MAX_FS_NAME],
}

pub const MAX_FS_NAME: usize = 96;

impl Default for FsEntry {
    fn default() -> Self {
        FsEntry {
            size: 0,
            name_len: 0,
            name: [0; MAX_FS_NAME],
        }
    }
}

impl FsEntry {
    pub fn name(&self) -> &str {
        let n = (self.name_len as usize).min(MAX_FS_NAME);
        core::str::from_utf8(&self.name[..n]).unwrap_or("?")
    }
}

/// Rights of a directory handle (docs/m5c1-design.md §2).
pub mod dir_rights {
    /// List, stat and read files below the directory.
    pub const READ: u64 = 1;
    /// Create directories below it.
    pub const CREATE: u64 = 2;
    /// Remove empty directories below it.
    pub const REMOVE: u64 = 4;
    pub const ALL: u64 = READ | CREATE | REMOVE;
}

/// Longest path accepted by the directory calls, and longest name.
pub const MAX_FS_PATH: usize = 256;
pub const MAX_DIR_NAME: usize = 64;

/// Second argument of calls 20-25 (the first is the directory handle).
/// `path` is relative to the handle's directory. `arg` is the rights for
/// `dir_open`, the entry index for `dir_list` and the byte offset for
/// `file_read`. `buf` receives a `DirEntry` (`dir_list`, `dir_stat`) or
/// file bytes (`file_read`).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FsArgs {
    pub path: u64,
    pub path_len: u64,
    pub arg: u64,
    pub buf: u64,
    pub buf_len: u64,
}

pub const KIND_DIR: u64 = 1;
pub const KIND_FILE: u64 = 2;

/// Filled in by `dir_list` and `dir_stat`. `size` is bytes for a file and
/// the number of entries for a directory.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirEntry {
    pub kind: u64,
    pub size: u64,
    pub name_len: u64,
    pub name: [u8; MAX_DIR_NAME],
}

impl Default for DirEntry {
    fn default() -> Self {
        DirEntry {
            kind: 0,
            size: 0,
            name_len: 0,
            name: [0; MAX_DIR_NAME],
        }
    }
}

impl DirEntry {
    pub fn name(&self) -> &str {
        let n = (self.name_len as usize).min(MAX_DIR_NAME);
        core::str::from_utf8(&self.name[..n]).unwrap_or("?")
    }

    pub fn is_dir(&self) -> bool {
        self.kind == KIND_DIR
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_round_trip() {
        for e in [
            Error::NoSys,
            Error::BadHandle,
            Error::Fault,
            Error::Inval,
            Error::Full,
            Error::PeerClosed,
            Error::NotFound,
            Error::NoMem,
            Error::Crashed,
            Error::Rights,
            Error::TooSmall,
            Error::Killed,
            Error::Busy,
            Error::Again,
            Error::Exists,
            Error::NotEmpty,
            Error::NotDir,
        ] {
            assert!(e.as_return() < 0);
            assert_eq!(Error::from_return(e.as_return()), Some(e));
        }
        assert_eq!(Error::from_return(0), None);
        assert_eq!(Error::from_return(-99), None);
    }
}
