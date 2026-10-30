//! OriginsOS M1 kernel: boots via the bootloader crate's UEFI loader, reports
//! startup markers on COM1, and halts. No allocator, interrupts, or scheduler.

#![no_std]
#![no_main]

mod cpu;
mod serial;

use bootloader_api::info::MemoryRegionKind;
use bootloader_api::{BootInfo, entry_point};
use core::panic::PanicInfo;

const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(not(feature = "panic-test"))]
const MODE: &str = "normal";
#[cfg(feature = "panic-test")]
const MODE: &str = "panic-test";

entry_point!(kernel_main);

/// Called by the bootloader after UEFI boot services have been exited, with
/// the kernel mapped and a stack set up. Never returns.
fn kernel_main(boot_info: &'static mut BootInfo) -> ! {
    serial::init();
    serial_println!("ORIGINS_OS BOOT version={}", VERSION);

    // Derived from data the bootloader handed to this kernel at runtime, so
    // it is evidence that guest kernel code ran, not a fixed string.
    let (regions, usable_bytes) = usable_memory(boot_info);
    serial_println!(
        "ORIGINS_OS INFO mode={} memory_regions={} usable_bytes={}",
        MODE,
        regions,
        usable_bytes
    );

    #[cfg(feature = "panic-test")]
    panic!("intentional-test");

    #[cfg(not(feature = "panic-test"))]
    {
        serial_println!("ORIGINS_OS READY");
        cpu::halt_forever()
    }
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

/// Abort-style panic handler: report on serial, then halt. No unwinding and
/// no reboot (QEMU is run with -no-reboot, and the host harness stops it).
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    serial_println!("ORIGINS_OS PANIC {}", info.message());
    if let Some(loc) = info.location() {
        serial_println!(
            "ORIGINS_OS INFO panic_location={}:{}",
            loc.file(),
            loc.line()
        );
    }
    cpu::halt_forever()
}
