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

    pub fn zeroed_frame(&mut self) -> Result<PhysFrame, SpaceError> {
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

    /// Maps an existing frame (not owned by this mapping, e.g. the
    /// framebuffer or a shared tmpfs page) at user address `va` in `p4`:
    /// user-accessible, no-execute unless `executable`.
    pub fn map_user_frame(
        &mut self,
        p4: PhysFrame,
        va: u64,
        frame: PhysFrame,
        writable: bool,
        executable: bool,
    ) -> Result<(), SpaceError> {
        debug_assert!(va < USER_END);
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
        // SAFETY: `p4` is a live process table; `frame` is device memory the
        // caller hands out deliberately (it is not in the allocator, so it is
        // never freed through this mapping; see `unmap_user_keep`).
        let result = unsafe {
            let mut space = self.space(p4);
            space.map_to_with_table_flags(page, frame, flags, parents, &mut self.frames)
        };
        match result {
            Ok(flush) => {
                flush.flush();
                Ok(())
            }
            Err(_) => Err(SpaceError::AlreadyMapped),
        }
    }

    /// Unmaps `pages` user pages from `va` in `p4` without freeing their
    /// frames (for memory the allocator does not own, like the framebuffer).
    pub fn unmap_user_keep(&mut self, p4: PhysFrame, va: u64, pages: u64) {
        // SAFETY: `p4` is a live process table that is not active.
        let mut space = unsafe { self.space(p4) };
        for i in 0..pages {
            let page = Page::<Size4KiB>::containing_address(VirtAddr::new(va + i * 4096));
            if let Ok((_, flush)) = space.unmap(page) {
                flush.ignore();
            }
        }
    }

    /// Unmaps `pages` user pages from `va` in `p4` and frees their frames
    /// (allocator-owned memory, e.g. a heap extension being rolled back).
    pub fn unmap_user_free(&mut self, p4: PhysFrame, va: u64, pages: u64) {
        for i in 0..pages {
            let page = Page::<Size4KiB>::containing_address(VirtAddr::new(va + i * 4096));
            // SAFETY: `p4` is a live process table; a temporary mapper is
            // created per page so it does not overlap the frame allocator
            // borrow below.
            let unmapped = unsafe { self.space(p4).unmap(page) };
            if let Ok((frame, flush)) = unmapped {
                flush.flush();
                let _ = self.frames.deallocate(frame);
            }
        }
    }

    /// The first mapped user page in `[from, end)` of `p4`, skipping
    /// unused page-table subtrees (docs/posix2-design.md §3).
    pub fn next_mapped(&self, p4: PhysFrame, from: u64, end: u64) -> Option<u64> {
        const L1: u64 = 4096;
        const L2: u64 = L1 * 512;
        const L3: u64 = L2 * 512;
        const L4: u64 = L3 * 512;
        let end = end.min(USER_END);
        let mut va = from & !(L1 - 1);
        // SAFETY: `p4` is a live process table; the tables are only read.
        unsafe {
            let t4 = self.table(p4);
            while va < end {
                let e4 = &t4[((va / L4) % 512) as usize];
                let Ok(f3) = e4.frame() else {
                    va = (va / L4 + 1) * L4;
                    continue;
                };
                let e3 = &self.table(f3)[((va / L3) % 512) as usize];
                let Ok(f2) = e3.frame() else {
                    va = (va / L3 + 1) * L3;
                    continue;
                };
                let e2 = &self.table(f2)[((va / L2) % 512) as usize];
                let Ok(f1) = e2.frame() else {
                    va = (va / L2 + 1) * L2;
                    continue;
                };
                let e1 = &self.table(f1)[((va / L1) % 512) as usize];
                if e1.flags().contains(PageTableFlags::PRESENT) {
                    return Some(va);
                }
                va += L1;
            }
        }
        None
    }

    /// (writable, executable) of the mapped user page at `va`, if any.
    pub fn user_page_access(&self, p4: PhysFrame, va: u64) -> Option<(bool, bool)> {
        // SAFETY: `p4` is a live process table; the mapper is only read.
        let space = unsafe { self.space(p4) };
        match space.translate(VirtAddr::new(va)) {
            TranslateResult::Mapped { flags, .. } => Some((
                flags.contains(PageTableFlags::WRITABLE),
                !flags.contains(PageTableFlags::NO_EXECUTE),
            )),
            _ => None,
        }
    }

    /// Changes the access of the mapped user page at `va`, flushing it from
    /// this CPU's TLB (the space may be the active one).
    pub fn protect_user_page(
        &mut self,
        p4: PhysFrame,
        va: u64,
        writable: bool,
        executable: bool,
    ) -> Result<(), SpaceError> {
        let mut flags = PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE;
        if writable {
            flags |= PageTableFlags::WRITABLE;
        }
        if !executable {
            flags |= PageTableFlags::NO_EXECUTE;
        }
        let page = Page::<Size4KiB>::containing_address(VirtAddr::new(va));
        // SAFETY: `p4` is a live process table; only the leaf's flags change
        // and the frame stays the same.
        let r = unsafe { self.space(p4).update_flags(page, flags) };
        match r {
            Ok(flush) => {
                flush.flush();
                Ok(())
            }
            Err(_) => Err(SpaceError::AlreadyMapped),
        }
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

    /// A new address space with a private copy of every user page of `src`
    /// at the same address and with the same access (Linux `fork`,
    /// docs/linux5-design.md). Every user page of `src` must be
    /// allocator-owned (no device, ramdisk or shared mappings). On failure
    /// nothing is left allocated.
    pub fn copy_address_space(&mut self, src: PhysFrame) -> Result<PhysFrame, SpaceError> {
        let dst = self.new_address_space()?;
        let parents =
            PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::USER_ACCESSIBLE;
        // The leaves to copy: (address, frame, flags), gathered first so the
        // source tables are only read while the destination is built.
        let mut leaves = alloc::vec::Vec::new();
        // SAFETY: `src` is a live process table; its tables are only read.
        unsafe {
            let l4 = self.table(src);
            for (i4, e4) in l4.iter().enumerate().take(UPPER_HALF_FIRST_P4) {
                let Ok(t3) = e4.frame() else { continue };
                for (i3, e3) in self.table(t3).iter().enumerate() {
                    let Ok(t2) = e3.frame() else { continue };
                    for (i2, e2) in self.table(t2).iter().enumerate() {
                        let Ok(t1) = e2.frame() else { continue };
                        for (i1, e1) in self.table(t1).iter().enumerate() {
                            if !e1.flags().contains(PageTableFlags::PRESENT) {
                                continue;
                            }
                            let va = ((i4 as u64) << 39)
                                | ((i3 as u64) << 30)
                                | ((i2 as u64) << 21)
                                | ((i1 as u64) << 12);
                            let leaf = e1.frame().expect("user leaf is a 4 KiB frame");
                            leaves.push((va, leaf, e1.flags()));
                        }
                    }
                }
            }
        }
        for (va, from, flags) in leaves {
            let Some(frame) = self.frames.allocate() else {
                self.free_address_space(dst);
                return Err(SpaceError::OutOfMemory);
            };
            // SAFETY: `frame` was just allocated and `from` is a mapped user
            // page of `src`; both are whole pages in the physical window and
            // distinct.
            unsafe {
                core::ptr::copy_nonoverlapping(
                    self.phys_to_virt(from.start_address()),
                    self.phys_to_virt(frame.start_address()),
                    4096,
                )
            };
            let page = Page::<Size4KiB>::containing_address(VirtAddr::new(va));
            // SAFETY: `dst` is the fresh space built here; `frame` is new.
            let r = unsafe {
                self.space(dst).map_to_with_table_flags(
                    page,
                    frame,
                    flags,
                    parents,
                    &mut self.frames,
                )
            };
            match r {
                // `dst` is not active, so there is nothing to flush.
                Ok(flush) => flush.ignore(),
                Err(_) => {
                    let _ = self.frames.deallocate(frame);
                    self.free_address_space(dst);
                    return Err(SpaceError::OutOfMemory);
                }
            }
        }
        Ok(dst)
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
