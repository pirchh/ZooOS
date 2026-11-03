//! `zoo_rt`: the minimal runtime for ZooOS user programs. Provides the
//! `_start` entry point (via `entry!`), system call wrappers, formatted
//! logging and a panic handler that exits with code 101.

#![no_std]

extern crate alloc;

use core::arch::asm;
use core::fmt;
pub use zoo_abi::{DirEntry, DisplayInfo, Error, FsEntry, MapInfo, NO_HANDLE, dir_rights, sys};

pub mod app;
pub mod exception;
pub mod fsproto;
mod heap;
pub use heap::stats as heap_stats;
pub mod net;
pub mod pool;
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

// ---- Files and folders through BadgerHandler (docs/s4-design.md) ----------
//
// A folder handle is a channel to BadgerHandler. These keep the signatures
// the system calls had; each is one or more request/reply exchanges. One
// request at a time per folder handle (threads sharing a handle must take
// turns).

use fsproto::Op;

/// A handle to the folder at `path` under `dir`, with `rights` (no wider
/// than `dir`'s).
pub fn dir_open(dir: Handle, path: &str, rights: u64) -> Result<Handle, Error> {
    fsproto::call(dir, Op::Open, rights, 0, path, &[], &mut [])?;
    fsproto::take_dir(dir)
}

/// The `index`-th entry of the folder at `path`; `NOTFOUND` past the end.
pub fn dir_list(dir: Handle, path: &str, index: u64) -> Result<DirEntry, Error> {
    let mut b = [0u8; zoo_abi::MAX_MESSAGE];
    let (_, n) = fsproto::call(dir, Op::List, index, 0, path, &[], &mut b)?;
    fsproto::decode_entry(&b[..n]).ok_or(Error::Inval)
}

/// Kind and size of `path` (its name is left empty).
pub fn dir_stat(dir: Handle, path: &str) -> Result<DirEntry, Error> {
    let mut b = [0u8; zoo_abi::MAX_MESSAGE];
    let (_, n) = fsproto::call(dir, Op::Stat, 0, 0, path, &[], &mut b)?;
    fsproto::decode_entry(&b[..n]).ok_or(Error::Inval)
}

/// Shared memory: `pages` zeroed pages as a handle (docs/f1-design.md).
pub fn shm_create(pages: u64) -> Result<Handle, Error> {
    result(syscall(sys::SHM_CREATE, pages, 0, 0))
}

/// Maps shared memory read/write into this process; returns its address.
pub fn shm_map(h: Handle) -> Result<u64, Error> {
    result(syscall(sys::SHM_MAP, h, 0, 0))
}

/// The model's length in bytes on the model disk (docs/stage2-design.md §3).
pub fn model_size() -> Result<u64, Error> {
    model_size_of(zoo_abi::MODEL_CHAT)
}

/// Model `which`'s length in bytes on the model disk
/// (`zoo_abi::MODEL_CHAT`, `zoo_abi::MODEL_EMBED`).
pub fn model_size_of(which: u64) -> Result<u64, Error> {
    result(syscall(sys::MODEL_SIZE, which, 0, 0))
}

/// Reads `buf.len() / 4096` blocks of the model from its block `block`
/// (at most `zoo_abi::BLOCK_RUN_MAX`; `buf.len()` a multiple of 4 KiB).
pub fn model_read(block: u64, buf: &mut [u8]) -> Result<(), Error> {
    model_read_of(zoo_abi::MODEL_CHAT, block, buf)
}

/// `model_read` of model `which`.
pub fn model_read_of(which: u64, block: u64, buf: &mut [u8]) -> Result<(), Error> {
    let count = (buf.len() / zoo_abi::BLOCK_BYTES) as u64;
    result(syscall(
        sys::MODEL_READ,
        block | (which << zoo_abi::MODEL_WHICH_SHIFT),
        count,
        buf.as_mut_ptr() as u64,
    ))
    .map(|_| ())
}

/// Processors online (docs/smp-design.md).
pub fn cpu_count() -> usize {
    result(syscall(sys::CPU_COUNT, 0, 0, 0)).map_or(1, |n| n.max(1) as usize)
}

