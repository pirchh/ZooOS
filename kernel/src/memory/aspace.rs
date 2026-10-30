//! Per-process address spaces (docs/m3-design.md §1).
//!
//! A process's level-4 table shares entries 256..512 with the kernel's (the
//! kernel is mapped, supervisor-only, everywhere) and owns entries 0..256,
//! whose tables and pages are all allocated here and freed by
//! `free_address_space`.

use super::{KSTACK_REGION, KSTACK_SIZE, KSTACK_SLOT, Memory, UPPER_HALF_FIRST_P4, USER_END};
use x86_64::VirtAddr;
use x86_64::structures::paging::{
    Mapper, OffsetPageTable, Page, PageTable, PageTableFlags, PhysFrame, Size4KiB, Translate,
    mapper::TranslateResult,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpaceError {
    OutOfMemory,
    AlreadyMapped,
}

impl Memory {
    /// A page-table frame viewed through the physical-memory window.
    ///
    /// # Safety
    /// `frame` must hold a page table owned by the caller, with no other
    /// live reference to it for the returned lifetime.
    unsafe fn table<'a>(&self, frame: PhysFrame) -> &'a mut PageTable {
        // SAFETY: forwarded from the caller; the window maps all memory.
        unsafe { &mut *(self.phys_to_virt(frame.start_address()) as *mut PageTable) }
    }

    /// A mapper for a process's (non-active or active) level-4 table.
    ///
    /// # Safety
    /// `p4` must be a process level-4 table created by `new_address_space`
    /// and not yet freed, with no other mapper for it alive.
    unsafe fn space<'a>(&self, p4: PhysFrame) -> OffsetPageTable<'a> {
        // SAFETY: forwarded from the caller.
        unsafe { OffsetPageTable::new(self.table(p4), VirtAddr::new(self.phys_offset)) }
    }

    fn zeroed_frame(&mut self) -> Result<PhysFrame, SpaceError> {
        let f = self.frames.allocate().ok_or(SpaceError::OutOfMemory)?;
        // SAFETY: the frame was just allocated, so nothing else uses it; the
        // window maps it writable.
        unsafe { core::ptr::write_bytes(self.phys_to_virt(f.start_address()), 0, 4096) };
        Ok(f)
    }

    /// A new level-4 table: empty user half, kernel half copied.
    pub fn new_address_space(&mut self) -> Result<PhysFrame, SpaceError> {
        let p4 = self.zeroed_frame()?;
        // SAFETY: `p4` is a fresh frame owned here; the kernel table is only
        // read, through the kernel mapper's own reference.
        let new = unsafe { self.table(p4) };
        let kernel = self.mapper.level_4_table();
        for i in UPPER_HALF_FIRST_P4..512 {
            new[i] = kernel[i].clone();
        }
        Ok(p4)
    }

    /// Maps a fresh zeroed frame at user address `va` in `p4`. `writable`
    /// and `executable` choose the leaf flags; it is always user-accessible.
    pub fn map_user_page(
        &mut self,
        p4: PhysFrame,
        va: u64,
        writable: bool,
        executable: bool,
    ) -> Result<PhysFrame, SpaceError> {
        debug_assert!(va < USER_END);
        let frame = self.zeroed_frame()?;
        let mut flags = PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE;
        if writable {
            flags |= PageTableFlags::WRITABLE;
        }
        if !executable {
            flags |= PageTableFlags::NO_EXECUTE;
        }
        let parents =
            PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::USER_ACCESSIBLE;
        let page = Page::<Size4KiB>::containing_address(VirtAddr::new(va));
        // SAFETY: `p4` is a live process table (caller contract of this
        // module); the frame is fresh; `va` is in the user half, which only
        // this module maps.
        let result = unsafe {
            let mut space = self.space(p4);
            space.map_to_with_table_flags(page, frame, flags, parents, &mut self.frames)
        };
        match result {
            // Not necessarily the active space; a stale TLB entry for a page
            // that was never mapped cannot exist.
            Ok(flush) => {
                flush.ignore();
                Ok(frame)
            }
            Err(_) => {
                let _ = self.frames.deallocate(frame);
                Err(SpaceError::AlreadyMapped)
            }
        }
    }

    /// True if `[addr, addr+len)` lies in the user half and every page is
    /// mapped user-accessible in `p4` (and writable, if `write`).
    pub fn user_range_ok(&self, p4: PhysFrame, addr: u64, len: u64, write: bool) -> bool {
        if len == 0 {
            return addr < USER_END;
        }
        let Some(end) = addr.checked_add(len) else {
            return false;
        };
        if end > USER_END {
            return false;
        }
        // SAFETY: `p4` is a live process table; the mapper is only read.
        let space = unsafe { self.space(p4) };
        let mut page = addr & !0xFFF;
        while page < end {
            match space.translate(VirtAddr::new(page)) {
                TranslateResult::Mapped { flags, .. }
                    if flags.contains(PageTableFlags::USER_ACCESSIBLE)
                        && (!write || flags.contains(PageTableFlags::WRITABLE)) => {}
                _ => return false,
            }
            page += 4096;
        }
        true
    }

    /// True if `p4`'s kernel half is identical to the kernel's table.
    pub fn shares_kernel_half(&self, p4: PhysFrame) -> bool {
        // SAFETY: `p4` is a live process table; both tables are only read.
        let t = unsafe { self.table(p4) };
        let k = self.mapper.level_4_table();
        (UPPER_HALF_FIRST_P4..512)
            .all(|i| t[i].addr() == k[i].addr() && t[i].flags() == k[i].flags())
    }

    /// Physical frame mapped at `va` in `p4`, if any (used by self-tests).
    pub fn user_translate(&self, p4: PhysFrame, va: u64) -> Option<PhysFrame> {
        // SAFETY: `p4` is a live process table; the mapper is only read.
        let space = unsafe { self.space(p4) };
        match space.translate(VirtAddr::new(va)) {
            TranslateResult::Mapped { frame, .. } => {
                Some(PhysFrame::containing_address(frame.start_address()))
            }
            _ => None,
        }
    }

    /// Frees every user page and page table under `p4`, then `p4` itself.
    /// The space must not be active (CR3) on this CPU.
    pub fn free_address_space(&mut self, p4: PhysFrame) {
        let release = |m: &mut Memory, f: PhysFrame| {
            m.frames
                .deallocate(f)
                .unwrap_or_else(|e| panic!("freeing frame {:#x}: {e:?}", f.start_address()));
        };
        // SAFETY: the tables below were all created by this module for this
        // space, which is no longer active; nothing else references them.
        unsafe {
            let l4 = self.table(p4);
            for e4 in l4
                .iter()
                .take(UPPER_HALF_FIRST_P4)
                .filter(|e| !e.is_unused())
            {
                let t3 = e4.frame().expect("user level-4 entry is a table");
                for e3 in self.table(t3).iter().filter(|e| !e.is_unused()) {
                    let t2 = e3.frame().expect("user level-3 entry is a table");
                    for e2 in self.table(t2).iter().filter(|e| !e.is_unused()) {
                        let t1 = e2.frame().expect("user level-2 entry is a table");
                        for e1 in self.table(t1).iter().filter(|e| !e.is_unused()) {
                            let leaf = e1.frame().expect("user leaf is a 4 KiB frame");
                            release(self, leaf);
                        }
                        release(self, t1);
                    }
                    release(self, t2);
                }
                release(self, t3);
            }
        }
        release(self, p4);
    }

    /// Maps kernel-stack slot `slot` (guard page left unmapped). Returns the
    /// stack top.
    pub fn map_kernel_stack(&mut self, slot: usize) -> Result<u64, SpaceError> {
        let base = KSTACK_REGION + slot as u64 * KSTACK_SLOT + 4096;
        let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::NO_EXECUTE;
        let mut va = base;
        while va < base + KSTACK_SIZE {
            if self.map_new(va, flags).is_err() {
                // Undo the pages mapped so far.
                let mut undo = base;
                while undo < va {
                    if let Ok(f) = self.unmap(undo) {
                        let _ = self.frames.deallocate(f);
                    }
                    undo += 4096;
                }
                return Err(SpaceError::OutOfMemory);
            }
            va += 4096;
        }
        Ok(base + KSTACK_SIZE)
    }

    /// Unmaps and frees kernel-stack slot `slot`. Must not be the stack in use.
    pub fn free_kernel_stack(&mut self, slot: usize) {
        let base = KSTACK_REGION + slot as u64 * KSTACK_SLOT + 4096;
        let mut va = base;
        while va < base + KSTACK_SIZE {
            let f = self
                .unmap(va)
                .unwrap_or_else(|e| panic!("kernel stack slot {slot} page {va:#x}: {e}"));
            self.frames
                .deallocate(f)
                .unwrap_or_else(|e| panic!("freeing kernel stack frame: {e:?}"));
            va += 4096;
        }
    }
}
