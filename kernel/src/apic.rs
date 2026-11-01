//! The local APIC timer as the system tick (docs/h3-design.md). Some recent
//! PCs gate the legacy PIT's clock, which would stop ZooOS at boot; the
//! local APIC timer is in every x86-64 CPU. It is calibrated against the
//! ACPI power-management timer (3.579545 MHz, found through the ACPI FADT),
//! which nearly every PC has. Without either, the PIT stays in use.

use crate::memory::Memory;
use core::sync::atomic::{AtomicU64, Ordering};
use x86_64::PhysAddr;
use x86_64::instructions::port::Port;
use x86_64::registers::model_specific::Msr;

/// ACPI PM timer frequency (Hz).
const PM_HZ: u64 = 3_579_545;
/// Calibration window: PM-timer counts in 10 ms (one tick).
const PM_PER_TICK: u64 = PM_HZ / crate::interrupts::TIMER_HZ as u64;
/// Bounded waits (iterations).
const SPINS: u64 = 200_000_000;

const IA32_APIC_BASE: u32 = 0x1B;
// Local APIC registers.
const TPR: u64 = 0x80;
const EOI: u64 = 0xB0;
const SVR: u64 = 0xF0;
const LVT_TIMER: u64 = 0x320;
const TIMER_INITIAL: u64 = 0x380;
const TIMER_CURRENT: u64 = 0x390;
const TIMER_DIVIDE: u64 = 0x3E0;
/// Spurious-interrupt vector (its handler does nothing).
pub const SPURIOUS_VECTOR: u8 = 0xFF;

/// The RSDP's physical address from the bootloader (0: none).
static RSDP: AtomicU64 = AtomicU64::new(0);
/// The local APIC's mapped registers once it drives the tick (0: PIT).
static LAPIC: AtomicU64 = AtomicU64::new(0);

pub fn set_rsdp(phys: u64) {
    RSDP.store(phys, Ordering::Relaxed);
}

/// True when the local APIC timer is the tick source.
pub fn active() -> bool {
    LAPIC.load(Ordering::Relaxed) != 0
}

fn write(base: u64, reg: u64, v: u32) {
    // SAFETY: `base` is the local APIC's register page, mapped uncached by
    // `init`; every register used is inside it.
    unsafe { core::ptr::write_volatile((base + reg) as *mut u32, v) }
}

fn read(base: u64, reg: u64) -> u32 {
    // SAFETY: as in `write`.
    unsafe { core::ptr::read_volatile((base + reg) as *const u32) }
}

/// This CPU's local APIC id, once the local APIC drives the tick.
pub fn id() -> Option<u32> {
    let base = LAPIC.load(Ordering::Relaxed);
    (base != 0).then(|| read(base, 0x20) >> 24)
}

/// End of interrupt for the timer (interrupt context).
pub fn eoi() {
    let base = LAPIC.load(Ordering::Relaxed);
    if base != 0 {
        write(base, EOI, 0);
    }
}