/// Runs this program only when no normal or background program is ready
/// (docs/vectors-design.md): for work that may wait indefinitely.
pub fn set_idle() -> Result<(), Error> {
    result(syscall(sys::SET_BACKGROUND, 2, 0, 0)).map(|_| ())
}

/// Marks this program as background work (`true`): it runs only when no
/// other program is ready, and input preempts it (docs/stage2-design.md).
pub fn set_background(on: bool) -> Result<(), Error> {
    result(syscall(sys::SET_BACKGROUND, u64::from(on), 0, 0)).map(|_| ())
}

/// The pid of the process holding the other end of channel `h`.
pub fn channel_peer(h: Handle) -> Result<u64, Error> {
    result(syscall(sys::CHANNEL_PEER, h, 0, 0))
}

/// The size in bytes of shared-memory object `h`.
pub fn shm_size(h: Handle) -> Result<u64, Error> {
    result(syscall(sys::SHM_SIZE, h, 0, 0))
}

/// Unmaps what `shm_map` mapped at `addr`.
pub fn shm_unmap(addr: u64) -> Result<(), Error> {
    result(syscall(sys::SHM_UNMAP, addr, 0, 0)).map(|_| ())
}

/// Transfers above this size use the folder's shared buffer.
const BULK_MIN: usize = 2048;

/// Folder handles with a shared buffer attached: (handle, address). A
/// small table; a folder not in it uses the message path.
static BULK: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
static mut BULK_TABLE: [(Handle, u64); 8] = [(NO_HANDLE, 0); 8];

/// The shared buffer for folder `dir`, attaching one on first use.
fn bulk_buffer(dir: Handle) -> Option<u64> {
    use core::sync::atomic::Ordering;
    // A simple lock: the table is touched briefly, by any thread.
    while BULK
        .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        core::hint::spin_loop();
    }
    // SAFETY: only touched while holding the `BULK` lock above.
    let table = unsafe { &mut *core::ptr::addr_of_mut!(BULK_TABLE) };
    let found = table.iter().find(|e| e.0 == dir).map(|e| e.1);
    let result = found.or_else(|| {
        let slot = table.iter().position(|e| e.0 == NO_HANDLE)?;
        let h = shm_create(fsproto::BULK_PAGES).ok()?;
        let addr = match shm_map(h) {
            Ok(a) => a,
            Err(_) => {
                let _ = close(h);
                return None;
            }
        };
        if send_handle(dir, h, fsproto::TAG_SHM).is_err() {
            let _ = close(h);
            let _ = shm_unmap(addr);
            return None;
        }
        table[slot] = (dir, addr);
        Some(addr)
    });
    BULK.store(false, Ordering::Release);
    result
}

/// Reads file `path` from `offset` into `buf`; returns the bytes read.
pub fn file_read(dir: Handle, path: &str, offset: u64, buf: &mut [u8]) -> Result<usize, Error> {
    if buf.len() > BULK_MIN
        && let Some(shared) = bulk_buffer(dir)
    {
        let mut done = 0;
        while done < buf.len() {
            let want = (buf.len() - done).min(fsproto::BULK_SIZE);
            let len = (want as u32).to_le_bytes();
            let (n, _) = fsproto::call(
                dir,
                Op::ReadBulk,
                offset + done as u64,
                0,
                path,
                &len,
                &mut [],
            )?;
            let n = (n as usize).min(want);
            // SAFETY: the reply came, so BadgerHandler has finished with
            // the shared buffer (requests and replies alternate), and `n`
            // fits in it.
            unsafe {
                core::ptr::copy_nonoverlapping(shared as *const u8, buf[done..].as_mut_ptr(), n)
            };
            if n == 0 {
                break;
            }
            done += n;
        }
        return Ok(done);
    }
    let mut done = 0;
    while done < buf.len() {
        let want = (buf.len() - done).min(fsproto::MAX_DATA);
        let (n, _) = fsproto::call(
            dir,
            Op::Read,
            offset + done as u64,
            want as u16,
            path,
            &[],
            &mut buf[done..done + want],
        )?;
        if n == 0 {
            break;
        }
        done += n as usize;
    }
    Ok(done)
}

