//! Threads, futexes and thread-local storage for ZooOS user programs
//! (docs/posix1-design.md §3).
//!
//! `spawn` runs a closure on a new thread with its own 64 KiB stack from the
//! heap. Stacks are not freed when a thread ends (the heap is per process
//! and goes away with it); a program that creates many short-lived threads
//! should reuse them.

use crate::{Error, Handle, result, sys, syscall};
use alloc::boxed::Box;
use core::cell::UnsafeCell;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicU32, Ordering};

pub const STACK_SIZE: usize = 64 * 1024;

/// Ends the calling thread; the process ends with it if it was the last.
pub fn exit(code: u8) -> ! {
    syscall(sys::THREAD_EXIT, code as u64, 0, 0);
    loop {
        core::hint::spin_loop();
    }
}

/// Waits while `*word == expected`, until `wake` or `timeout` ticks
/// (0: no timeout). May return early; callers re-check their condition.
pub fn futex_wait(word: &AtomicU32, expected: u32, timeout: u64) -> Result<(), Error> {
    result(syscall(
        sys::FUTEX_WAIT,
        word.as_ptr() as u64,
        expected as u64,
        timeout,
    ))
    .map(|_| ())
}

/// Wakes up to `n` threads of this process waiting on `word`.
pub fn futex_wake(word: &AtomicU32, n: u64) -> u64 {
    result(syscall(sys::FUTEX_WAKE, word.as_ptr() as u64, n, 0)).unwrap_or(0)
}

/// Sets this thread's FS base (thread-local storage pointer).
pub fn set_fs_base(base: u64) -> Result<(), Error> {
    result(syscall(sys::SET_FS_BASE, base, 0, 0)).map(|_| ())
}

/// Reads the 8 bytes at FS:`offset` (thread-local storage).
pub fn fs_read_u64<const OFFSET: u64>() -> u64 {
    let v: u64;
    // SAFETY: reads through the FS segment base this thread set with
    // `set_fs_base`; callers only use offsets inside their TLS block.
    unsafe {
        core::arch::asm!(
            "mov {v}, qword ptr fs:[{off}]",
            v = out(reg) v,
            off = const OFFSET,
            options(nostack, readonly, preserves_flags),
        );
    }
    v
}

extern "C" fn trampoline(arg: u64) -> ! {
    // SAFETY: `arg` is the `Box<Box<dyn FnOnce>>` leaked by `spawn` for
    // exactly this thread, which takes it back once.
    let f = unsafe { Box::from_raw(arg as *mut Box<dyn FnOnce() -> u8 + Send>) };
    let code = f();
    exit(code)
}

/// Runs `f` on a new thread. Returns a handle for `crate::wait`, which
/// yields the closure's return value as the exit code.
pub fn spawn<F: FnOnce() -> u8 + Send + 'static>(f: F) -> Result<Handle, Error> {
    let stack: Box<[u8]> = alloc::vec![0u8; STACK_SIZE].into_boxed_slice();
    let top = (stack.as_ptr() as u64 + STACK_SIZE as u64) & !15;
    let boxed: Box<Box<dyn FnOnce() -> u8 + Send>> = Box::new(Box::new(f));
    let arg = Box::into_raw(boxed) as u64;
    let entry = trampoline as extern "C" fn(u64) -> ! as usize as u64;
    match result(syscall(sys::THREAD_CREATE, entry, top, arg)) {
        Ok(h) => {
            // The stack lives as long as the process (see module docs).
            Box::leak(stack);
            Ok(h)
        }
        Err(e) => {
            // SAFETY: the thread was not created, so `arg` is still ours.
            drop(unsafe { Box::from_raw(arg as *mut Box<dyn FnOnce() -> u8 + Send>) });
            Err(e)
        }
    }
}

/// A futex-based mutex: 0 unlocked, 1 locked, 2 locked with waiters.
pub struct Mutex<T> {
    state: AtomicU32,
    /// Times `lock` found the mutex taken (and so used the futex path).
    contended: AtomicU32,
    value: UnsafeCell<T>,
}

// SAFETY: access to `value` is serialised by `state` (acquire on lock,
// release on unlock).
unsafe impl<T: Send> Sync for Mutex<T> {}
unsafe impl<T: Send> Send for Mutex<T> {}

pub struct Guard<'a, T> {
    m: &'a Mutex<T>,
}

impl<T> Mutex<T> {
    pub const fn new(value: T) -> Self {
        Mutex {
            state: AtomicU32::new(0),
            contended: AtomicU32::new(0),
            value: UnsafeCell::new(value),
        }
    }

    pub fn lock(&self) -> Guard<'_, T> {
        if self
            .state
            .compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            // Mark contended and sleep until we take it.
            self.contended.fetch_add(1, Ordering::Relaxed);
            while self.state.swap(2, Ordering::Acquire) != 0 {
                let _ = futex_wait(&self.state, 2, 0);
            }
        }
        Guard { m: self }
    }

    /// How often `lock` had to wait (for tests and diagnostics).
    pub fn contended(&self) -> u32 {
        self.contended.load(Ordering::Relaxed)
    }
}

impl<T> Drop for Guard<'_, T> {
    fn drop(&mut self) {
        if self.m.state.swap(0, Ordering::Release) == 2 {
            futex_wake(&self.m.state, 1);
        }
    }
}

impl<T> Deref for Guard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        // SAFETY: the guard proves the lock is held.
        unsafe { &*self.m.value.get() }
    }
}

impl<T> DerefMut for Guard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: the guard proves the lock is held, exclusively.
        unsafe { &mut *self.m.value.get() }
    }
}
