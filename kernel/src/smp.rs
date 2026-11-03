//! The other processors (docs/smp-design.md), step 1: found through the ACPI
//! MADT, started with INIT and SIPI, brought into 64-bit mode by a small
//! trampoline in low memory, given their own GDT, TSS and local APIC, and
//! parked. They run no tasks yet.
//!
//! The trampoline is the only assembly: real mode, then protected mode,
//! then long mode, with temporary page tables that map the trampoline's
//! page to itself and the kernel's upper half as the kernel maps it. The
//! processor then jumps to `ap_entry`, which loads the kernel's own tables.

use crate::kprintln;
use crate::memory::Memory;
use alloc::boxed::Box;
use alloc::vec::Vec;
use core::arch::global_asm;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use x86_64::PhysAddr;
use x86_64::registers::control::{Cr3, Cr3Flags};
use x86_64::structures::paging::PhysFrame;

/// Most processors brought up (the boot processor included).
pub const MAX_CPUS: usize = 32;
/// Each parked processor's stack.
const AP_STACK: usize = 32 * 1024;

/// Low pages (below 1 MiB, from `Usable` regions) for the trampoline and
/// its temporary page tables: the trampoline, PML4, PDPT, PD.
static LOW_PAGES: [AtomicU64; 4] = [const { AtomicU64::new(0) }; 4];
/// Processors online, the boot processor included.
static ONLINE: AtomicU32 = AtomicU32::new(1);
/// Set by a processor that has reached `ap_entry`'s end.
static STARTED: AtomicBool = AtomicBool::new(false);
/// The kernel's level-4 table, for `ap_entry`.
static KERNEL_P4: AtomicU64 = AtomicU64::new(0);

/// Records the low pages, from the bootloader's memory map: four whole
/// usable pages at or above 0x1000 and below 0x9F000 (SIPI can start a
/// processor only below 1 MiB, and the top of conventional memory belongs
/// to the EBDA). Returns how many were found.
pub fn set_low_pages(regions: &[bootloader_api::info::MemoryRegion]) -> usize {
    let mut found = 0;
    for r in regions {
        if r.kind != bootloader_api::info::MemoryRegionKind::Usable {
            continue;
        }
        let mut page = r.start.max(0x1000).div_ceil(0x1000) * 0x1000;
        while page + 0x1000 <= r.end.min(0x9F000) && found < LOW_PAGES.len() {
            LOW_PAGES[found].store(page, Ordering::Relaxed);
            found += 1;
            page += 0x1000;
        }
    }
    found
}

pub fn online() -> u32 {
    ONLINE.load(Ordering::Relaxed)
}

// The trampoline (AT&T syntax). It is copied to a low page `T` and entered
// by SIPI at `T:0` in real mode. Fields after the code are filled by
// `start_one` before each start.
global_asm!(
    ".section .text.zoo_ap_trampoline, \"ax\"",
    ".global zoo_ap_start",
    ".global zoo_ap_end",
    ".code16",
    "zoo_ap_start:",
    "cli",
    "cld",
    "movw %cs, %ax",
    "movw %ax, %ds",
    // ESI = T, the page's physical address, for the 32-bit part.
    "movzwl %ax, %esi",
    "shll $4, %esi",
    "lgdtl (zoo_ap_gdtr - zoo_ap_start)",
    "movl %cr0, %eax",
    "orl $1, %eax",
    "movl %eax, %cr0",
    "ljmpl *(zoo_ap_pm32_ptr - zoo_ap_start)",
    ".code32",
    "zoo_ap_pm32:",
    "movw $0x10, %ax",
    "movw %ax, %ds",
    "movw %ax, %es",
    "movw %ax, %ss",
    // PAE, then the temporary tables, then long mode with NX, then paging
    // with write protection.
    "movl %cr4, %eax",
    "orl $0x20, %eax",
    "movl %eax, %cr4",
    "movl (zoo_ap_cr3 - zoo_ap_start)(%esi), %eax",
    "movl %eax, %cr3",
    "movl $0xC0000080, %ecx",
    "rdmsr",
    "orl $0x900, %eax",
    "wrmsr",
    "movl %cr0, %eax",
    "orl $0x80010000, %eax",
    "movl %eax, %cr0",
    "ljmpl *(zoo_ap_lm_ptr - zoo_ap_start)(%esi)",
    ".code64",
    "zoo_ap_lm64:",
    "xorl %eax, %eax",
    "movw %ax, %ds",
    "movw %ax, %es",
    "movw %ax, %ss",
    "movq zoo_ap_stack(%rip), %rsp",
    "movq zoo_ap_arg(%rip), %rdi",
    "movq zoo_ap_entry(%rip), %rax",
    "jmpq *%rax",
    ".balign 8",
    "zoo_ap_gdt:",
    ".quad 0",
    // 0x08: 32-bit code; 0x10: data; 0x18: 64-bit code.
    ".quad 0x00CF9A000000FFFF",
    ".quad 0x00CF92000000FFFF",
    ".quad 0x00AF9A000000FFFF",
    "zoo_ap_gdtr:",
    ".word 31",
    ".long 0",
    "zoo_ap_pm32_ptr:",
    ".long 0",
    ".word 0x08",
    "zoo_ap_lm_ptr:",
    ".long 0",
    ".word 0x18",
    "zoo_ap_cr3:",
    ".long 0",
    ".balign 8",
    "zoo_ap_stack:",
    ".quad 0",
    "zoo_ap_arg:",
    ".quad 0",
    "zoo_ap_entry:",
    ".quad 0",
    "zoo_ap_end:",
    ".text",
    options(att_syntax)
);

