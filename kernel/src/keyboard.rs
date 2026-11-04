//! PS/2 keyboard input queue (docs/console-design.md §2).
//!
//! Interrupt handlers (IRQ1, and the timer tick for USB keyboards) are the
//! producers, one at a time, and the shell loop the only consumer, so a
//! ring of atomics is enough: no lock, no allocation, safe to call from
//! interrupt context.

use core::sync::atomic::{AtomicU8, AtomicU64, AtomicUsize, Ordering};
use x86_64::instructions::port::Port;

const CAPACITY: usize = 256;
const DATA_PORT: u16 = 0x60;
const STATUS_PORT: u16 = 0x64;
const STATUS_OUTPUT_FULL: u8 = 1;

static QUEUE: [AtomicU8; CAPACITY] = [const { AtomicU8::new(0) }; CAPACITY];
/// Next slot to write (producer only).
static HEAD: AtomicUsize = AtomicUsize::new(0);
/// Next slot to read (consumer only).
static TAIL: AtomicUsize = AtomicUsize::new(0);
static RECEIVED: AtomicU64 = AtomicU64::new(0);
static DROPPED: AtomicU64 = AtomicU64::new(0);

/// Reads one scancode byte from the controller and queues it. Called only
/// from the IRQ1 handler.
pub fn on_interrupt() {
    // SAFETY: port 0x60 is the i8042 data port; reading it takes the byte
    // that raised IRQ1, which is exactly what this handler must do.
    let (status, byte) = unsafe {
        (
            Port::<u8>::new(STATUS_PORT).read(),
            Port::<u8>::new(DATA_PORT).read(),
        )
    };
    // With the mouse enabled both devices share the data port; a mouse byte
    // can be pending when IRQ1 is serviced (docs/d1-design.md).
    if status & crate::mouse::STATUS_AUX != 0 {
        crate::mouse::on_byte(byte);
        return;
    }
    push(byte);
}

/// Queues one set-1 scancode byte. Called from interrupt handlers only
/// (IRQ1, and the timer tick for USB keyboards, docs/h1b-design.md); they
/// never nest, so there is still one producer at a time.
pub fn push(byte: u8) {
    RECEIVED.fetch_add(1, Ordering::Relaxed);
    // F12 (set 1 make code): a task dump on the next tick, for hangs.
    if byte == 0x58 {
        crate::process::request_dump();
    }
    let head = HEAD.load(Ordering::Relaxed);
    let tail = TAIL.load(Ordering::Acquire);
    if head.wrapping_sub(tail) >= CAPACITY {
        DROPPED.fetch_add(1, Ordering::Relaxed);
        return;
    }
    QUEUE[head % CAPACITY].store(byte, Ordering::Relaxed);
    HEAD.store(head.wrapping_add(1), Ordering::Release);
    crate::process::kernel_has_input();
}

/// True if a byte is waiting.
pub fn pending() -> bool {
    TAIL.load(Ordering::Relaxed) != HEAD.load(Ordering::Acquire)
}

/// Takes the oldest queued byte. Called only from the shell loop.
pub fn pop() -> Option<u8> {
    let tail = TAIL.load(Ordering::Relaxed);
    let head = HEAD.load(Ordering::Acquire);
    if tail == head {
        return None;
    }
    let byte = QUEUE[tail % CAPACITY].load(Ordering::Relaxed);
    TAIL.store(tail.wrapping_add(1), Ordering::Release);
    Some(byte)
}

/// (bytes received, bytes dropped because the queue was full)
pub fn stats() -> (u64, u64) {
    (
        RECEIVED.load(Ordering::Relaxed),
        DROPPED.load(Ordering::Relaxed),
    )
}

/// Discards bytes already waiting in the controller (e.g. left by the
/// firmware), so the first IRQ1 is for a fresh key. Bounded.
pub fn drain_controller() -> usize {
    let mut drained = 0;
    for _ in 0..64 {
        // SAFETY: reading the i8042 status register has no side effects;
        // reading the data port only when the status says a byte is waiting.
        let full = unsafe { Port::<u8>::new(STATUS_PORT).read() } & STATUS_OUTPUT_FULL != 0;
        if !full {
            break;
        }
        // SAFETY: as above; this consumes the waiting byte.
        let _ = unsafe { Port::<u8>::new(DATA_PORT).read() };
        drained += 1;
    }
    drained
}

/// Asks the i8042 to pulse the CPU reset line. Returns if nothing happens.
pub fn request_reset() {
    // SAFETY: command 0xFE to the i8042 command port resets the machine,
    // which is the caller's intent; it does not touch memory.
    unsafe { Port::<u8>::new(STATUS_PORT).write(0xFE) };
}
