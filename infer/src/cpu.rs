//! Which wide instructions the CPU and the OS allow (docs/stage3-design.md):
//! AVX2 and FMA need the CPU to have them (CPUID) and the OS to save the
//! 256-bit registers between programs (XCR0, read with XGETBV, which user
//! code may run once the OS has set CR4.OSXSAVE). Checked once.

use core::arch::x86_64::{__cpuid, __cpuid_count, _xgetbv};
use core::sync::atomic::{AtomicU8, Ordering};

/// 0: not checked yet; 1: no AVX2; 2: AVX2; 3: forced off (`force_portable`).
static STATE: AtomicU8 = AtomicU8::new(0);

/// Whether `quant::dot` may use the AVX2 and FMA code.
pub fn avx2() -> bool {
    match STATE.load(Ordering::Relaxed) {
        2 => true,
        1 | 3 => false,
        _ => {
            let yes = detect();
            STATE.store(if yes { 2 } else { 1 }, Ordering::Relaxed);
            yes
        }
    }
}

/// Use the portable code even where AVX2 is allowed (to compare the two:
/// `--portable` on the host's `infer`, `infer-check` and `assist-check`).
pub fn force_portable() {
    STATE.store(3, Ordering::Relaxed);
}

/// Which code `quant::dot` uses, for logs.
pub fn backend() -> &'static str {
    if avx2() { "avx2" } else { "sse" }
}

fn detect() -> bool {
    let leaf1 = __cpuid(1);
    let fma = leaf1.ecx & (1 << 12) != 0;
    let osxsave = leaf1.ecx & (1 << 27) != 0;
    let avx = leaf1.ecx & (1 << 28) != 0;
    if !(fma && osxsave && avx) {
        return false;
    }
    let avx2 = __cpuid_count(7, 0).ebx & (1 << 5) != 0;
    // SAFETY: `xgetbv` is valid because CPUID reports OSXSAVE (the OS set
    // CR4.OSXSAVE); it only reads XCR0.
    let xcr0 = unsafe { _xgetbv(0) };
    // The OS saves SSE (bit 1) and AVX (bit 2) state.
    avx2 && xcr0 & 0b110 == 0b110
}
