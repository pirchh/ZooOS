//! Processor idle and halt helpers. The kernel has no inline assembly of its
//! own; these wrap instructions provided by the `x86_64` crate.

use x86_64::instructions::{hlt, interrupts};

/// Disables interrupts and halts permanently. Used by the panic handler and
/// fatal exception handlers, so nothing runs after a fatal report.
pub fn halt_forever() -> ! {
    interrupts::disable();
    loop {
        // With interrupts off only an NMI can wake the CPU; halt again.
        hlt();
    }
}

/// Idles with interrupts enabled, so the timer keeps running.
pub fn idle_forever() -> ! {
    loop {
        // `sti; hlt` as one pair, so an interrupt cannot slip in between and
        // leave the CPU halted until the next one.
        interrupts::enable_and_hlt();
    }
}
