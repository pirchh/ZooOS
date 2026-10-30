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

/// Bytes currently allocated.
pub fn used() -> usize {
    ALLOCATOR.lock().used()
}
