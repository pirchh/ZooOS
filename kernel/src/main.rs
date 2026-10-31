//! ZooOS kernel: boots via the bootloader crate's UEFI loader, sets up
//! its own GDT/IDT, memory ownership, heap and timer (M2), runs self-tests,
//! then starts a debug shell on the screen and keyboard (interim console).
//! Output goes to COM1 and the framebuffer. See docs/m2-design.md and
//! docs/console-design.md.

#![no_std]
#![no_main]
// `extern "x86-interrupt"` handlers; avoids hand-written assembly stubs.
#![feature(abi_x86_interrupt)]
// Fault-test kernels stop before the timer and shell, so those are unused.
#![cfg_attr(
    any(
        feature = "fault-write-protect",
        feature = "fault-heap-guard",
        feature = "fault-stack-overflow",
        feature = "triple-fault"
    ),
    allow(dead_code)
)]

extern crate alloc;

mod console;
mod cpu;
mod faults;
mod fpu;
mod fs;
mod gdt;
mod initrd;
mod interrupts;
mod keyboard;
mod klog;
mod memory;
mod process;
mod selftest;
mod serial;
mod shell;
mod trap;

use bootloader_api::config::{BootloaderConfig, Mapping};
use bootloader_api::info::MemoryRegionKind;
use bootloader_api::{BootInfo, entry_point};
use core::panic::PanicInfo;

const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(any(
    all(
        feature = "panic-test",
        any(
            feature = "fault-write-protect",
            feature = "fault-heap-guard",
            feature = "fault-stack-overflow",
            feature = "triple-fault"
        )
    ),
    all(
        feature = "fault-write-protect",
        any(
            feature = "fault-heap-guard",
            feature = "fault-stack-overflow",
            feature = "triple-fault"
        )
    ),
    all(
        feature = "fault-heap-guard",
        any(feature = "fault-stack-overflow", feature = "triple-fault")
    ),
    all(feature = "fault-stack-overflow", feature = "triple-fault"),
))]
compile_error!("at most one test-mode feature may be enabled");

const MODE: &str = if cfg!(feature = "panic-test") {
    "panic-test"
} else if cfg!(feature = "fault-write-protect") {
    "fault-write-protect"
} else if cfg!(feature = "fault-heap-guard") {
    "fault-heap-guard"
} else if cfg!(feature = "fault-stack-overflow") {
    "fault-stack-overflow"
} else if cfg!(feature = "triple-fault") {
    "triple-fault"
} else {
    "normal"
};

/// Boot contract with the bootloader (design §1–3).
pub static BOOTLOADER_CONFIG: BootloaderConfig = {
    let mut config = BootloaderConfig::new_default();
    // Map all physical memory so the kernel can reach page tables and frames.
    config.mappings.physical_memory = Some(Mapping::Dynamic);
    // Three-way split (docs/m3-design.md §1): the lower half (level-4
    // entries 0..256) is for user processes, the bootloader's dynamic
    // mappings go in entries 256..384, and 384..512 belong to the kernel.
    config.mappings.dynamic_range_start = Some(0xFFFF_8000_0000_0000);
    config.mappings.dynamic_range_end = Some(0xFFFF_BFFF_FFFF_F000);
    config.kernel_stack_size = 128 * 1024;
    config
};

entry_point!(kernel_main, config = &BOOTLOADER_CONFIG);

/// Called by the bootloader after UEFI boot services have been exited, with
/// interrupts disabled, the kernel mapped and a stack set up. Never returns.
fn kernel_main(boot_info: &'static mut BootInfo) -> ! {
    serial::init();
    // Before the first line, so BOOT is on the first screen row.
    let screen = console::init(boot_info.framebuffer.take());
    kprintln!("ZOO_OS BOOT version={}", VERSION);
    match screen {
        Ok(fb) => kprintln!(
            "ZOO_OS INFO console {}x{} format={:?} cols={} rows={}",
            fb.width,
            fb.height,
            fb.pixel_format,
            console::size().map_or(0, |s| s.0),
            console::size().map_or(0, |s| s.1)
        ),
        Err(why) => kprintln!("ZOO_OS INFO console disabled: {}", why),
    }

    let (regions, usable_bytes) = usable_memory(boot_info);
    kprintln!(
        "ZOO_OS INFO mode={} memory_regions={} usable_bytes={}",
        MODE,
        regions,
        usable_bytes
    );

    gdt::init();
    fpu::init();
    interrupts::init_idt(boot_info.kernel_stack_bottom);
    let mem = memory::init(boot_info);
    kprintln!(
        "ZOO_OS INFO memory phys_offset={:#x} stack_bottom={:#x} stack_len={:#x} heap={:#x}+{:#x}",
        mem.phys_offset,
        mem.stack_bottom,
        mem.stack_len,
        memory::HEAP_START,
        memory::HEAP_SIZE
    );
    match initrd::init(boot_info) {
        Ok((files, bytes)) => kprintln!("ZOO_OS INFO initrd files={} bytes={}", files, bytes),
        Err(why) => kprintln!("ZOO_OS INFO initrd unavailable: {}", why),
    }
    run(mem)
}

