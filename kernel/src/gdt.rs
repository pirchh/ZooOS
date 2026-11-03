//! Global descriptor table and task state segment.
//!
//! Kernel and user code/data segments, plus a TSS whose interrupt stack table
//! gives the double-fault handler a known-good stack and whose ring-0 stack
//! pointer (RSP0) is updated on every switch to a user process.

use core::cell::UnsafeCell;
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

/// The TSS lives in an `UnsafeCell` because the CPU reads it and the
/// scheduler rewrites RSP0 at run time. Rust code never holds a reference to
/// it: the GDT descriptor is built from its address, and updates go through
/// a raw pointer.
struct TssCell(UnsafeCell<TaskStateSegment>);

// SAFETY: single CPU; the only writer (`set_kernel_stack`) runs with
// interrupts disabled, so there is no concurrent access.
unsafe impl Sync for TssCell {}

static TSS: TssCell = TssCell(UnsafeCell::new(TaskStateSegment::new()));

/// Each processor's TSS (docs/smp-design.md): the boot processor's is
/// `TSS`; the others' are leaked by `init_ap`.
static TSS_OF: [core::sync::atomic::AtomicPtr<TaskStateSegment>; crate::smp::MAX_CPUS] =
    [const { core::sync::atomic::AtomicPtr::new(core::ptr::null_mut()) }; crate::smp::MAX_CPUS];

pub struct Selectors {
    pub kernel_code: SegmentSelector,
    pub kernel_data: SegmentSelector,
    pub user_code: SegmentSelector,
    pub user_data: SegmentSelector,
    tss: SegmentSelector,
}

struct Gdt {
    table: GlobalDescriptorTable,
    sel: Selectors,
}

static GDT: LazyLock<Gdt> = LazyLock::new(|| {
    let mut table = GlobalDescriptorTable::new();
    let kernel_code = table.append(Descriptor::kernel_code_segment());
    let kernel_data = table.append(Descriptor::kernel_data_segment());
    let user_data = table.append(Descriptor::user_data_segment());
    let user_code = table.append(Descriptor::user_code_segment());
    // SAFETY: TSS is a static, so the pointer stays valid for as long as
    // the GDT is loaded; the TSS is fully initialised before `load_tss`.
    let tss = table.append(unsafe { Descriptor::tss_segment_unchecked(TSS.0.get()) });
    Gdt {
        table,
        sel: Selectors {
            kernel_code,
            kernel_data,
            user_code,
            user_data,
            tss,
        },
    }
});

pub fn selectors() -> &'static Selectors {
    &GDT.sel
}

/// Sets the stack the CPU switches to when a trap arrives from user mode.
///
/// # Safety
/// Interrupts must be disabled, and `top` must be the 16-byte-aligned top of
/// a mapped kernel stack that stays mapped while the next user task runs.
pub unsafe fn set_kernel_stack(top: u64) {
    let cpu = crate::smp::this_cpu();
    let mut tss = TSS_OF[cpu].load(core::sync::atomic::Ordering::Relaxed);
    if tss.is_null() {
        tss = TSS.0.get();
    }
    // SAFETY: per the contract, nothing else is accessing this processor's
    // TSS now (only this processor writes it); the write goes through the
    // raw pointer without forming a reference.
    unsafe { (*tss).privilege_stack_table[0] = VirtAddr::new(top) };
    // The `syscall` entry has no TSS switch; it reads this copy. Linux
    // programs, its only users, run on the boot processor.
    if cpu == 0 {
        crate::trap::set_syscall_stack(top);
    }
}

/// A non-boot processor's own GDT and TSS (docs/smp-design.md): the same
/// segments, so the same selectors, but its own TSS (its own double-fault
/// stack). Built on the heap and never freed. Call once on that
/// processor, with interrupts disabled.
pub fn init_ap(cpu: usize) {
    use alloc::boxed::Box;
    let stack: &'static mut [u8] =
        Box::leak(alloc::vec![0u8; DOUBLE_FAULT_STACK_SIZE].into_boxed_slice());
    let tss: &'static mut TaskStateSegment = Box::leak(Box::new(TaskStateSegment::new()));
    tss.interrupt_stack_table[DOUBLE_FAULT_IST_INDEX as usize] =
        VirtAddr::from_ptr(stack.as_ptr()) + DOUBLE_FAULT_STACK_SIZE as u64;
    let mut table = GlobalDescriptorTable::new();
    let kernel_code = table.append(Descriptor::kernel_code_segment());
    let kernel_data = table.append(Descriptor::kernel_data_segment());
    let _user_data = table.append(Descriptor::user_data_segment());
    let _user_code = table.append(Descriptor::user_code_segment());
    // SAFETY: the TSS is leaked, so it outlives the GDT that points at it.
    let tss_ptr: *mut TaskStateSegment = tss;
    let tss_sel = table.append(unsafe { Descriptor::tss_segment_unchecked(tss_ptr) });
    TSS_OF[cpu].store(tss_ptr, core::sync::atomic::Ordering::Relaxed);
    let table: &'static GlobalDescriptorTable = Box::leak(Box::new(table));
    table.load();
    // SAFETY: the selectors come from the GDT just loaded (the same layout
    // as the boot processor's), flat kernel segments and a leaked TSS.
    unsafe {
        CS::set_reg(kernel_code);
        SS::set_reg(kernel_data);
        DS::set_reg(kernel_data);
        ES::set_reg(kernel_data);
        load_tss(tss_sel);
    }
}

/// Loads the kernel GDT and TSS. Call once, with interrupts disabled.
pub fn init() {
    // SAFETY: interrupts are disabled and the TSS is not yet loaded, so this
    // is the only access. Raw pointer, no reference formed to the static.
    unsafe {
        let start = VirtAddr::from_ptr(&raw const DOUBLE_FAULT_STACK);
        (*TSS.0.get()).interrupt_stack_table[DOUBLE_FAULT_IST_INDEX as usize] =
            start + DOUBLE_FAULT_STACK_SIZE as u64;
    }
    GDT.table.load();
    let s = &GDT.sel;
    // SAFETY: the selectors come from the GDT just loaded: valid 64-bit
    // kernel code and data segments and a TSS descriptor for a static TSS.
    // Reloading segment registers keeps the CPU at CPL 0 with flat segments.
    unsafe {
        CS::set_reg(s.kernel_code);
        SS::set_reg(s.kernel_data);
        DS::set_reg(s.kernel_data);
        ES::set_reg(s.kernel_data);
        load_tss(s.tss);
    }
}
