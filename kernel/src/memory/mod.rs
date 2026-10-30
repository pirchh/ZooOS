//! Memory ownership, page mapping and the kernel heap (docs/m2-design.md).

pub mod frames;
pub mod heap;

use bootloader_api::BootInfo;
use frames::BitmapFrameAllocator;
use x86_64::registers::control::Cr3;
use x86_64::structures::paging::{
    Mapper, OffsetPageTable, Page, PageTable, PageTableFlags, PhysFrame, Size4KiB, Translate,
};
use x86_64::{PhysAddr, VirtAddr};

/// First level-4 index of the kernel-owned upper half.
pub const KERNEL_HALF_FIRST_P4: usize = 256;

/// Kernel virtual layout (design §3). The first page is the heap's lower
/// guard; the page after the heap is its upper guard. Neither is mapped.
pub const HEAP_REGION: u64 = 0xFFFF_C000_0000_0000;
pub const HEAP_START: u64 = HEAP_REGION + 0x1000;
pub const HEAP_SIZE: u64 = 1024 * 1024;
pub const HEAP_GUARD_LOW: u64 = HEAP_REGION;
pub const HEAP_GUARD_HIGH: u64 = HEAP_START + HEAP_SIZE;
/// Scratch page mapped read-only by the write-protection fault test.
pub const SCRATCH_READ_ONLY: u64 = 0xFFFF_C000_4000_0000;
/// Scratch page mapped and unmapped by the paging self-test.
pub const SCRATCH_TEST: u64 = SCRATCH_READ_ONLY + 0x1000;

/// The single owner of the page tables and free physical memory.
pub struct Memory {
    pub mapper: OffsetPageTable<'static>,
    pub frames: BitmapFrameAllocator,
    pub phys_offset: u64,
    pub stack_bottom: u64,
    pub stack_len: u64,
}

impl Memory {
    /// Virtual address through which physical address `phys` is accessible.
    pub fn phys_to_virt(&self, phys: PhysAddr) -> *mut u8 {
        (self.phys_offset + phys.as_u64()) as *mut u8
    }

    pub fn is_mapped(&self, virt: u64) -> bool {
        self.mapper.translate_addr(VirtAddr::new(virt)).is_some()
    }

    /// Maps one page at `virt` to a newly allocated frame.
    pub fn map_new(&mut self, virt: u64, flags: PageTableFlags) -> Result<PhysFrame, &'static str> {
        let page = Page::<Size4KiB>::containing_address(VirtAddr::new(virt));
        let frame = self.frames.allocate().ok_or("out of physical frames")?;
        // SAFETY: `frame` was just taken from the allocator, so nothing else
        // uses it, and `page` lies in the kernel-owned upper half, where the
        // mapper only creates mappings on behalf of this kernel.
        let result = unsafe { self.mapper.map_to(page, frame, flags, &mut self.frames) };
        match result {
            Ok(flush) => {
                flush.flush();
                Ok(frame)
            }
            Err(_) => {
                let _ = self.frames.deallocate(frame);
                Err("page already mapped or page-table allocation failed")
            }
        }
    }

    /// Unmaps the page at `virt` and returns its frame (not freed).
    pub fn unmap(&mut self, virt: u64) -> Result<PhysFrame, &'static str> {
        let page = Page::<Size4KiB>::containing_address(VirtAddr::new(virt));
        let (frame, flush) = self.mapper.unmap(page).map_err(|_| "page not mapped")?;
        flush.flush();
        Ok(frame)
    }
}

/// Takes ownership of page tables and free memory, then maps and starts the
/// heap. Panics with a clear message if the boot contract is not met.
pub fn init(boot_info: &BootInfo) -> Memory {
    let phys_offset = boot_info
        .physical_memory_offset
        .into_option()
        .expect("bootloader did not map physical memory (config.mappings.physical_memory)");

    let (l4_frame, _) = Cr3::read();
    let l4_ptr = (phys_offset + l4_frame.start_address().as_u64()) as *mut PageTable;
    // SAFETY: the bootloader maps all physical memory at `phys_offset`, so
    // `l4_ptr` is the active level-4 table. This is the only reference ever
    // created to it, and it lives as long as the kernel ('static).
    let l4: &'static mut PageTable = unsafe { &mut *l4_ptr };

    if let Some(i) = (KERNEL_HALF_FIRST_P4..512).find(|&i| !l4[i].is_unused()) {
        panic!("level-4 entry {i} in the kernel-owned half is already in use");
    }

    // SAFETY: as above, all physical memory is mapped at `phys_offset` and
    // `l4` is the active table with no other references.
    let mapper = unsafe { OffsetPageTable::new(l4, VirtAddr::new(phys_offset)) };
    // SAFETY: called once; `Usable` regions are unused per the boot contract
    // and mapped writable through the physical-memory window.
    let frames = unsafe { BitmapFrameAllocator::new(&boot_info.memory_regions, phys_offset) }
        .unwrap_or_else(|e| panic!("frame allocator: {e}"));

    let mut mem = Memory {
        mapper,
        frames,
        phys_offset,
        stack_bottom: boot_info.kernel_stack_bottom,
        stack_len: boot_info.kernel_stack_len,
    };

    let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::NO_EXECUTE;
    let mut page = HEAP_START;
    while page < HEAP_START + HEAP_SIZE {
        mem.map_new(page, flags)
            .unwrap_or_else(|e| panic!("mapping heap page {page:#x}: {e}"));
        page += 4096;
    }
    // SAFETY: the heap range was just mapped writable to frames owned by no
    // one else, and it is never unmapped.
    unsafe { heap::init(HEAP_START, HEAP_SIZE as usize) };
    mem
}
