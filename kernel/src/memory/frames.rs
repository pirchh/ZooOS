//! Bitmap physical frame allocator (docs/m2-design.md §4).
//!
//! Two bitmaps, one bit per 4 KiB frame: `managed` (the allocator owns this
//! frame) and `free` (it is currently available). Managed frames are those
//! wholly inside `Usable` regions, at or above 1 MiB, minus the frames holding
//! the bitmaps themselves.

use bootloader_api::info::{MemoryRegion, MemoryRegionKind};
use x86_64::PhysAddr;
use x86_64::structures::paging::{FrameAllocator, PhysFrame, Size4KiB};

pub const FRAME_SIZE: u64 = 4096;
/// Frames below this physical address are never handed out.
pub const LOW_MEMORY_LIMIT: u64 = 0x10_0000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameError {
    /// The frame is not owned by this allocator.
    NotManaged,
    /// The frame is already free.
    DoubleFree,
}

pub struct BitmapFrameAllocator {
    managed: &'static mut [u64],
    free: &'static mut [u64],
    frame_count: usize,
    total: usize,
    available: usize,
    hint: usize,
}

fn usable_frames(region: &MemoryRegion) -> Option<(u64, u64)> {
    if region.kind != MemoryRegionKind::Usable {
        return None;
    }
    let start = region.start.max(LOW_MEMORY_LIMIT).div_ceil(FRAME_SIZE);
    let end = (region.end / FRAME_SIZE).max(start); // round end down
    (end > start).then_some((start, end))
}

impl BitmapFrameAllocator {
    /// Builds the allocator, storing its bitmaps in the first usable frames
    /// large enough to hold them.
    ///
    /// # Safety
    /// `phys_offset` must be the start of a writable mapping of all physical
    /// memory, `Usable` regions must be unused by anything else, and this
    /// must be called only once (it takes ownership of those frames).
    pub unsafe fn new(regions: &[MemoryRegion], phys_offset: u64) -> Result<Self, &'static str> {
        let frame_count = regions
            .iter()
            .filter_map(usable_frames)
            .map(|(_, end)| end)
            .max()
            .ok_or("no usable memory above 1 MiB")? as usize;
        let words = frame_count.div_ceil(64);
        let bitmap_bytes = (words * 8 * 2) as u64;
        let bitmap_frames = bitmap_bytes.div_ceil(FRAME_SIZE);

        let (store_start, _) = regions
            .iter()
            .filter_map(usable_frames)
            .find(|(s, e)| e - s >= bitmap_frames)
            .ok_or("no usable region large enough for the frame bitmap")?;

        let base = (phys_offset + store_start * FRAME_SIZE) as *mut u64;
        // SAFETY: per the caller's contract the frames at `store_start` are
        // unused, usable and mapped writable at `phys_offset`; they are large
        // enough for `2 * words` u64s (checked above) and suitably aligned
        // (frame aligned). Both slices are disjoint and exclusively ours.
        let (managed, free) = unsafe {
            core::ptr::write_bytes(base, 0, 2 * words);
            (
                core::slice::from_raw_parts_mut(base, words),
                core::slice::from_raw_parts_mut(base.add(words), words),
            )
        };

        let mut this = BitmapFrameAllocator {
            managed,
            free,
            frame_count,
            total: 0,
            available: 0,
            hint: 0,
        };
        for (start, end) in regions.iter().filter_map(usable_frames) {
            for f in start..end {
                this.set_bit(f as usize, true);
            }
        }
        for f in store_start..store_start + bitmap_frames {
            this.set_bit(f as usize, false);
        }
        this.total = this.managed.iter().map(|w| w.count_ones() as usize).sum();
        this.available = this.total;
        Ok(this)
    }

    fn set_bit(&mut self, frame: usize, value: bool) {
        let (w, b) = (frame / 64, frame % 64);
        if value {
            self.managed[w] |= 1 << b;
            self.free[w] |= 1 << b;
        } else {
            self.managed[w] &= !(1 << b);
            self.free[w] &= !(1 << b);
        }
    }

    pub fn total(&self) -> usize {
        self.total
    }

    pub fn available(&self) -> usize {
        self.available
    }

    pub fn allocate(&mut self) -> Option<PhysFrame> {
        let words = self.free.len();
        for i in 0..words {
            let w = (self.hint + i) % words;
            let word = self.free[w];
            if word != 0 {
                let bit = word.trailing_zeros() as usize;
                self.free[w] &= !(1 << bit);
                self.available -= 1;
                self.hint = w;
                let addr = ((w * 64 + bit) as u64) * FRAME_SIZE;
                return Some(PhysFrame::containing_address(PhysAddr::new(addr)));
            }
        }
        None
    }

    pub fn deallocate(&mut self, frame: PhysFrame) -> Result<(), FrameError> {
        let index = (frame.start_address().as_u64() / FRAME_SIZE) as usize;
        if index >= self.frame_count {
            return Err(FrameError::NotManaged);
        }
        let (w, b) = (index / 64, index % 64);
        if self.managed[w] & (1 << b) == 0 {
            return Err(FrameError::NotManaged);
        }
        if self.free[w] & (1 << b) != 0 {
            return Err(FrameError::DoubleFree);
        }
        self.free[w] |= 1 << b;
        self.available += 1;
        Ok(())
    }
}

// SAFETY: `allocate` only returns frames that are managed, were free, and are
// now marked used, so a frame is never handed out twice while in use.
unsafe impl FrameAllocator<Size4KiB> for BitmapFrameAllocator {
    fn allocate_frame(&mut self) -> Option<PhysFrame<Size4KiB>> {
        self.allocate()
    }
}
