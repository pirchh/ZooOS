//! `zoo_rt`: the minimal runtime for ZooOS user programs. Provides the
//! `_start` entry point (via `entry!`), system call wrappers, formatted
//! logging and a panic handler that exits with code 101.

#![no_std]

extern crate alloc;

use core::arch::asm;
use core::fmt;
pub use zoo_abi::{
    DirEntry, DisplayInfo, Error, FsArgs, FsEntry, MapInfo, NO_HANDLE, dir_rights, sys,
};

pub mod app;
pub mod exception;
mod heap;
pub mod thread;
pub mod vm;

/// A handle value as returned by the kernel.
pub type Handle = u64;

/// Raw system call: number in `rax`, up to three arguments. Returns the raw
/// result (negative = `-error`).
#[inline(always)]
pub fn syscall(number: u64, a1: u64, a2: u64, a3: u64) -> i64 {
    let ret: i64;
    // SAFETY: `int 0x80` enters the kernel's system call gate, which saves
    // and restores every register except `rax` (the result). The kernel may
    // write to memory the arguments point at, so memory is not declared
    // untouched; the stack is not used by the instruction itself.
    unsafe {
        asm!(
            "int 0x80",
            inlateout("rax") number as i64 => ret,
            in("rdi") a1,
            in("rsi") a2,
            in("rdx") a3,
            options(nostack),
        );
    }
    ret
}

fn result(ret: i64) -> Result<u64, Error> {
    if ret >= 0 {
        Ok(ret as u64)
    } else {
        Err(Error::from_return(ret).unwrap_or(Error::Inval))
    }
}

pub fn abi_version() -> u64 {
    syscall(sys::ABI_VERSION, 0, 0, 0) as u64
}

pub fn exit(code: u8) -> ! {
    syscall(sys::EXIT, code as u64, 0, 0);
    // The kernel never returns from exit; if it did, spin rather than run on.
    loop {
        core::hint::spin_loop();
    }
}

pub fn yield_now() {
    syscall(sys::YIELD, 0, 0, 0);
}

pub fn log(text: &str) -> Result<u64, Error> {
    result(syscall(
        sys::LOG,
        text.as_ptr() as u64,
        text.len() as u64,
        0,
    ))
}

pub fn channel_create() -> Result<(Handle, Handle), Error> {
    let mut pair = [0u64; 2];
    result(syscall(sys::CHANNEL_CREATE, pair.as_mut_ptr() as u64, 0, 0))?;
    Ok((pair[0], pair[1]))
}

/// Starts program `name` from the initial filesystem, moving `give` (if any)
/// into it as its start-up handle. Returns a process handle.
pub fn spawn(name: &str, give: Option<Handle>) -> Result<Handle, Error> {
    let give = give.unwrap_or(NO_HANDLE);
    result(syscall(
        sys::SPAWN,
        name.as_ptr() as u64,
        name.len() as u64,
        give,
    ))
}

pub fn send(h: Handle, msg: &[u8]) -> Result<(), Error> {
    result(syscall(sys::SEND, h, msg.as_ptr() as u64, msg.len() as u64)).map(|_| ())
}

/// Blocks until a message arrives; returns its length.
pub fn recv(h: Handle, buf: &mut [u8]) -> Result<usize, Error> {
    result(syscall(
        sys::RECV,
        h,
        buf.as_mut_ptr() as u64,
        buf.len() as u64,
    ))
    .map(|n| n as usize)
}

/// Blocks until the process ends; returns its exit code, or `Crashed`.
pub fn wait(h: Handle) -> Result<u8, Error> {
    result(syscall(sys::WAIT, h, 0, 0)).map(|c| c as u8)
}

/// Ends the process behind a process handle (the supervisor's last resort).
pub fn kill(h: Handle) -> Result<(), Error> {
    result(syscall(sys::KILL, h, 0, 0)).map(|_| ())
}

/// Timer ticks (100 Hz) since boot.
pub fn ticks() -> u64 {
    syscall(sys::TICKS, 0, 0, 0) as u64
}

/// Blocks for at least `ticks` timer ticks.
pub fn sleep(ticks: u64) {
    syscall(sys::SLEEP, ticks, 0, 0);
}

/// Like `recv`, but returns `Err(Again)` instead of blocking.
pub fn try_recv(h: Handle, buf: &mut [u8]) -> Result<usize, Error> {
    result(syscall(
        sys::TRY_RECV,
        h,
        buf.as_mut_ptr() as u64,
        buf.len() as u64,
    ))
    .map(|n| n as usize)
}

/// Takes the display and keyboard; the framebuffer is mapped at
/// `info.base`.
pub fn display_acquire() -> Result<DisplayInfo, Error> {
    let mut info = DisplayInfo::default();
    result(syscall(
        sys::DISPLAY_ACQUIRE,
        &mut info as *mut DisplayInfo as u64,
        0,
        0,
    ))?;
    Ok(info)
}

