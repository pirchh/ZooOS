//! User floating-point/SSE/AVX state (docs/m5a-design.md §1,
//! docs/stage3-design.md).
//!
//! The kernel itself is soft-float and never touches x87/SSE/AVX registers,
//! so a user task's register state only needs saving when the scheduler
//! switches from one user task to another. Saving is eager: when a user task
//! is switched out, and restored when one is resumed.
//!
//! With `XSAVE` (Stage 3), the saved state includes the upper halves of the
//! 256-bit AVX registers, so programs may use AVX2. Without it (an older
//! CPU, or QEMU's plain `qemu64` model), `FXSAVE` saves x87 and SSE as
//! before, and AVX stays off: `CPUID` then tells programs it is unusable.

use core::arch::asm;
use core::arch::x86_64::{__cpuid, __cpuid_count};
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use spin::Once;
use x86_64::registers::control::{Cr0, Cr0Flags, Cr4, Cr4Flags};

/// Bytes kept per task: enough for the standard-format XSAVE image of x87,
/// SSE and AVX (832 bytes on current CPUs, from `CPUID` leaf 0xD); `init`
/// falls back to `FXSAVE` if the CPU reports more.
const AREA: usize = 1024;

/// A task's saved register state. 64-byte aligned, as `XSAVE` requires.
/// The first 512 bytes always have the FXSAVE layout (in XSAVE's standard
/// format too), which is what Linux signal frames carry.
#[repr(C, align(64))]
#[derive(Clone)]
pub struct FxArea([u8; AREA]);

static TEMPLATE: Once<FxArea> = Once::new();
/// `XSAVE` is in use (else `FXSAVE`).
static XSAVE: AtomicBool = AtomicBool::new(false);
/// The state components saved and restored (the value of XCR0).
static MASK: AtomicU64 = AtomicU64::new(0);

/// XCR0 components: x87, SSE (XMM registers), AVX (upper halves of YMM).
const X87: u64 = 1;
const SSE: u64 = 2;
const AVX: u64 = 4;

/// What `init` turned on, for the boot log.
pub struct Features {
    pub xsave: bool,
    pub avx: bool,
    /// Bytes of the save image.
    pub size: u32,
}

/// Enables SSE (and, when the CPU has them, XSAVE and AVX) for user code
/// and captures a pristine state for new tasks. Call once at boot, with
/// interrupts disabled.
pub fn init() -> Features {
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
    // CPUID leaf 1: ECX bit 26 XSAVE, bit 28 AVX.
    let leaf1 = __cpuid(1);
    let has_xsave = leaf1.ecx & (1 << 26) != 0;
    let has_avx = leaf1.ecx & (1 << 28) != 0;
    let mut size = 512;
    if has_xsave {
        let mask = X87 | SSE | if has_avx { AVX } else { 0 };
        // SAFETY: OSXSAVE enables `xsetbv`/`xgetbv`/`xsave`; XCR0 is set to
        // components the CPU reports (x87 and SSE always; AVX only with
        // CPUID's AVX bit), which `xsetbv` requires.
        unsafe {
            Cr4::update(|f| f.insert(Cr4Flags::OSXSAVE));
            asm!(
                "xsetbv",
                in("ecx") 0u32,
                in("eax") mask as u32,
                in("edx") (mask >> 32) as u32,
                options(nomem, nostack)
            );
        }
        // Leaf 0xD, sub-leaf 0, EBX: the image size for what XCR0 enables.
        size = __cpuid_count(0xD, 0).ebx;
        if size as usize <= AREA {
            MASK.store(mask, Ordering::Relaxed);
            XSAVE.store(true, Ordering::Relaxed);
        } else {
            // An image larger than the area: stay with FXSAVE, AVX off.
            // SAFETY: as above; x87 and SSE only.
            unsafe {
                asm!(
                    "xsetbv",
                    in("ecx") 0u32,
                    in("eax") (X87 | SSE) as u32,
                    in("edx") 0u32,
                    options(nomem, nostack)
                );
            }
            size = 512;
        }
    }
    let mut area = FxArea([0; AREA]);
    let mxcsr: u32 = 0x1F80; // all SIMD exceptions masked, round to nearest
    // SAFETY: `fninit` and `ldmxcsr` reset the FPU/SSE control state (not
    // used by the kernel).
    unsafe {
        asm!("fninit", options(nomem, nostack));
        asm!("ldmxcsr [{}]", in(reg) &mxcsr, options(nostack, readonly));
    }
    save(&mut area);
    TEMPLATE.call_once(|| area);
    Features {
        xsave: XSAVE.load(Ordering::Relaxed),
        avx: MASK.load(Ordering::Relaxed) & AVX != 0,
        size,
    }
}

