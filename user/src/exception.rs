//! User exception handling (docs/posix3-design.md): a process-wide handler
//! that sees the fault and the interrupted registers, and either resumes
//! (possibly after editing them) or declares the fault unhandled.

use crate::{Error, result, sys, syscall};
use core::sync::atomic::{AtomicUsize, Ordering};
pub use zoo_abi::{EXC_RESUME, EXC_UNHANDLED, ExceptionContext};

/// The handler: return true to resume from the (edited) context, false to
/// let the process crash as if there were no handler.
pub type Handler = fn(&mut ExceptionContext) -> bool;

static HANDLER: AtomicUsize = AtomicUsize::new(0);

extern "C" fn trampoline(ctx: *mut ExceptionContext) -> ! {
    let h = HANDLER.load(Ordering::Acquire);
    // SAFETY: the kernel passes a pointer to a context it just wrote on
    // this thread's exception stack; nothing else refers to it.
    let ctx = unsafe { &mut *ctx };
    let resume = if h == 0 {
        false
    } else {
        // SAFETY: only `set_handler` stores into HANDLER, always a `Handler`.
        let f: Handler = unsafe { core::mem::transmute(h) };
        f(ctx)
    };
    let how = if resume { EXC_RESUME } else { EXC_UNHANDLED };
    syscall(
        sys::EXCEPTION_RETURN,
        ctx as *mut ExceptionContext as u64,
        how,
        0,
    );
    // Only reached if the kernel refused the context (e.g. a bad `rip`).
    crate::exit(103)
}

/// Installs `h` as the process's exception handler.
pub fn set_handler(h: Handler) -> Result<(), Error> {
    HANDLER.store(h as usize, Ordering::Release);
    let entry = trampoline as extern "C" fn(*mut ExceptionContext) -> ! as usize as u64;
    result(syscall(sys::SET_EXCEPTION_HANDLER, entry, 0, 0)).map(|_| ())
}

/// Gives the calling thread its own exception stack of `size` bytes from
/// the heap (kept for the life of the process), so stack faults can be
/// handled too.
pub fn use_own_stack(size: usize) -> Result<(), Error> {
    let stack = alloc::vec![0u8; size].leak();
    let top = (stack.as_ptr() as u64 + size as u64) & !15;
    result(syscall(sys::SET_EXCEPTION_STACK, top, 0, 0)).map(|_| ())
}

/// Calls `exception_return` directly (for tests of the kernel's checks).
pub fn raw_return(ctx: &ExceptionContext, how: u64) -> Result<(), Error> {
    result(syscall(
        sys::EXCEPTION_RETURN,
        ctx as *const ExceptionContext as u64,
        how,
        0,
    ))
    .map(|_| ())
}