/// Copies waiting keyboard scancode bytes (display owner only).
pub fn input_read(buf: &mut [u8]) -> Result<usize, Error> {
    result(syscall(
        sys::INPUT_READ,
        buf.as_mut_ptr() as u64,
        buf.len() as u64,
        0,
    ))
    .map(|n| n as usize)
}

/// Describes the `index`-th file of the initial filesystem.
pub fn fs_stat(index: u64) -> Result<FsEntry, Error> {
    let mut e = FsEntry::default();
    result(syscall(
        sys::FS_STAT,
        index,
        &mut e as *mut FsEntry as u64,
        0,
    ))?;
    Ok(e)
}

/// Maps `len` bytes of new zeroed heap memory; returns its address.
pub fn mem_alloc(len: u64) -> Result<u64, Error> {
    result(syscall(sys::MEM_ALLOC, len, 0, 0))
}

/// Maps the `index`-th initial-filesystem file read-only and returns its
/// contents. The mapping lasts for the life of the process.
pub fn fs_map(index: u64) -> Result<&'static [u8], Error> {
    let mut info = MapInfo::default();
    result(syscall(
        sys::FS_MAP,
        index,
        &mut info as *mut MapInfo as u64,
        0,
    ))?;
    // SAFETY: the kernel mapped `len` readable bytes at `addr` for the rest
    // of this process's life and never changes them.
    Ok(unsafe { core::slice::from_raw_parts(info.addr as *const u8, info.len as usize) })
}

/// Index of the initial-filesystem file named `name`, if present.
pub fn fs_find(name: &str) -> Option<u64> {
    (0..)
        .map_while(|i| fs_stat(i).ok().map(|e| (i, e)))
        .find(|(_, e)| e.name() == name)
        .map(|(i, _)| i)
}

/// The floating-point workload used to check that x87/SSE state is kept
/// per process (docs/m5a-design.md §5). Deterministic: the same `param`
/// always gives the same bits. Keeps every 1024th partial sum in `trace`.
pub fn narwhal_series(param: f64, n: u64, trace: &mut alloc::vec::Vec<f64>) -> f64 {
    let mut acc = 0.0f64;
    let mut x = 1.0f64;
    for i in 0..n {
        let fi = i as f64;
        x = (fi * param + 1.0) / (1.0 + fi * 0.5) + x * 1e-3;
        acc = acc * 0.999 + x * x;
        if i % 1024 == 0 {
            trace.push(acc);
        }
    }
    acc + trace.iter().sum::<f64>() * 1e-9
}

/// The root directory handle; only for a process the kernel started, once.
pub fn fs_root() -> Result<Handle, Error> {
    result(syscall(sys::FS_ROOT, 0, 0, 0))
}

fn dir_call(number: u64, dir: Handle, path: &str, arg: u64, buf: &mut [u8]) -> Result<u64, Error> {
    let args = FsArgs {
        path: path.as_ptr() as u64,
        path_len: path.len() as u64,
        arg,
        buf: buf.as_mut_ptr() as u64,
        buf_len: buf.len() as u64,
    };
    result(syscall(number, dir, &args as *const FsArgs as u64, 0))
}

fn entry_call(number: u64, dir: Handle, path: &str, arg: u64) -> Result<DirEntry, Error> {
    let mut e = DirEntry::default();
    // SAFETY: `DirEntry` is plain `repr(C)` integers and bytes; any bytes
    // the kernel writes are a valid value. The slice covers exactly `e`.
    let buf = unsafe {
        core::slice::from_raw_parts_mut(
            &mut e as *mut DirEntry as *mut u8,
            core::mem::size_of::<DirEntry>(),
        )
    };
    dir_call(number, dir, path, arg, buf)?;
    Ok(e)
}

/// A handle to the directory at `path` under `dir`, with `rights` (no wider
/// than `dir`'s).
pub fn dir_open(dir: Handle, path: &str, rights: u64) -> Result<Handle, Error> {
    dir_call(sys::DIR_OPEN, dir, path, rights, &mut [])
}

/// The `index`-th entry of the directory at `path`; `NOTFOUND` past the end.
pub fn dir_list(dir: Handle, path: &str, index: u64) -> Result<DirEntry, Error> {
    entry_call(sys::DIR_LIST, dir, path, index)
}

/// Kind and size of `path` (its name is left empty).
pub fn dir_stat(dir: Handle, path: &str) -> Result<DirEntry, Error> {
    entry_call(sys::DIR_STAT, dir, path, 0)
}

/// Reads file `path` from `offset` into `buf`; returns the bytes read.
pub fn file_read(dir: Handle, path: &str, offset: u64, buf: &mut [u8]) -> Result<usize, Error> {
    dir_call(sys::FILE_READ, dir, path, offset, buf).map(|n| n as usize)
}

pub fn dir_create(dir: Handle, path: &str) -> Result<(), Error> {
    dir_call(sys::DIR_CREATE, dir, path, 0, &mut []).map(|_| ())
}

/// Removes the empty directory at `path`.
pub fn dir_remove(dir: Handle, path: &str) -> Result<(), Error> {
    dir_call(sys::DIR_REMOVE, dir, path, 0, &mut []).map(|_| ())
}

