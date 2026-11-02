//! ZooOS system call ABI, executable format checks and initial-filesystem
//! format (docs/m3-design.md). `no_std`, no dependencies, no unsafe code.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

pub mod elf;
pub mod font;
pub mod tar;
pub mod ui;

/// Returned by syscall 0. Bumped when calls are added or change meaning.
/// Version 2 (M4) added calls 11-16 and errors 13-14; version 3 (M5a)
/// added calls 17-18; version 4 (M5c-1) added calls 19-26 and errors 15-17;
/// version 5 (POSIX step 1) added calls 27-31 and error 18; version 6
/// (POSIX step 2) added calls 32-37; version 7 (POSIX step 3) added calls
/// 38-40; version 8 (POSIX step 4) added calls 41-45 and the WRITE right;
/// version 9 (services step 1) added call 46; version 10 (services step 2)
/// added calls 47-48; version 11 (services step 3) added calls 49-50 and
/// error 19; version 12 (services step 4a) added call 51.
/// Version 13 (services step 4b) removed the in-kernel filesystem: calls
/// 19-25 and 41-45 now return `NOSYS` (their numbers stay reserved).
/// Version 14 (services step 5) added calls 52-55; version 15 (desktop
/// step 1) added call 56; version 16 (POSIX step 5b-1) added calls 57-58
/// and command lines in `spawn`; version 17 (fast files step 1) added
/// calls 59-61.
pub const ABI_VERSION: u64 = 17;

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
    pub const FILE_CREATE: u64 = 41;
    pub const FILE_WRITE: u64 = 42;
    pub const FILE_TRUNCATE: u64 = 43;
    pub const FILE_REMOVE: u64 = 44;
    pub const RENAME: u64 = 45;
    /// Pid of a process handle, or of the caller for `NO_HANDLE`.
    pub const PROCESS_ID: u64 = 46;
    /// A log-reader handle (the resident supervisor only).
    pub const LOG_ATTACH: u64 = 47;
    /// Next line from the kernel log buffer.
    pub const LOG_READ: u64 = 48;
    /// Moves a handle into a channel, with a tag.
    pub const SEND_HANDLE: u64 = 49;
    /// Takes a handle message from a channel.
    pub const RECV_HANDLE: u64 = 50;
    /// Waits until one of several handles is ready, or a timeout.
    pub const WAIT_ANY: u64 = 51;
    /// A block-device handle (the resident supervisor only).
    pub const BLOCK_ATTACH: u64 = 52;
    /// Reads one 4 KiB block.
    pub const BLOCK_READ: u64 = 53;
    /// Writes one 4 KiB block.
    pub const BLOCK_WRITE: u64 = 54;
    /// Device size in bytes.
    pub const BLOCK_SIZE: u64 = 55;
    /// Mouse events, for the display owner.
    pub const MOUSE_READ: u64 = 56;
    /// The process's command line (docs/posix5b1-design.md).
    pub const ARGS_GET: u64 = 57;
    /// Wall-clock or monotonic time in nanoseconds.
    pub const CLOCK: u64 = 58;
    /// Shared memory (docs/f1-design.md): create, map, unmap.
    pub const SHM_CREATE: u64 = 59;
    pub const SHM_MAP: u64 = 60;
    pub const SHM_UNMAP: u64 = 61;
    /// Reads or writes up to `BLOCK_RUN_MAX` consecutive blocks in one
    /// request (docs/linux10c-design.md): the second argument is the first
    /// block in bits 0..48 and the count in bits 48..64.
    pub const BLOCK_READ_RUN: u64 = 62;
    pub const BLOCK_WRITE_RUN: u64 = 63;
    /// Display owner only (docs/u1b-design.md): blocks until keyboard or
    /// mouse input arrives, any wake-up, or `a1` ticks pass.
    pub const INPUT_WAIT: u64 = 64;
    /// Display owner only: writes [events, total wait, longest wait] (TSC
    /// cycles from a mouse event's arrival to `MOUSE_READ`) to `a1` and
    /// restarts the counts. A measurement aid (docs/u1b-design.md).
    pub const INPUT_LATENCY: u64 = 65;
    /// Display owner, when `DisplayInfo::flags` has `DISPLAY_FLUSH`
    /// (docs/gpu1-design.md): shows rectangle `a1` = x | y << 16 |
    /// w << 32 | h << 48 of the display memory.
    pub const DISPLAY_FLUSH: u64 = 66;
    /// Display owner, with `DISPLAY_HW_CURSOR`: sets the cursor image
    /// (`a1`: `CURSOR_SIZE`² pixels of 0xAARRGGBB), its hot spot (`a2` =
    /// x | y << 16) and position (`a3` = x | y << 16).
    pub const CURSOR_SET: u64 = 67;
    /// Display owner, with `DISPLAY_HW_CURSOR`: moves the cursor to
    /// (`a1`, `a2`).
    pub const CURSOR_MOVE: u64 = 68;
    /// The network device (docs/n1-design.md): a handle to it, for one
    /// process at a time (`BUSY` while another live process has one).
    pub const NET_ATTACH: u64 = 69;
    /// Sends the Ethernet frame `a2`/`a3` (at most `NET_MAX_FRAME` bytes).
    pub const NET_SEND: u64 = 70;
    /// Takes one received frame into `a2`/`a3`: its length, or `AGAIN`.
    pub const NET_RECV: u64 = 71;
    /// The device's MAC address, in the low 48 bits (first byte lowest).
    pub const NET_MAC: u64 = 72;
}

