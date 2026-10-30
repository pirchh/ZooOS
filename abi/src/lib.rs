//! ZooOS system call ABI, executable format checks and initial-filesystem
//! format (docs/m3-design.md). `no_std`, no dependencies, no unsafe code.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

pub mod elf;
pub mod tar;

/// Returned by syscall 0. Bumped when calls are added or change meaning.
/// Version 2 (M4) added calls 11-16 and errors 13-14.
pub const ABI_VERSION: u64 = 2;

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
        ] {
            assert!(e.as_return() < 0);
            assert_eq!(Error::from_return(e.as_return()), Some(e));
        }
        assert_eq!(Error::from_return(0), None);
        assert_eq!(Error::from_return(-99), None);
    }
}
