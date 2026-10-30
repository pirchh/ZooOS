//! `zoo_rt`: the minimal runtime for ZooOS user programs. Provides the
//! `_start` entry point (via `entry!`), system call wrappers, formatted
//! logging and a panic handler that exits with code 101.

#![no_std]

use core::arch::asm;
use core::fmt;
pub use zoo_abi::{Error, NO_HANDLE, sys};

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

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    log!("panic: {}", info.message());
    exit(101)
}
