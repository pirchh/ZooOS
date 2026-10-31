//! Processor idle and halt helpers. The kernel has no inline assembly of its
//! own; these wrap instructions provided by the `x86_64` crate.

use x86_64::instructions::{hlt, interrupts};

/// Disables interrupts and halts permanently. Used by the panic handler and
/// fatal exception handlers, so nothing runs after a fatal report.
pub fn halt_forever() -> ! {
    interrupts::disable();
    // The report must reach the screen and the serial port before the CPU
    // stops.
    crate::console::render();
    crate::serial::flush();
    loop {
        // With interrupts off only an NMI can wake the CPU; halt again.
        hlt();
    }
}
