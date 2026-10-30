//! Global descriptor table and task state segment.
//!
//! Replaces the bootloader's GDT with one the kernel owns, adding a TSS whose
//! interrupt stack table gives the double-fault handler a known-good stack.

use spin::LazyLock;
use x86_64::VirtAddr;
use x86_64::instructions::segmentation::{CS, DS, ES, SS, Segment};
use x86_64::instructions::tables::load_tss;
use x86_64::structures::gdt::{Descriptor, GlobalDescriptorTable, SegmentSelector};
use x86_64::structures::tss::TaskStateSegment;

/// IST slot used by the double-fault handler.
pub const DOUBLE_FAULT_IST_INDEX: u16 = 0;
const DOUBLE_FAULT_STACK_SIZE: usize = 20 * 1024;

#[repr(C, align(16))]
struct Stack([u8; DOUBLE_FAULT_STACK_SIZE]);

/// Only ever written by the CPU when it switches to this stack for a double
/// fault. Rust code never creates a reference to it; only its address is used.
static mut DOUBLE_FAULT_STACK: Stack = Stack([0; DOUBLE_FAULT_STACK_SIZE]);

static TSS: LazyLock<TaskStateSegment> = LazyLock::new(|| {
    let mut tss = TaskStateSegment::new();
    // A raw pointer to the static; no reference is formed.
    let start = VirtAddr::from_ptr(&raw const DOUBLE_FAULT_STACK);
    // Stacks grow down: the IST entry is the top (end) of the region.
    tss.interrupt_stack_table[DOUBLE_FAULT_IST_INDEX as usize] =
        start + DOUBLE_FAULT_STACK_SIZE as u64;
    tss
});

struct Gdt {
    table: GlobalDescriptorTable,
    code: SegmentSelector,
    data: SegmentSelector,
    tss: SegmentSelector,
}

static GDT: LazyLock<Gdt> = LazyLock::new(|| {
    let mut table = GlobalDescriptorTable::new();
    let code = table.append(Descriptor::kernel_code_segment());
    let data = table.append(Descriptor::kernel_data_segment());
    let tss = table.append(Descriptor::tss_segment(&TSS));
    Gdt {
        table,
        code,
        data,
        tss,
    }
});

/// Loads the kernel GDT and TSS. Call once, with interrupts disabled.
pub fn init() {
    GDT.table.load();
    // SAFETY: the selectors come from the GDT just loaded: `code` is a valid
    // 64-bit kernel code segment, `data` a kernel data segment and `tss` a TSS
    // descriptor whose TSS is a 'static value. Reloading segment registers
    // with them keeps the CPU at CPL 0 with flat segments, as before.
    unsafe {
        CS::set_reg(GDT.code);
        SS::set_reg(GDT.data);
        DS::set_reg(GDT.data);
        ES::set_reg(GDT.data);
        load_tss(GDT.tss);
    }
}
