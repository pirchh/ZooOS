//! NarwhalTest: a floating-point worker (docs/m5a-design.md §5). Receives
//! a parameter on its channel, runs `narwhal_series` (long enough to be
//! preempted many times, alongside another NarwhalTest), and sends back the
//! result's exact bits. Also uses a few MiB of heap.
//!
//! Where the CPU and the OS allow AVX (docs/stage3-design.md), it first fills
//! all sixteen 256-bit registers with a pattern of its own, spins while the
//! other NarwhalTest does the same, and checks every bit came back: the
//! kernel must save the upper halves (XSAVE), which FXSAVE does not.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use zoo_rt::{recv, send};

zoo_rt::entry!(main);

/// Iterations: long enough to span many timer ticks under emulation.
pub const ITERATIONS: u64 = 3_000_000;

fn main(channel: u64) -> u8 {
    let mut buf = [0u8; 16];
    let Ok(8) = recv(channel, &mut buf) else {
        return 2;
    };
    let param = f64::from_bits(u64::from_le_bytes(buf[..8].try_into().unwrap_or([0; 8])));

    if zoo_infer::cpu::avx2() {
        let t = zoo_rt::ticks();
        // SAFETY: `avx2()` is true, so the CPU has AVX and the OS saves
        // YMM state.
        let kept = unsafe { ymm_kept(param.to_bits()) };
        if !kept {
            let _ = zoo_rt::log("NarwhalTest: AVX registers changed across task switches");
            return 5;
        }
        zoo_rt::log!(
            "NarwhalTest: AVX registers kept over {} ticks",
            zoo_rt::ticks() - t
        );
    } else {
        let _ = zoo_rt::log("NarwhalTest: no AVX here; register check skipped");
    }

    // Exercise heap growth beyond the first 1 MiB chunk.
    let mut big: Vec<u8> = Vec::with_capacity(4 << 20);
    big.resize(4 << 20, 0xBE);
    if big.iter().step_by(4096).any(|&b| b != 0xBE) {
        return 3;
    }

    let mut trace: Vec<f64> = Vec::new();
    let result = zoo_rt::narwhal_series(param, ITERATIONS, &mut trace);
    if send(channel, &result.to_bits().to_le_bytes()).is_err() {
        return 4;
    }
    0
}

/// Loads a pattern from `seed` into YMM0..YMM15, spins long enough to be
/// switched out many times, then stores the registers and compares. One
/// `asm` block, so no compiler code touches the registers in between.
///
/// # Safety
/// The CPU must have AVX and the OS must save YMM state.
#[target_feature(enable = "avx")]
unsafe fn ymm_kept(seed: u64) -> bool {
    let mut src = [[0u64; 4]; 16];
    for (r, lanes) in src.iter_mut().enumerate() {
        for (l, v) in lanes.iter_mut().enumerate() {
            *v = seed
                .wrapping_mul(0x9E37_79B9_7F4A_7C15)
                .wrapping_add(((r * 4 + l) as u64).wrapping_mul(0xBF58_476D_1CE4_E5B9));
        }
    }
    let mut dst = [[0u64; 4]; 16];
    // About 0.1 s at 3 GHz: many 10 ms ticks, with the other NarwhalTest
    // running between.
    let spins: u64 = 300_000_000;
    // SAFETY: `src` and `dst` are 16 * 32 readable / writable bytes; every
    // YMM register used is declared clobbered; the loop only uses `n`.
    unsafe {
        core::arch::asm!(
            "vmovdqu ymm0, [{s}]",
            "vmovdqu ymm1, [{s} + 32]",
            "vmovdqu ymm2, [{s} + 64]",
            "vmovdqu ymm3, [{s} + 96]",
            "vmovdqu ymm4, [{s} + 128]",
            "vmovdqu ymm5, [{s} + 160]",
            "vmovdqu ymm6, [{s} + 192]",
            "vmovdqu ymm7, [{s} + 224]",
            "vmovdqu ymm8, [{s} + 256]",
            "vmovdqu ymm9, [{s} + 288]",
            "vmovdqu ymm10, [{s} + 320]",
            "vmovdqu ymm11, [{s} + 352]",
            "vmovdqu ymm12, [{s} + 384]",
            "vmovdqu ymm13, [{s} + 416]",
            "vmovdqu ymm14, [{s} + 448]",
            "vmovdqu ymm15, [{s} + 480]",
            "2:",
            "dec {n}",
            "jnz 2b",
            "vmovdqu [{d}], ymm0",
            "vmovdqu [{d} + 32], ymm1",
            "vmovdqu [{d} + 64], ymm2",
            "vmovdqu [{d} + 96], ymm3",
            "vmovdqu [{d} + 128], ymm4",
            "vmovdqu [{d} + 160], ymm5",
            "vmovdqu [{d} + 192], ymm6",
            "vmovdqu [{d} + 224], ymm7",
            "vmovdqu [{d} + 256], ymm8",
            "vmovdqu [{d} + 288], ymm9",
            "vmovdqu [{d} + 320], ymm10",
            "vmovdqu [{d} + 352], ymm11",
            "vmovdqu [{d} + 384], ymm12",
            "vmovdqu [{d} + 416], ymm13",
            "vmovdqu [{d} + 448], ymm14",
            "vmovdqu [{d} + 480], ymm15",
            s = in(reg) src.as_ptr(),
            d = in(reg) dst.as_mut_ptr(),
            n = inout(reg) spins => _,
            out("ymm0") _, out("ymm1") _, out("ymm2") _, out("ymm3") _,
            out("ymm4") _, out("ymm5") _, out("ymm6") _, out("ymm7") _,
            out("ymm8") _, out("ymm9") _, out("ymm10") _, out("ymm11") _,
            out("ymm12") _, out("ymm13") _, out("ymm14") _, out("ymm15") _,
            options(nostack),
        );
    }
    src == dst
}
