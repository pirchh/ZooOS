//! Minimal processor operations. All inline assembly in the kernel lives here.

use core::arch::asm;

/// Writes one byte to an x86 I/O port.
///
/// # Safety
/// The caller must ensure `port` is a device register for which writing
/// `value` has no effect on memory safety (e.g. a UART data/control register
/// owned by the caller). Port I/O is only permitted at CPL 0, which holds for
/// this kernel.
pub unsafe fn outb(port: u16, value: u8) {
    // SAFETY: forwarded from the caller's contract. The instruction touches no
    // Rust-visible memory and does not use the stack.
    unsafe {
        asm!("out dx, al", in("dx") port, in("al") value, options(nomem, nostack, preserves_flags));
    }
}

/// Reads one byte from an x86 I/O port.
///
/// # Safety
/// Same as [`outb`]: `port` must be a register whose read side effects are
/// acceptable to the caller (reading a 16550 status register has none).
pub unsafe fn inb(port: u16) -> u8 {
    let value: u8;
    // SAFETY: forwarded from the caller's contract; no memory or stack access.
    unsafe {
        asm!("in al, dx", out("al") value, in("dx") port, options(nomem, nostack, preserves_flags));
    }
    value
}

/// Disables interrupts and halts the CPU permanently.
///
/// Interrupts are never enabled in M1, so `hlt` should not return; the loop
/// covers NMIs and spurious wakeups.
pub fn halt_forever() -> ! {
    loop {
        // SAFETY: `cli` and `hlt` only change interrupt state and pause the
        // CPU; they access no memory. The kernel is at CPL 0.
        unsafe {
            asm!("cli", "hlt", options(nomem, nostack));
        }
    }
}