pub fn dir_create(dir: Handle, path: &str) -> Result<(), Error> {
    fsproto::call(dir, Op::CreateDir, 0, 0, path, &[], &mut []).map(|_| ())
}

/// Removes the empty folder at `path`.
pub fn dir_remove(dir: Handle, path: &str) -> Result<(), Error> {
    fsproto::call(dir, Op::RemoveDir, 0, 0, path, &[], &mut []).map(|_| ())
}

/// Creates an empty file at `path` (needs CREATE).
pub fn file_create(dir: Handle, path: &str) -> Result<(), Error> {
    fsproto::call(dir, Op::CreateFile, 0, 0, path, &[], &mut []).map(|_| ())
}

/// Writes `data` at `offset` of file `path`, growing it (needs WRITE).
pub fn file_write(dir: Handle, path: &str, offset: u64, data: &[u8]) -> Result<usize, Error> {
    if data.len() > BULK_MIN
        && let Some(shared) = bulk_buffer(dir)
    {
        let mut done = 0;
        while done < data.len() {
            let n = (data.len() - done).min(fsproto::BULK_SIZE);
            // SAFETY: no request is outstanding, so BadgerHandler is not
            // using the buffer; `n` fits in it.
            unsafe { core::ptr::copy_nonoverlapping(data[done..].as_ptr(), shared as *mut u8, n) };
            let len = (n as u32).to_le_bytes();
            let (w, _) = fsproto::call(
                dir,
                Op::WriteBulk,
                offset + done as u64,
                0,
                path,
                &len,
                &mut [],
            )?;
            if w as usize != n {
                return Err(Error::Inval);
            }
            done += n;
        }
        return Ok(done);
    }
    let room = fsproto::room(path);
    if room == 0 {
        return Err(Error::Inval);
    }
    if data.is_empty() {
        fsproto::call(dir, Op::Write, offset, 0, path, &[], &mut [])?;
        return Ok(0);
    }
    let mut done = 0;
    while done < data.len() {
        let chunk = &data[done..(done + room).min(data.len())];
        let (n, _) = fsproto::call(
            dir,
            Op::Write,
            offset + done as u64,
            0,
            path,
            chunk,
            &mut [],
        )?;
        done += n as usize;
    }
    Ok(done)
}

/// Sets the length of file `path` (needs WRITE).
pub fn file_truncate(dir: Handle, path: &str, len: u64) -> Result<(), Error> {
    fsproto::call(dir, Op::Truncate, len, 0, path, &[], &mut []).map(|_| ())
}

/// Removes file `path` (needs REMOVE).
pub fn file_remove(dir: Handle, path: &str) -> Result<(), Error> {
    fsproto::call(dir, Op::RemoveFile, 0, 0, path, &[], &mut []).map(|_| ())
}

/// Moves `from` to `to`, both under `dir` (needs CREATE and REMOVE).
pub fn rename(dir: Handle, from: &str, to: &str) -> Result<(), Error> {
    fsproto::call(dir, Op::Rename, 0, 0, from, to.as_bytes(), &mut []).map(|_| ())
}