#[cfg(feature = "fault-write-protect")]
fn run(mut mem: memory::Memory) -> ! {
    faults::write_protect(&mut mem)
}

#[cfg(feature = "fault-heap-guard")]
fn run(_mem: memory::Memory) -> ! {
    faults::heap_guard()
}

#[cfg(feature = "fault-stack-overflow")]
fn run(_mem: memory::Memory) -> ! {
    faults::stack_overflow()
}

#[cfg(feature = "triple-fault")]
fn run(_mem: memory::Memory) -> ! {
    faults::triple_fault()
}

/// Normal and panic-test modes: self-tests, then READY (or the panic).
#[cfg(not(any(
    feature = "fault-write-protect",
    feature = "fault-heap-guard",
    feature = "fault-stack-overflow",
    feature = "triple-fault"
)))]
fn run(mut mem: memory::Memory) -> ! {
    selftest::breakpoint();
    selftest::frames(&mut mem);
    selftest::paging(&mut mem);
    selftest::address_spaces(&mut mem);
    selftest::heap();
    interrupts::init_timer();
    selftest::timer();

    if cfg!(feature = "panic-test") {
        panic!("intentional-test");
    }
    memory::install(mem);
    process::init();
    let (files, nodes) = fs::init();
    kprintln!("ZOO_OS INFO fs seeded files={} nodes={}", files, nodes);
    run_boot_processes();
    start_supervisor();
    kprintln!("ZOO_OS READY");
    shell::run()
}

/// Starts the resident supervisor and waits (bounded, 5 s) for its `ready`
/// message, sent once its services are running (docs/s1-design.md).
#[cfg(not(any(
    feature = "fault-write-protect",
    feature = "fault-heap-guard",
    feature = "fault-stack-overflow",
    feature = "triple-fault"
)))]
fn start_supervisor() {
    if let Err(e) = process::start_supervisor() {
        kprintln!("ZOO_OS INFO supervisor not started: {}", e.name());
        return;
    }
    let limit = interrupts::ticks() + 5 * u64::from(interrupts::TIMER_HZ);
    while interrupts::ticks() < limit {
        if let Some(msg) = process::supervisor_try_recv() {
            if msg == b"ready" {
                return;
            }
            continue;
        }
        x86_64::instructions::interrupts::disable();
        process::idle_hlt();
        x86_64::instructions::interrupts::enable();
    }
    kprintln!("ZOO_OS INFO supervisor did not report ready within 5 s");
}

/// Starts Zookeeper and waits (bounded) until every process has exited and
/// been reaped, then checks no physical memory leaked (docs/m3-design.md §9).
#[cfg(not(any(
    feature = "fault-write-protect",
    feature = "fault-heap-guard",
    feature = "fault-stack-overflow",
    feature = "triple-fault"
)))]
fn run_boot_processes() {
    // Map and free one kernel stack first, so the page tables for the
    // kernel-stack region exist before the baseline is taken.
    memory::with(|m| {
        m.map_kernel_stack(0)
            .unwrap_or_else(|e| panic!("kernel stack warm-up: {e:?}"));
        m.free_kernel_stack(0);
    });
    // Pages holding file contents outlive processes; count them as free.
    let free = || memory::with(|m| m.frames.available()) + fs::file_pages();
    let baseline = free();
    if let Err(e) = process::spawn_from_kernel("zookeeper") {
        panic!("cannot start zookeeper: {}", e.name());
    }
    let limit = 30 * u64::from(interrupts::TIMER_HZ);
    let start = interrupts::ticks();
    while process::user_tasks() > 0 {
        if interrupts::ticks() - start > limit {
            kprintln!("ZOO_OS PROC boot-timeout");
            return;
        }
        x86_64::instructions::interrupts::disable();
        process::idle_hlt();
        x86_64::instructions::interrupts::enable();
    }
    let after = free();
    let held =
        x86_64::instructions::interrupts::without_interrupts(|| fs::with(|f| f.held_pages()));
    if held != fs::file_pages() {
        panic!(
            "self-test process-cleanup: files hold {held} pages but {} are counted",
            fs::file_pages()
        );
    }
    if after != baseline {
        panic!("self-test process-cleanup: {after} frames free, expected {baseline}");
    }
    kprintln!(
        "ZOO_OS INFO processes done frames_free={} file_pages={}",
        after,
        held
    );
    kprintln!("ZOO_OS TEST process-cleanup ok");
}

/// Returns (total region count, sum of usable region sizes in bytes).
fn usable_memory(boot_info: &BootInfo) -> (usize, u64) {
    let regions = &boot_info.memory_regions;
    let usable = regions
        .iter()
        .filter(|r| r.kind == MemoryRegionKind::Usable)
        .map(|r| r.end.saturating_sub(r.start))
        .fold(0u64, u64::saturating_add);
    (regions.len(), usable)
}

/// Abort-style panic handler: stop interrupts, report on serial and screen,
/// halt. No unwinding and no reboot.
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    x86_64::instructions::interrupts::disable();
    kprintln!("ZOO_OS PANIC {}", info.message());
    if let Some(loc) = info.location() {
        kprintln!("ZOO_OS INFO panic_location={}:{}", loc.file(), loc.line());
    }
    cpu::halt_forever()
}