unsafe extern "C" {
    static zoo_ap_start: u8;
    static zoo_ap_end: u8;
    static zoo_ap_gdtr: u8;
    static zoo_ap_pm32: u8;
    static zoo_ap_pm32_ptr: u8;
    static zoo_ap_lm64: u8;
    static zoo_ap_lm_ptr: u8;
    static zoo_ap_cr3: u8;
    static zoo_ap_gdt: u8;
    static zoo_ap_stack: u8;
    static zoo_ap_arg: u8;
    static zoo_ap_entry: u8;
}

/// Offset of trampoline symbol `sym` from its start.
fn off(sym: *const u8) -> u64 {
    // SAFETY: only the symbols' addresses are taken, never their contents.
    sym as u64 - (&raw const zoo_ap_start) as u64
}

/// Starts every other processor the MADT lists, one at a time, and parks
/// it. Call once, from the boot processor, after the local APIC timer and
/// the heap are up, with interrupts enabled or not.
pub fn start_all(mem: &mut Memory) {
    let pages: Vec<u64> = LOW_PAGES
        .iter()
        .map(|p| p.load(Ordering::Relaxed))
        .collect();
    if pages.contains(&0) {
        kprintln!("ZOO_OS INFO smp: no low memory for the trampoline");
        return;
    }
    let Some(bsp) = crate::apic::id() else {
        kprintln!("ZOO_OS INFO smp: no local APIC");
        return;
    };
    let ids = match crate::apic::processors(mem) {
        Ok(ids) => ids,
        Err(why) => {
            kprintln!("ZOO_OS INFO smp: {}", why);
            return;
        }
    };
    let others: Vec<u32> = ids.into_iter().filter(|&id| id != bsp).collect();
    if others.is_empty() {
        kprintln!("ZOO_OS INFO smp cpus=1");
        return;
    }
    let (t, p4, p3, p2) = (pages[0], pages[1], pages[2], pages[3]);
    KERNEL_P4.store(mem.kernel_p4.start_address().as_u64(), Ordering::Relaxed);
    let virt = |phys: u64| mem.phys_to_virt(PhysAddr::new(phys));
    // SAFETY: the four pages are whole `Usable` pages below 1 MiB, which
    // the frame allocator never hands out (`LOW_MEMORY_LIMIT`), so nothing
    // else uses them; they are reached through the physical-memory map.
    unsafe {
        let len = off(&raw const zoo_ap_end) as usize;
        core::ptr::copy_nonoverlapping(&raw const zoo_ap_start, virt(t), len);
        let put32 = |at: u64, v: u32| (virt(t + at) as *mut u32).write_unaligned(v);
        put32(
            off(&raw const zoo_ap_gdtr) + 2,
            (t + off(&raw const zoo_ap_gdt)) as u32,
        );
        put32(
            off(&raw const zoo_ap_pm32_ptr),
            (t + off(&raw const zoo_ap_pm32)) as u32,
        );
        put32(
            off(&raw const zoo_ap_lm_ptr),
            (t + off(&raw const zoo_ap_lm64)) as u32,
        );
        put32(off(&raw const zoo_ap_cr3), p4 as u32);
        // Temporary tables: the first 2 MiB mapped to itself (the
        // trampoline runs there while paging comes on), and the kernel's
        // upper half as the kernel maps it.
        core::ptr::write_bytes(virt(p4), 0, 4096);
        core::ptr::write_bytes(virt(p3), 0, 4096);
        core::ptr::write_bytes(virt(p2), 0, 4096);
        let (present_writable, huge) = (0b11u64, 1u64 << 7);
        (virt(p2) as *mut u64).write(present_writable | huge);
        (virt(p3) as *mut u64).write(p2 | present_writable);
        (virt(p4) as *mut u64).write(p3 | present_writable);
        let kp4 = virt(mem.kernel_p4.start_address().as_u64()) as *const u64;
        for i in crate::memory::UPPER_HALF_FIRST_P4..512 {
            (virt(p4) as *mut u64).add(i).write(kp4.add(i).read());
        }
    }
    for (k, &id) in others.iter().enumerate() {
        let cpu = k + 1;
        if cpu >= MAX_CPUS {
            break;
        }
        start_one(t, id, cpu as u64, &virt);
    }
    kprintln!("ZOO_OS INFO smp cpus={}", online());
}