/// Starts a private BadgerHandler (for the boot checks) and returns
/// (process, control channel, root folder handle).
pub fn start_badger() -> Result<(Handle, Handle, Handle), Error> {
    let (mine, theirs) = channel_create()?;
    let process = spawn("badger-handler", Some(theirs))?;
    send(mine, b"root")?;
    let (root, _tag) = recv_handle(mine)?;
    Ok((process, mine, root))
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

/// Moves handle `h` into channel `chan` with `tag` (docs/s3-design.md);
/// `h` is gone from this process afterwards.
pub fn send_handle(chan: Handle, h: Handle, tag: u64) -> Result<(), Error> {
    result(syscall(sys::SEND_HANDLE, chan, h, tag)).map(|_| ())
}

fn recv_handle_flags(chan: Handle, flags: u64) -> Result<(Handle, u64), Error> {
    let mut m = zoo_abi::HandleMsg::default();
    result(syscall(
        sys::RECV_HANDLE,
        chan,
        &mut m as *mut zoo_abi::HandleMsg as u64,
        flags,
    ))?;
    Ok((m.handle, m.tag))
}

/// Takes the next message from `chan` if it is a handle: (handle, tag).
/// Blocks while the channel is empty; `WRONGKIND` if the next message is
/// bytes (it stays queued).
pub fn recv_handle(chan: Handle) -> Result<(Handle, u64), Error> {
    recv_handle_flags(chan, 0)
}

/// Like `recv_handle`, but `AGAIN` instead of blocking.
pub fn try_recv_handle(chan: Handle) -> Result<(Handle, u64), Error> {
    recv_handle_flags(chan, zoo_abi::RECV_NONBLOCK)
}

/// Waits until one of `handles` is ready (a channel with a message or a
/// closed peer, an ended process or thread) and returns its index;
/// `TIMEDOUT` after `timeout` ticks (`zoo_abi::WAIT_FOREVER` for none).
pub fn wait_any(handles: &[Handle], timeout: u64) -> Result<usize, Error> {
    result(syscall(
        sys::WAIT_ANY,
        handles.as_ptr() as u64,
        handles.len() as u64,
        timeout,
    ))
    .map(|k| k as usize)
}

/// This process's command line (program name, then arguments, separated
/// by NUL bytes); copies up to `buf.len()` bytes and returns the full
/// length.
pub fn args_get(buf: &mut [u8]) -> Result<usize, Error> {
    result(syscall(
        sys::ARGS_GET,
        buf.as_mut_ptr() as u64,
        buf.len() as u64,
        0,
    ))
    .map(|n| n as usize)
}

/// Nanoseconds: `zoo_abi::CLOCK_REALTIME` since 1970 (UTC), or
/// `zoo_abi::CLOCK_MONOTONIC` since boot.
pub fn clock(which: u64) -> Result<u64, Error> {
    result(syscall(sys::CLOCK, which, 0, 0))
}

/// Time-stamp-counter cycles per millisecond, measured once per program
/// against the 10 ms timer (the method PeacockHandler's animation clock
/// uses, docs/u2d-design.md).
static CYCLES_PER_MS: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

fn cycles() -> u64 {
    // SAFETY: `rdtsc` only reads the counter; user mode may run it (the
    // kernel does not set CR4.TSD).
    unsafe { core::arch::x86_64::_rdtsc() }
}

/// Microseconds on the CPU's cycle counter, for timing lines
/// (docs/stage1-design.md). The kernel's monotonic clock counts 10 ms ticks,
/// too coarse for them. The first call calibrates, waiting about 30 ms.
pub fn now_us() -> u64 {
    use core::sync::atomic::Ordering;
    let mut per_ms = CYCLES_PER_MS.load(Ordering::Relaxed);
    if per_ms == 0 {
        let t0 = ticks();
        while ticks() == t0 {}
        let (c0, t1) = (cycles(), ticks());
        while ticks() < t1 + 3 {}
        per_ms = (cycles().wrapping_sub(c0) / 30).max(1);
        CYCLES_PER_MS.store(per_ms, Ordering::Relaxed);
    }
    (u128::from(cycles()) * 1000 / u128::from(per_ms)) as u64
}

/// Mouse events waiting for the display owner (at most `buf.len()`).
pub fn mouse_read(buf: &mut [zoo_abi::MouseEvent]) -> Result<usize, Error> {
    result(syscall(
        sys::MOUSE_READ,
        buf.as_mut_ptr() as u64,
        buf.len() as u64,
        0,
    ))
    .map(|n| n as usize)
}

/// Blocks until keyboard or mouse input arrives, another wake-up (a
/// message, a process exit) or `ticks` timer ticks pass; may return early.
/// Display owner only.
pub fn input_wait(ticks: u64) -> Result<(), Error> {
    result(syscall(sys::INPUT_WAIT, ticks, 0, 0)).map(|_| ())
}

/// Shows rectangle (x, y, w, h) of display memory, on a display that
/// needs it (`zoo_abi::DISPLAY_FLUSH_NEEDED`).
pub fn display_flush(x: usize, y: usize, w: usize, h: usize) -> Result<(), Error> {
    let r = x as u64 | (y as u64) << 16 | (w as u64) << 32 | (h as u64) << 48;
    result(syscall(sys::DISPLAY_FLUSH, r, 0, 0)).map(|_| ())
}

/// Sets the hardware cursor (`zoo_abi::DISPLAY_HW_CURSOR`): an image of
/// `CURSOR_SIZE`² 0xAARRGGBB pixels, its hot spot and its position.
pub fn cursor_set(image: &[u32], hot: (usize, usize), at: (usize, usize)) -> Result<(), Error> {
    if image.len() < zoo_abi::CURSOR_SIZE * zoo_abi::CURSOR_SIZE {
        return Err(Error::Inval);
    }
    let pack = |(x, y): (usize, usize)| x as u64 | (y as u64) << 16;
    result(syscall(
        sys::CURSOR_SET,
        image.as_ptr() as u64,
        pack(hot),
        pack(at),
    ))
    .map(|_| ())
}

/// Moves the hardware cursor.
pub fn cursor_move(x: usize, y: usize) -> Result<(), Error> {
    result(syscall(sys::CURSOR_MOVE, x as u64, y as u64, 0)).map(|_| ())
}

/// [events, total wait, longest wait]: TSC cycles from a mouse event's
/// arrival to `mouse_read`, since the last call (display owner only).
pub fn input_latency() -> Result<[u64; 3], Error> {
    let mut v = [0u64; 3];
    result(syscall(sys::INPUT_LATENCY, v.as_mut_ptr() as u64, 0, 0))?;
    Ok(v)
}

/// A block-device handle for the data disk (the resident supervisor only).
pub fn block_attach() -> Result<Handle, Error> {
    result(syscall(sys::BLOCK_ATTACH, 0, 0, 0))
}

/// Reads 4 KiB block `block` into `buf`.
pub fn block_read(
    disk: Handle,
    block: u64,
    buf: &mut [u8; zoo_abi::BLOCK_BYTES],
) -> Result<(), Error> {
    result(syscall(
        sys::BLOCK_READ,
        disk,
        block,
        buf.as_mut_ptr() as u64,
    ))
    .map(|_| ())
}

/// Writes `buf` to 4 KiB block `block`.
pub fn block_write(
    disk: Handle,
    block: u64,
    buf: &[u8; zoo_abi::BLOCK_BYTES],
) -> Result<(), Error> {
    result(syscall(sys::BLOCK_WRITE, disk, block, buf.as_ptr() as u64)).map(|_| ())
}

/// Reads `buf.len() / 4096` consecutive blocks from `block` in one request
/// (at most `zoo_abi::BLOCK_RUN_MAX`; `buf.len()` a multiple of 4 KiB).
pub fn block_read_run(disk: Handle, block: u64, buf: &mut [u8]) -> Result<(), Error> {
    let count = (buf.len() / zoo_abi::BLOCK_BYTES) as u64;
    result(syscall(
        sys::BLOCK_READ_RUN,
        disk,
        block | (count << 48),
        buf.as_mut_ptr() as u64,
    ))
    .map(|_| ())
}

/// Writes `buf.len() / 4096` consecutive blocks from `block` in one request.
pub fn block_write_run(disk: Handle, block: u64, buf: &[u8]) -> Result<(), Error> {
    let count = (buf.len() / zoo_abi::BLOCK_BYTES) as u64;
    result(syscall(
        sys::BLOCK_WRITE_RUN,
        disk,
        block | (count << 48),
        buf.as_ptr() as u64,
    ))
    .map(|_| ())
}

/// Size of the data disk in bytes.
pub fn block_size(disk: Handle) -> Result<u64, Error> {
    result(syscall(sys::BLOCK_SIZE, disk, 0, 0))
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
