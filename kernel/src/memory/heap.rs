//! Kernel heap: a fixed-size linked-list allocator (docs/m2-design.md §6).

use linked_list_allocator::LockedHeap;

#[global_allocator]
static ALLOCATOR: LockedHeap = LockedHeap::empty();

/// # Safety
/// `[start, start + size)` must be mapped writable, unused by anything else,
/// and stay mapped for the rest of the kernel's life. Call once.
pub unsafe fn init(start: u64, size: usize) {
    // SAFETY: forwarded from the caller's contract.
    unsafe { ALLOCATOR.lock().init(start as *mut u8, size) }
}

/// Heap a program's request may not use: kept for the kernel's own needs
/// (starting and ending tasks, messages it sends itself), so a program
/// that fills the heap gets `NOMEM` instead of panicking the kernel
/// (docs/heap-design.md).
pub const RESERVE: usize = 1024 * 1024;

/// Whether a program's request must be refused: less than `RESERVE` (and
/// `want` more) bytes are left.
pub fn low(want: usize) -> bool {
    let h = ALLOCATOR.lock();
    h.free() < RESERVE.saturating_add(want)
}

/// Bytes currently allocated.
pub fn used() -> usize {
    ALLOCATOR.lock().used()
}
