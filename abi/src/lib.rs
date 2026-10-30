//! ZooOS system call ABI, executable format checks and initial-filesystem
//! format (docs/m3-design.md). `no_std`, no dependencies, no unsafe code.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

pub mod elf;
pub mod tar;

/// Returned by syscall 0. Bumped when calls are added or change meaning.
/// Version 2 (M4) added calls 11-16 and errors 13-14; version 3 (M5a)
/// added calls 17-18; version 4 (M5c-1) added calls 19-26 and errors 15-17;
/// version 5 (POSIX step 1) added calls 27-31 and error 18; version 6
/// (POSIX step 2) added calls 32-37; version 7 (POSIX step 3) added calls
/// 38-40.
pub const ABI_VERSION: u64 = 7;

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
    pub const THREAD_CREATE: u64 = 27;
    pub const THREAD_EXIT: u64 = 28;
    pub const FUTEX_WAIT: u64 = 29;
    pub const FUTEX_WAKE: u64 = 30;
    pub const SET_FS_BASE: u64 = 31;
    pub const VM_RESERVE: u64 = 32;
    pub const VM_COMMIT: u64 = 33;
    pub const VM_PROTECT: u64 = 34;
    pub const VM_DECOMMIT: u64 = 35;
    pub const VM_RELEASE: u64 = 36;
    pub const VM_QUERY: u64 = 37;
    pub const SET_EXCEPTION_HANDLER: u64 = 38;
    pub const SET_EXCEPTION_STACK: u64 = 39;
    pub const EXCEPTION_RETURN: u64 = 40;
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
    TimedOut = 18,
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
            -18 => Error::TimedOut,
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
            Error::TimedOut => "TIMEDOUT",
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

/// Address-space window for `vm_*` calls (docs/posix2-design.md §2):
/// between the program image and the heap.
pub const VM_START: u64 = 0x0000_0000_4000_0000;
pub const VM_END: u64 = 0x0000_1000_0000_0000;
/// Kernel-chosen reservations are aligned to this (as on Windows).
pub const VM_GRANULE: u64 = 64 * 1024;
/// Most committed bytes per process through `vm_commit`.
pub const VM_COMMIT_LIMIT: u64 = 1 << 30;
/// Most reservations per process.
pub const VM_MAX_RESERVATIONS: usize = 256;

/// Page protection for `vm_commit` and `vm_protect`; must include READ.
pub mod prot {
    pub const READ: u64 = 1;
    pub const WRITE: u64 = 2;
    pub const EXEC: u64 = 4;
}

pub const VM_FREE: u64 = 0;
pub const VM_RESERVED: u64 = 1;
pub const VM_COMMITTED: u64 = 2;

/// Filled in by `vm_query`: the run of pages from the queried address that
/// share one state (and, if committed, one protection).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VmInfo {
    pub base: u64,
    pub len: u64,
    pub state: u64,
    pub prot: u64,
    /// The enclosing reservation (0, 0 when free).
    pub reservation: u64,
    pub reservation_len: u64,
}

/// What a user exception handler receives (docs/posix3-design.md §2): the
/// fault, and the interrupted thread's registers, which `exception_return`
/// restores (possibly edited).
#[repr(C, align(16))]
#[derive(Clone, Copy)]
pub struct ExceptionContext {
    /// CPU exception vector (0 divide, 6 invalid opcode, 13 general
    /// protection, 14 page fault, 16 x87, 19 SIMD, ...).
    pub vector: u64,
    pub error: u64,
    /// Faulting address for page faults (CR2), else 0.
    pub addr: u64,
    pub rip: u64,
    pub rsp: u64,
    pub rflags: u64,
    pub rax: u64,
    pub rbx: u64,
    pub rcx: u64,
    pub rdx: u64,
    pub rsi: u64,
    pub rdi: u64,
    pub rbp: u64,
    pub r8: u64,
    pub r9: u64,
    pub r10: u64,
    pub r11: u64,
    pub r12: u64,
    pub r13: u64,
    pub r14: u64,
    pub r15: u64,
    pub fs_base: u64,
    /// The FXSAVE image of the x87/SSE registers.
    pub fx: [u8; 512],
}

/// `exception_return`'s second argument.
pub const EXC_RESUME: u64 = 0;
/// The handler gives up: the process crashes as if it had no handler.
pub const EXC_UNHANDLED: u64 = 1;

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
            Error::TimedOut,
        ] {
            assert!(e.as_return() < 0);
            assert_eq!(Error::from_return(e.as_return()), Some(e));
        }
        assert_eq!(Error::from_return(0), None);
        assert_eq!(Error::from_return(-99), None);
    }
}
