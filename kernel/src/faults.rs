//! Deliberate faults for the fault-test kernels. Each function must not
//! return: the exception handler reports and halts.

#![allow(dead_code)] // only the function for the enabled test mode is used

use crate::memory::{self, Memory};
use x86_64::structures::paging::PageTableFlags;

/// Maps a page without WRITABLE and writes to it. With CR0.WP set (the
/// bootloader sets it) this must raise a protection page fault.
pub fn write_protect(mem: &mut Memory) -> ! {
    let va = memory::SCRATCH_READ_ONLY;
    let flags = PageTableFlags::PRESENT | PageTableFlags::NO_EXECUTE;
    mem.map_new(va, flags)
        .unwrap_or_else(|e| panic!("fault-write-protect: map: {e}"));
    // SAFETY: deliberately invalid: the page is mapped read-only, so this
    // write faults before any memory changes. That fault is the test.
    unsafe { (va as *mut u8).write_volatile(0xAA) };
    panic!("fault-write-protect: write to a read-only page did not fault");
}

/// Reads the first byte of the unmapped page just above the heap.
pub fn heap_guard() -> ! {
    // SAFETY: deliberately invalid: the guard page is unmapped, so the read
    // faults. That fault is the test.
    let v = unsafe { (memory::HEAP_GUARD_HIGH as *const u8).read_volatile() };
    panic!("fault-heap-guard: read of guard page returned {v}");
}

/// Recurses until the kernel stack runs into its guard page.
pub fn stack_overflow() -> ! {
    let depth = recurse(0);
    panic!("fault-stack-overflow: recursion returned at depth {depth}");
}

#[allow(unconditional_recursion)]
#[inline(never)]
fn recurse(depth: u64) -> u64 {
    // A 512-byte frame the optimiser cannot remove, and a call that is not in
    // tail position, so each level really uses stack.
    let buf = core::hint::black_box([depth; 64]);
    recurse(depth + 1) + core::hint::black_box(buf[63])
}

/// Loads an IDT with no handlers and raises #BP. With no breakpoint, #GP or
/// double-fault handler the CPU triple-faults; QEMU (-no-reboot) then exits.
/// This mode exists so the verifier can prove it rejects a silent reset.
pub fn triple_fault() -> ! {
    static EMPTY: spin::LazyLock<x86_64::structures::idt::InterruptDescriptorTable> =
        spin::LazyLock::new(x86_64::structures::idt::InterruptDescriptorTable::new);
    EMPTY.load();
    x86_64::instructions::interrupts::int3();
    panic!("triple-fault: CPU continued after int3 with an empty IDT");
}