/// A fresh state for a new task (masked exceptions, empty registers).
pub fn new_state() -> FxArea {
    TEMPLATE.get().expect("fpu::init not called").clone()
}

/// Saves the current registers into `area`.
pub fn save(area: &mut FxArea) {
    if XSAVE.load(Ordering::Relaxed) {
        let mask = MASK.load(Ordering::Relaxed);
        // SAFETY: `area` is 64-byte aligned and at least as large as the
        // image `init` measured for exactly these components; `xsave64`
        // only reads registers and writes the area.
        unsafe {
            asm!(
                "xsave64 [{}]",
                in(reg) area,
                in("eax") mask as u32,
                in("edx") (mask >> 32) as u32,
                options(nostack)
            );
        }
    } else {
        // SAFETY: writes 512 bytes into a 16-byte-aligned area owned by the
        // caller; only reads CPU registers.
        unsafe { asm!("fxsave64 [{}]", in(reg) area, options(nostack)) };
    }
}

/// The 512 bytes in FXSAVE layout (Linux signal frames carry these).
pub fn bytes(area: &FxArea) -> &[u8; 512] {
    area.0[..512].try_into().expect("512 bytes")
}

/// An area from user-supplied FXSAVE bytes, if its MXCSR has no bits the
/// CPU rejects (restoring would fault in the kernel otherwise). The AVX
/// upper halves are set to their initial state (zero).
pub fn from_user(bytes: &[u8; 512]) -> Option<FxArea> {
    let template = TEMPLATE.get().expect("fpu::init not called");
    let word = |a: &[u8], at: usize| u32::from_le_bytes([a[at], a[at + 1], a[at + 2], a[at + 3]]);
    // MXCSR_MASK at offset 28; 0 means the architectural default 0xFFBF.
    let mask = match word(&template.0, 28) {
        0 => 0xFFBF,
        m => m,
    };
    if word(bytes, 24) & !mask != 0 {
        return None;
    }
    let mut area = template.clone();
    area.0[..512].copy_from_slice(bytes);
    if XSAVE.load(Ordering::Relaxed) {
        // The XSAVE header: XSTATE_BV says x87 and SSE come from the area
        // (AVX from its initial state); XCOMP_BV 0 is the standard format.
        area.0[512..520].copy_from_slice(&(X87 | SSE).to_le_bytes());
        area.0[520..576].fill(0);
    }
    Some(area)
}

/// Loads the registers from `area`.
pub fn restore(area: &FxArea) {
    if XSAVE.load(Ordering::Relaxed) {
        let mask = MASK.load(Ordering::Relaxed);
        // SAFETY: `area` was produced by `xsave64` with the same components
        // (or by `from_user`, which writes a valid standard-format header),
        // so `xrstor64` finds a valid image; only registers the kernel does
        // not use change.
        unsafe {
            asm!(
                "xrstor64 [{}]",
                in(reg) area,
                in("eax") mask as u32,
                in("edx") (mask >> 32) as u32,
                options(nostack, readonly)
            );
        }
    } else {
        // SAFETY: reads 512 bytes from a 16-byte-aligned area produced by
        // `fxsave64` (or the template), so every field is valid; only
        // changes registers the kernel does not use.
        unsafe { asm!("fxrstor64 [{}]", in(reg) area, options(nostack, readonly)) };
    }
}