fn path_call(number: u64, dir: Handle, path: &str, arg: u64, buf: &[u8]) -> Result<u64, Error> {
    let args = FsArgs {
        path: path.as_ptr() as u64,
        path_len: path.len() as u64,
        arg,
        buf: buf.as_ptr() as u64,
        buf_len: buf.len() as u64,
    };
    result(syscall(number, dir, &args as *const FsArgs as u64, 0))
}

/// Creates an empty file at `path` (needs CREATE).
pub fn file_create(dir: Handle, path: &str) -> Result<(), Error> {
    path_call(sys::FILE_CREATE, dir, path, 0, &[]).map(|_| ())
}

/// Writes `data` at `offset` of file `path`, growing it (needs WRITE).
pub fn file_write(dir: Handle, path: &str, offset: u64, data: &[u8]) -> Result<usize, Error> {
    path_call(sys::FILE_WRITE, dir, path, offset, data).map(|n| n as usize)
}

/// Sets the length of file `path` (needs WRITE).
pub fn file_truncate(dir: Handle, path: &str, len: u64) -> Result<(), Error> {
    path_call(sys::FILE_TRUNCATE, dir, path, len, &[]).map(|_| ())
}

/// Removes file `path` (needs REMOVE).
pub fn file_remove(dir: Handle, path: &str) -> Result<(), Error> {
    path_call(sys::FILE_REMOVE, dir, path, 0, &[]).map(|_| ())
}

/// Moves `from` to `to`, both under `dir` (needs CREATE and REMOVE).
pub fn rename(dir: Handle, from: &str, to: &str) -> Result<(), Error> {
    path_call(sys::RENAME, dir, from, 0, to.as_bytes()).map(|_| ())
}

/// Starts `name`, moving up to two handles into it (its `main` receives
/// them). Returns a process handle.
pub fn spawn_with(name: &str, give: [Option<Handle>; 2]) -> Result<Handle, Error> {
    let values = give.map(|h| h.unwrap_or(NO_HANDLE));
    result(syscall(
        sys::SPAWN_WITH,
        name.as_ptr() as u64,
        name.len() as u64,
        values.as_ptr() as u64,
    ))
}

/// A log-reader handle (the resident supervisor only).
pub fn log_attach() -> Result<Handle, Error> {
    result(syscall(sys::LOG_ATTACH, 0, 0, 0))
}

/// One line from the kernel log buffer (`<tick> <pid> <name> <text>`);
/// `AGAIN` when it is empty.
pub fn log_read(reader: Handle, buf: &mut [u8]) -> Result<usize, Error> {
    result(syscall(
        sys::LOG_READ,
        reader,
        buf.as_mut_ptr() as u64,
        buf.len() as u64,
    ))
    .map(|n| n as usize)
}

/// Pid of the process behind handle `h`, or the caller's own for `NO_HANDLE`.
pub fn process_id(h: Handle) -> Result<u64, Error> {
    result(syscall(sys::PROCESS_ID, h, 0, 0))
}

pub fn close(h: Handle) -> Result<(), Error> {
    result(syscall(sys::CLOSE, h, 0, 0)).map(|_| ())
}

/// A fixed-size line buffer for formatted logging (no heap in user space).
pub struct Line {
    buf: [u8; zoo_abi::MAX_MESSAGE],
    len: usize,
}

impl Line {
    pub const fn new() -> Self {
        Line {
            buf: [0; zoo_abi::MAX_MESSAGE],
            len: 0,
        }
    }

    pub fn as_str(&self) -> &str {
        // Only whole `&str`s are ever appended, cut at char boundaries.
        core::str::from_utf8(&self.buf[..self.len]).unwrap_or("?")
    }
}

impl Default for Line {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Write for Line {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let room = self.buf.len() - self.len;
        let mut take = s.len().min(room);
        while !s.is_char_boundary(take) {
            take -= 1;
        }
        self.buf[self.len..self.len + take].copy_from_slice(&s.as_bytes()[..take]);
        self.len += take;
        Ok(())
    }
}

/// `log!("x = {}", x)`: formats into a `Line` and logs it.
#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => {{
        use core::fmt::Write as _;
        let mut line = $crate::Line::new();
        let _ = write!(line, $($arg)*);
        let _ = $crate::log(line.as_str());
    }};
}

/// Defines `_start`, which calls `main(start_handle) -> u8` and exits with
/// the result.
#[macro_export]
macro_rules! entry {
    ($main:path) => {
        #[unsafe(no_mangle)]
        pub extern "C" fn _start(start_handle: u64) -> ! {
            let code: u8 = $main(start_handle);
            $crate::exit(code)
        }
    };
}

/// Like `entry!`, for programs started by `spawn_with`: `main(first,
/// second) -> u8` receives both start-up handles.
#[macro_export]
macro_rules! entry2 {
    ($main:path) => {
        #[unsafe(no_mangle)]
        pub extern "C" fn _start(first: u64, second: u64) -> ! {
            let code: u8 = $main(first, second);
            $crate::exit(code)
        }
    };
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    log!("panic: {}", info.message());
    exit(101)
}