/// The ACPI PM timer: (I/O port, 32-bit counter) from the FADT.
fn pm_timer(mem: &Memory) -> Result<(u16, bool), &'static str> {
    let rsdp = RSDP.load(Ordering::Relaxed);
    if rsdp == 0 {
        return Err("no-acpi");
    }
    let at = |phys: u64| mem.phys_to_virt(PhysAddr::new(phys));
    // SAFETY (all reads below): ACPI tables live in memory the firmware
    // reported, which the bootloader maps at `phys_offset`; reads are
    // unaligned because the tables are byte-packed. Lengths come from the
    // tables' own headers and are bounded.
    let r8 = |phys: u64| unsafe { at(phys).read_unaligned() };
    let r32 = |phys: u64| unsafe { (at(phys) as *const u32).read_unaligned() };
    let r64 = |phys: u64| unsafe { (at(phys) as *const u64).read_unaligned() };
    let sig = |phys: u64, want: &[u8]| (0..want.len()).all(|i| r8(phys + i as u64) == want[i]);
    if !sig(rsdp, b"RSD PTR ") {
        return Err("bad-rsdp");
    }
    let (root, entry) = if r8(rsdp + 15) >= 2 {
        (r64(rsdp + 24), 8)
    } else {
        (u64::from(r32(rsdp + 16)), 4)
    };
    let len = u64::from(r32(root + 4)).min(36 + 8 * 256);
    let mut off = 36;
    while off + entry <= len {
        let table = if entry == 8 {
            r64(root + off)
        } else {
            u64::from(r32(root + off))
        };
        off += entry;
        if !sig(table, b"FACP") {
            continue;
        }
        let fadt_len = r32(table + 4);
        let flags = r32(table + 112);
        let ext = flags & (1 << 8) != 0;
        let mut port = r32(table + 76) as u64;
        // ACPI 2.0+: the extended address, if it is in I/O space.
        if port == 0 && fadt_len >= 220 && r8(table + 208) == 1 {
            port = r64(table + 212);
        }
        if port == 0 || port > 0xFFFF {
            return Err("no-pm-timer");
        }
        return Ok((port as u16, ext));
    }
    Err("no-fadt")
}

/// Starts the local APIC timer at the system tick rate on `vector`.
/// Returns (APIC counts per tick, PM timer port). Call with interrupts
/// disabled, before anything relies on ticks.
pub fn init(mem: &mut Memory, vector: u8) -> Result<(u32, u16), &'static str> {
    // CPUID.1:EDX bit 9: a local APIC.
    if core::arch::x86_64::__cpuid(1).edx & (1 << 9) == 0 {
        return Err("no-apic");
    }
    let (pm_port, pm_32) = pm_timer(mem)?;
    let mask: u32 = if pm_32 { u32::MAX } else { 0x00FF_FFFF };
    let pm = || {
        // SAFETY: reading the ACPI PM timer port the FADT named.
        unsafe { Port::<u32>::new(pm_port).read() & mask }
    };

    let mut msr = Msr::new(IA32_APIC_BASE);
    // SAFETY: IA32_APIC_BASE exists on every CPU with a local APIC
    // (checked above); setting bit 11 (enable) keeps the base unchanged.
    let base_phys = unsafe {
        let v = msr.read();
        if v & (1 << 11) == 0 {
            msr.write(v | 1 << 11);
        }
        v & 0x000F_FFFF_FFFF_F000
    };
    let base = mem.map_mmio(base_phys, 1)?;

    write(base, TPR, 0);
    write(base, SVR, 1 << 8 | u32::from(SPURIOUS_VECTOR));
    write(base, TIMER_DIVIDE, 0x3); // divide by 16
    write(base, LVT_TIMER, 1 << 16 | u32::from(vector)); // masked, one-shot

    // Count down from the top for one tick of PM-timer time.
    let start = pm();
    write(base, TIMER_INITIAL, u32::MAX);
    let mut elapsed = 0u64;
    for _ in 0..SPINS {
        elapsed = u64::from(pm().wrapping_sub(start) & mask);
        if elapsed >= PM_PER_TICK {
            break;
        }
        core::hint::spin_loop();
    }
    let counted = u64::from(u32::MAX - read(base, TIMER_CURRENT));
    write(base, TIMER_INITIAL, 0);
    if elapsed < PM_PER_TICK {
        return Err("pm-timer-stuck");
    }
    let per_tick = counted * PM_PER_TICK / elapsed;
    if per_tick == 0 || per_tick > u64::from(u32::MAX) {
        return Err("calibration-out-of-range");
    }
    // Periodic, unmasked.
    write(base, LVT_TIMER, 1 << 17 | u32::from(vector));
    write(base, TIMER_INITIAL, per_tick as u32);
    LAPIC.store(base, Ordering::Relaxed);
    Ok((per_tick as u32, pm_port))
}