/// Largest Ethernet frame `NET_SEND` takes and `NET_RECV` returns.
pub const NET_MAX_FRAME: usize = 1514;

/// The hardware cursor image is `CURSOR_SIZE` x `CURSOR_SIZE` pixels.
pub const CURSOR_SIZE: usize = 64;
/// `DisplayInfo::flags`: changes show only after `DISPLAY_FLUSH`.
pub const DISPLAY_FLUSH_NEEDED: u64 = 1;
/// `DisplayInfo::flags`: a hardware cursor (`CURSOR_SET`, `CURSOR_MOVE`).
pub const DISPLAY_HW_CURSOR: u64 = 2;

/// Most blocks in one `BLOCK_READ_RUN`/`BLOCK_WRITE_RUN`.
pub const BLOCK_RUN_MAX: usize = 16;

/// Most pages in one shared-memory object.
pub const SHM_MAX_PAGES: u64 = 256;

/// `clock` argument: nanoseconds since 1970-01-01 UTC.
pub const CLOCK_REALTIME: u64 = 0;
/// `clock` argument: nanoseconds since boot.
pub const CLOCK_MONOTONIC: u64 = 1;
/// Longest command line `spawn` accepts: the program name, then each
/// argument after a NUL byte.
pub const MAX_CMDLINE: usize = 512;

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
    /// The next message is of the other kind (bytes vs handle); it stays
    /// queued.
    WrongKind = 19,
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
            -19 => Error::WrongKind,
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
            Error::WrongKind => "WRONGKIND",
        }
    }
}

/// Largest `log` and channel message, in bytes (1 KiB since L10d, for
/// long paths; messages are stored at their own length).
pub const MAX_MESSAGE: usize = 1024;
/// Messages queued per channel direction.
pub const CHANNEL_DEPTH: usize = 8;
/// Handle slots per process.
pub const MAX_HANDLES: usize = 64;
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
    /// `DISPLAY_FLUSH_NEEDED`, `DISPLAY_HW_CURSOR`.
    pub flags: u64,
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

/// Filled in by `recv_handle`: the received handle and its tag.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HandleMsg {
    pub handle: u64,
    pub tag: u64,
}

/// One mouse event from `mouse_read`: relative motion (`dy` positive
/// upwards, as the mouse reports it) and the buttons held (bit 0 left,
/// bit 1 right, bit 2 middle). With `MOUSE_ABSOLUTE` set in `buttons`,
/// `dx`/`dy` are instead a position: 0 to `MOUSE_ABS_MAX` across the
/// screen, `dy` growing downwards (a tablet, docs/u1c-design.md).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MouseEvent {
    pub dx: i16,
    pub dy: i16,
    pub buttons: u8,
    pub _pad: [u8; 3],
}

/// `MouseEvent::buttons` flag: an absolute position.
pub const MOUSE_ABSOLUTE: u8 = 0x80;
/// The largest absolute coordinate (the far edge of the screen).
pub const MOUSE_ABS_MAX: i16 = 0x7FFF;

/// Bytes per block for `block_read`/`block_write`.
pub const BLOCK_BYTES: usize = 4096;

/// Most handles one `wait_any` call watches.
pub const WAIT_ANY_MAX: usize = 64;
/// `wait_any` timeout meaning "no timeout".
pub const WAIT_FOREVER: u64 = u64::MAX;

/// `recv_handle` flag: return `AGAIN` instead of blocking.
pub const RECV_NONBLOCK: u64 = 1;

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
    /// Remove empty directories and files below it.
    pub const REMOVE: u64 = 4;
    /// Write and truncate files below it.
    pub const WRITE: u64 = 8;
    pub const ALL: u64 = READ | CREATE | REMOVE | WRITE;
}

/// Longest path accepted by the directory calls, and longest name.
pub const MAX_FS_PATH: usize = 1000;
pub const MAX_DIR_NAME: usize = 255;

pub const KIND_DIR: u64 = 1;
pub const KIND_FILE: u64 = 2;
/// A symbolic link (docs/linux10d-design.md); its size is the target's
/// length.
pub const KIND_LINK: u64 = 3;

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
            Error::WrongKind,
        ] {
            assert!(e.as_return() < 0);
            assert_eq!(Error::from_return(e.as_return()), Some(e));
        }
        assert_eq!(Error::from_return(0), None);
        assert_eq!(Error::from_return(-99), None);
    }
}
