//! Polled, output-only driver for the 16550 UART at COM1 (I/O port 0x3F8).
//! QEMU connects this port to the host via `-serial`.
//!
//! Single CPU. The writer holds no state and takes no lock, so it is safe to
//! call from exception handlers; output from an exception that interrupts a
//! half-written line may interleave with it.

use core::fmt;
use x86_64::instructions::port::Port;

const COM1: u16 = 0x3F8;
const DATA: u16 = COM1; // DLAB=0: transmit holding register; DLAB=1: divisor low
const INT_ENABLE: u16 = COM1 + 1; // DLAB=1: divisor high
const FIFO_CTRL: u16 = COM1 + 2;
const LINE_CTRL: u16 = COM1 + 3;
const MODEM_CTRL: u16 = COM1 + 4;
const LINE_STATUS: u16 = COM1 + 5;
const LSR_THR_EMPTY: u8 = 1 << 5;

/// Upper bound on status polls per byte, so a missing or stuck UART cannot
/// hang the kernel. On timeout the byte is written anyway.
const MAX_TX_POLLS: u32 = 100_000;

fn outb(port: u16, value: u8) {
    // SAFETY: only called with COM1 register addresses, which this module
    // owns exclusively. UART register writes do not affect memory safety.
    unsafe { Port::<u8>::new(port).write(value) }
}

fn inb(port: u16) -> u8 {
    // SAFETY: only called for the COM1 line-status register, whose read has
    // no side effects.
    unsafe { Port::<u8>::new(port).read() }
}

/// Programs COM1 for 38400 baud, 8N1, FIFOs on, UART interrupts off.
pub fn init() {
    outb(INT_ENABLE, 0x00); // no UART interrupts
    outb(LINE_CTRL, 0x80); // DLAB on
    outb(DATA, 0x03); // divisor 3 => 38400 baud
    outb(INT_ENABLE, 0x00);
    outb(LINE_CTRL, 0x03); // DLAB off, 8 data bits, no parity, 1 stop
    outb(FIFO_CTRL, 0xC7); // enable + clear FIFOs, 14-byte threshold
    outb(MODEM_CTRL, 0x03); // DTR + RTS
}

fn write_byte(byte: u8) {
    let mut polls = 0;
    while inb(LINE_STATUS) & LSR_THR_EMPTY == 0 && polls < MAX_TX_POLLS {
        polls += 1;
        core::hint::spin_loop();
    }
    outb(DATA, byte);
}

pub struct Serial;

impl fmt::Write for Serial {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for byte in s.bytes() {
            if byte == b'\n' {
                write_byte(b'\r');
            }
            write_byte(byte);
        }
        Ok(())
    }
}

#[doc(hidden)]
pub fn _print(args: fmt::Arguments) {
    use fmt::Write;
    // Serial::write_str never fails, so the result carries no information.
    let _ = Serial.write_fmt(args);
}

#[macro_export]
macro_rules! serial_println {
    ($($arg:tt)*) => {{
        $crate::serial::_print(format_args!($($arg)*));
        $crate::serial::_print(format_args!("\n"));
    }};
}
