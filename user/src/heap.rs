//! The user heap (docs/m5a-design.md §2): a linked-list allocator that
//! starts with 1 MiB from `mem_alloc` and extends itself with further
//! `mem_alloc` calls, which the kernel maps contiguously.

use core::alloc::{GlobalAlloc, Layout};
use core::ptr::{NonNull, null_mut};
use core::sync::atomic::{AtomicUsize, Ordering};
use linked_list_allocator::LockedHeap;

const FIRST_CHUNK: u64 = 1 << 20;

struct GrowingHeap(LockedHeap);

#[global_allocator]
static HEAP: GrowingHeap = GrowingHeap(LockedHeap::empty());

/// Most bytes in use at once since the program started
/// (docs/stage1-design.md).
static PEAK: AtomicUsize = AtomicUsize::new(0);

/// Heap bytes in use now, the most ever in use, and the heap's size.
pub fn stats() -> (usize, usize, usize) {
    let heap = HEAP.0.lock();
    (heap.used(), PEAK.load(Ordering::Relaxed), heap.size())
}

// SAFETY: all state is behind the allocator's lock; memory comes only from
// `mem_alloc`, which returns fresh, exclusively owned, writable pages.
unsafe impl GlobalAlloc for GrowingHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let mut heap = self.0.lock();
        if heap.size() == 0 {
            let Ok(addr) = crate::mem_alloc(FIRST_CHUNK) else {
                return null_mut();
            };
            // SAFETY: `addr` is a fresh 1 MiB mapping owned by this heap.
            unsafe { heap.init(addr as *mut u8, FIRST_CHUNK as usize) };
        }
        loop {
            if let Ok(p) = heap.allocate_first_fit(layout) {
                PEAK.fetch_max(heap.used(), Ordering::Relaxed);
                return p.as_ptr();
            }
            // Grow by at least the request plus alignment slack.
            let want = (layout.size() + layout.align()) as u64;
            let grow = want.max(FIRST_CHUNK).div_ceil(4096) * 4096;
            let Ok(addr) = crate::mem_alloc(grow) else {
                return null_mut();
            };
            if addr != heap.top() as u64 {
                // Not contiguous (should not happen: only this allocator
                // calls mem_alloc). Fail rather than corrupt the heap.
                return null_mut();
            }
            // SAFETY: the new pages start exactly at the heap's top and are
            // owned by this heap.
            unsafe { heap.extend(grow as usize) };
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if let Some(p) = NonNull::new(ptr) {
            // SAFETY: `ptr` was returned by `alloc` with this `layout`.
            unsafe { self.0.lock().deallocate(p, layout) };
        }
    }
}