/// Starts the processor with local APIC id `apic` as processor `cpu`, and
/// waits (bounded) for it to report.
fn start_one(t: u64, apic: u32, cpu: u64, virt: &dyn Fn(u64) -> *mut u8) {
    let stack: Box<[u8]> = alloc::vec![0u8; AP_STACK].into_boxed_slice();
    // The stack is never freed: the processor keeps it.
    let top = (Box::leak(stack).as_ptr() as u64 + AP_STACK as u64) & !0xF;
    // SAFETY: the trampoline page was set up by `start_all`; these fields
    // are read by the processor only after the SIPI below, and the one
    // before it has reported (or timed out and is never started again).
    unsafe {
        let put64 = |at: u64, v: u64| (virt(t + at) as *mut u64).write_unaligned(v);
        // As after a `call`: the stack is 8 bytes off 16-byte alignment.
        put64(off(&raw const zoo_ap_stack), top - 8);
        put64(off(&raw const zoo_ap_arg), cpu);
        put64(off(&raw const zoo_ap_entry), ap_entry as *const () as u64);
    }
    STARTED.store(false, Ordering::SeqCst);
    let vector = (t >> 12) as u8;
    crate::apic::send_init(apic);
    crate::apic::delay_us(10_000);
    for _ in 0..2 {
        crate::apic::send_sipi(apic, vector);
        crate::apic::delay_us(200);
        if STARTED.load(Ordering::SeqCst) {
            break;
        }
    }
    // Up to 100 ms for it to report.
    for _ in 0..1000 {
        if STARTED.load(Ordering::SeqCst) {
            return;
        }
        crate::apic::delay_us(100);
    }
    kprintln!("ZOO_OS INFO smp: processor apic={} did not start", apic);
}

/// Where a started processor arrives, in 64-bit mode on its own stack,
/// with the temporary page tables.
extern "C" fn ap_entry(cpu: u64) -> ! {
    // SAFETY: the kernel's level-4 table maps the whole kernel (code,
    // statics, heap and this stack) as the temporary tables did, so the
    // code and stack in use stay mapped across the switch.
    unsafe {
        let p4 = PhysFrame::containing_address(PhysAddr::new(KERNEL_P4.load(Ordering::Relaxed)));
        Cr3::write(p4, Cr3Flags::empty());
    }
    crate::gdt::init_ap();
    crate::interrupts::load_idt();
    let _ = crate::fpu::init();
    crate::apic::init_ap();
    let n = ONLINE.fetch_add(1, Ordering::SeqCst) + 1;
    kprintln!(
        "ZOO_OS INFO cpu {} online apic={}",
        cpu,
        crate::apic::id().unwrap_or(0)
    );
    let _ = n;
    STARTED.store(true, Ordering::SeqCst);
    // Parked: no tasks run here yet (docs/smp-design.md, step 1).
    loop {
        x86_64::instructions::interrupts::disable();
        x86_64::instructions::hlt();
    }
}
