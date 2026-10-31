//! Output-only driver for the 16550 UART at COM1 (I/O port 0x3F8). QEMU
//! connects this port to the host via `-serial`.
//!
//! Output is buffered (docs/f2-design.md): text goes into a 64 KiB ring and
//! out to the UART a FIFO's worth (16 bytes) at a time, whenever the UART
//! has room, without waiting for it. What is left goes out on the next
//! write, timer tick or idle moment. `flush` sends everything, waiting, and
//! switches to direct output; fatal paths call it so nothing is lost.
//!
//! Single CPU. A writer that finds the ring locked (an exception during a
//! write) writes directly; its text may then appear out of order.

use core::fmt;
use core::sync::atomic::{AtomicBool, Ordering};
use spin::Mutex;
use x86_64::instructions::interrupts::without_interrupts;
use x86_64::instructions::port::Port;

const COM1: u16 = 0x3F8;
const DATA: u16 = COM1; // DLAB=0: transmit holding register; DLAB=1: divisor low
const INT_ENABLE: u16 = COM1 + 1; // DLAB=1: divisor high
const FIFO_CTRL: u16 = COM1 + 2;
const LINE_CTRL: u16 = COM1 + 3;
const MODEM_CTRL: u16 = COM1 + 4;
const LINE_STATUS: u16 = COM1 + 5;
const SCRATCH: u16 = COM1 + 7;
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
    // SAFETY: only called for the COM1 line-status and scratch registers,
    // whose reads have no side effects.
    unsafe { Port::<u8>::new(port).read() }
}

/// False once `init` finds no UART at COM1 (most laptops): output is then
/// dropped without touching the port (docs/f2-design.md).
static PRESENT: AtomicBool = AtomicBool::new(true);

/// True if a UART answers at COM1: its scratch register keeps a value.
fn uart_present() -> bool {
    [0xA5u8, 0x5A].iter().all(|&v| {
        outb(SCRATCH, v);
        inb(SCRATCH) == v
    })
}

/// Programs COM1 for 38400 baud, 8N1, FIFOs on, UART interrupts off, or
/// turns serial output off if there is no UART.
pub fn init() {
    if !uart_present() {
        PRESENT.store(false, Ordering::Relaxed);
        return;
    }
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

/// Bytes the 16550's transmit FIFO takes once it reports empty.
const FIFO_DEPTH: usize = 16;
const TX_SIZE: usize = 64 * 1024;

/// Text waiting for the UART: a ring of `len` bytes from `head`.
struct Tx {
    buf: [u8; TX_SIZE],
    head: usize,
    len: usize,
}

static TX: Mutex<Tx> = Mutex::new(Tx {
    buf: [0; TX_SIZE],
    head: 0,
    len: 0,
});

/// Set by `flush`: from then on every byte is written directly.
static DIRECT: AtomicBool = AtomicBool::new(false);

/// Writes `bytes` (at most `FIFO_DEPTH`) to the data register with one
/// string instruction.
fn out_burst(bytes: &[u8]) {
    // SAFETY: `rep outsb` reads `bytes.len()` bytes from `bytes` (DF is
    // clear, as the ABI guarantees) and writes them to the COM1 data port,
    // which this module owns; it touches no other memory.
    unsafe {
        core::arch::asm!(
            "rep outsb",
            in("dx") DATA,
            inout("rsi") bytes.as_ptr() => _,
            inout("rcx") bytes.len() => _,
            options(nostack, preserves_flags, readonly)
        );
    }
}

impl Tx {
    fn push(&mut self, byte: u8) {
        if self.len == TX_SIZE {
            // Full: make room the slow way rather than lose output.
            write_byte(self.buf[self.head]);
            self.head = (self.head + 1) % TX_SIZE;
            self.len -= 1;
        }
        self.buf[(self.head + self.len) % TX_SIZE] = byte;
        self.len += 1;
    }

    /// Sends while the UART has room, a FIFO's worth per check; never waits.
    fn pump(&mut self) {
        while self.len > 0 && inb(LINE_STATUS) & LSR_THR_EMPTY != 0 {
            let n = self.len.min(FIFO_DEPTH).min(TX_SIZE - self.head);
            out_burst(&self.buf[self.head..self.head + n]);
            self.head = (self.head + n) % TX_SIZE;
            self.len -= n;
        }
    }

    /// Sends everything, waiting for the UART (bounded per byte).
    fn drain(&mut self) {
        while self.len > 0 {
            write_byte(self.buf[self.head]);
            self.head = (self.head + 1) % TX_SIZE;
            self.len -= 1;
        }
    }
}

/// Sends buffered output while the UART has room. Called on timer ticks
/// and when idle.
pub fn pump() {
    if !PRESENT.load(Ordering::Relaxed) {
        return;
    }
    without_interrupts(|| {
        if let Some(mut tx) = TX.try_lock() {
            tx.pump();
        }
    });
}

/// Sends all buffered output, waiting, and writes directly from then on.
/// For fatal paths, before halting or resetting.
pub fn flush() {
    if !PRESENT.load(Ordering::Relaxed) {
        return;
    }
    DIRECT.store(true, Ordering::Relaxed);
    if let Some(mut tx) = TX.try_lock() {
        tx.drain();
    }
}

pub struct Serial;

impl fmt::Write for Serial {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        if !PRESENT.load(Ordering::Relaxed) {
            return Ok(());
        }
        let direct = |s: &str| {
            for byte in s.bytes() {
                if byte == b'\n' {
                    write_byte(b'\r');
                }
                write_byte(byte);
            }
        };
        if DIRECT.load(Ordering::Relaxed) {
            direct(s);
            return Ok(());
        }
        without_interrupts(|| match TX.try_lock() {
            Some(mut tx) => {
                for byte in s.bytes() {
                    if byte == b'\n' {
                        tx.push(b'\r');
                    }
                    tx.push(byte);
                }
                tx.pump();
            }
            None => direct(s),
        });
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
