//! User floating-point/SSE state (docs/m5a-design.md §1).
//!
//! The kernel itself is soft-float and never touches x87/SSE registers, so
//! a user task's register state only needs saving when the scheduler
//! switches from one user task to another. Saving is eager: `fxsave64` when
//! a user task is switched out, `fxrstor64` when one is resumed.

use core::arch::asm;
use spin::Once;
use x86_64::registers::control::{Cr0, Cr0Flags, Cr4, Cr4Flags};

/// The 512-byte FXSAVE image, aligned as the instructions require.
#[repr(C, align(16))]
#[derive(Clone)]
pub struct FxArea([u8; 512]);

static TEMPLATE: Once<FxArea> = Once::new();

/// Enables SSE for user code and captures a pristine state for new tasks.
/// Call once at boot, with interrupts disabled.
pub fn init() {
    // SAFETY: setting MP and clearing EM/TS makes x87/SSE instructions
    // execute instead of trapping; OSFXSR enables SSE and fxsave/fxrstor;
    // OSXMMEXCPT routes SIMD exceptions to #XM. None of this affects memory
    // safety; the kernel itself does not use these registers.
    unsafe {
        Cr0::update(|f| {
            f.remove(Cr0Flags::EMULATE_COPROCESSOR | Cr0Flags::TASK_SWITCHED);
            f.insert(Cr0Flags::MONITOR_COPROCESSOR);
        });
        Cr4::update(|f| f.insert(Cr4Flags::OSFXSR | Cr4Flags::OSXMMEXCPT_ENABLE));
    }
    let mut area = FxArea([0; 512]);
    let mxcsr: u32 = 0x1F80; // all SIMD exceptions masked, round to nearest
    // SAFETY: `fninit` and `ldmxcsr` reset the FPU/SSE control state (not
    // used by the kernel); `fxsave64` writes exactly 512 bytes to a
    // 16-byte-aligned buffer we own.
    unsafe {
        asm!("fninit", options(nomem, nostack));
        asm!("ldmxcsr [{}]", in(reg) &mxcsr, options(nostack, readonly));
        asm!("fxsave64 [{}]", in(reg) &mut area, options(nostack));
    }
    TEMPLATE.call_once(|| area);
}

/// A fresh state for a new task (masked exceptions, empty registers).
pub fn new_state() -> FxArea {
    TEMPLATE.get().expect("fpu::init not called").clone()
}

/// Saves the current x87/SSE registers into `area`.
pub fn save(area: &mut FxArea) {
    // SAFETY: writes 512 bytes into a 16-byte-aligned area owned by the
    // caller; only reads CPU registers.
    unsafe { asm!("fxsave64 [{}]", in(reg) area, options(nostack)) };
}

/// The 512 bytes of an FXSAVE image.
pub fn bytes(area: &FxArea) -> &[u8; 512] {
    &area.0
}

/// An area from user-supplied bytes, if its MXCSR has no bits the CPU
/// rejects (`fxrstor64` would fault in the kernel otherwise).
pub fn from_user(bytes: &[u8; 512]) -> Option<FxArea> {
    let template = TEMPLATE.get().expect("fpu::init not called");
    let word =
        |a: &[u8; 512], at: usize| u32::from_le_bytes([a[at], a[at + 1], a[at + 2], a[at + 3]]);
    // MXCSR_MASK at offset 28; 0 means the architectural default 0xFFBF.
    let mask = match word(&template.0, 28) {
        0 => 0xFFBF,
        m => m,
    };
    if word(bytes, 24) & !mask != 0 {
        return None;
    }
    Some(FxArea(*bytes))
}

/// Loads the x87/SSE registers from `area`.
pub fn restore(area: &FxArea) {
    // SAFETY: reads 512 bytes from a 16-byte-aligned area produced by
    // `fxsave64` (or the template), so every field is valid; only changes
    // registers the kernel does not use.
    unsafe { asm!("fxrstor64 [{}]", in(reg) area, options(nostack, readonly)) };
}
