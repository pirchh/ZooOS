//! PS/2 mouse (docs/d1-design.md §1): the i8042's auxiliary port, standard
//! 3-byte packets, IRQ12. Packets become events in a lock-free ring (one
//! producer: the IRQ handlers; one consumer: `mouse_read` for the display
//! owner), like the keyboard queue.

use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use spin::Mutex;
use x86_64::instructions::port::Port;

const DATA_PORT: u16 = 0x60;
const STATUS_PORT: u16 = 0x64;
const STATUS_OUTPUT_FULL: u8 = 1;
const STATUS_INPUT_FULL: u8 = 2;
/// The byte in the output buffer came from the auxiliary (mouse) port.
pub const STATUS_AUX: u8 = 0x20;
/// Bounded waits on the controller.
const SPINS: u32 = 100_000;

const CAPACITY: usize = 128;
/// Events packed as `buttons << 32 | dy as u16 << 16 | dx as u16`.
static QUEUE: [AtomicU64; CAPACITY] = [const { AtomicU64::new(0) }; CAPACITY];
static HEAD: AtomicUsize = AtomicUsize::new(0);
static TAIL: AtomicUsize = AtomicUsize::new(0);
static DROPPED: AtomicU64 = AtomicU64::new(0);
/// When each queued event arrived (time-stamp counter), and how long events
/// waited before `mouse_read` took them: count, sum and maximum in TSC
/// cycles (docs/u1b-design.md).
static STAMP: [AtomicU64; CAPACITY] = [const { AtomicU64::new(0) }; CAPACITY];
static WAIT_COUNT: AtomicU64 = AtomicU64::new(0);
static WAIT_SUM: AtomicU64 = AtomicU64::new(0);
static WAIT_MAX: AtomicU64 = AtomicU64::new(0);

pub fn tsc() -> u64 {
    // SAFETY: `rdtsc` only reads the time-stamp counter.
    unsafe { core::arch::x86_64::_rdtsc() }
}

/// Bytes of the packet being assembled (IRQ context only).
static PACKET: Mutex<([u8; 3], usize)> = Mutex::new(([0; 3], 0));

fn wait_input_empty() -> bool {
    for _ in 0..SPINS {
        // SAFETY: reading the i8042 status register has no side effects.
        if unsafe { Port::<u8>::new(STATUS_PORT).read() } & STATUS_INPUT_FULL == 0 {
            return true;
        }
        core::hint::spin_loop();
    }
    false
}

fn read_data() -> Option<u8> {
    for _ in 0..SPINS {
        // SAFETY: status read, then the data port only when a byte waits.
        unsafe {
            if Port::<u8>::new(STATUS_PORT).read() & STATUS_OUTPUT_FULL != 0 {
                return Some(Port::<u8>::new(DATA_PORT).read());
            }
        }
        core::hint::spin_loop();
    }
    None
}

fn command(byte: u8) -> bool {
    // SAFETY: an i8042 controller command, written when the controller is
    // ready for input.
    wait_input_empty()
        && unsafe {
            Port::<u8>::new(STATUS_PORT).write(byte);
            true
        }
}

fn write_data(byte: u8) -> bool {
    // SAFETY: a data byte for the controller or device, written when ready.
    wait_input_empty()
        && unsafe {
            Port::<u8>::new(DATA_PORT).write(byte);
            true
        }
}

/// Sends `byte` to the mouse and expects its ACK (0xFA).
fn mouse_command(byte: u8) -> bool {
    command(0xD4) && write_data(byte) && read_data() == Some(0xFA)
}

/// Enables the auxiliary port, its interrupt, and data reporting. Call with
/// interrupts disabled, before IRQ12 is unmasked. False if any step fails
/// (then no mouse).
pub fn init() -> bool {
    if !command(0xA8) {
        return false; // enable the auxiliary port
    }
    if !command(0x20) {
        return false; // read the controller configuration byte
    }
    let Some(config) = read_data() else {
        return false;
    };
    // Bit 1: IRQ12 enabled; bit 5: auxiliary clock disabled (clear it).
    let config = (config | 0x02) & !0x20;
    if !(command(0x60) && write_data(config)) {
        return false;
    }
    // Defaults (100 samples/s, 1:1 scaling), then start reporting.
    mouse_command(0xF6) && mouse_command(0xF4)
}

/// One byte from the auxiliary port (IRQ context).
pub fn on_byte(byte: u8) {
    let mut p = PACKET.lock();
    let (bytes, n) = &mut *p;
    // The first byte always has bit 3 set; drop bytes until it lines up.
    if *n == 0 && byte & 0x08 == 0 {
        return;
    }
    bytes[*n] = byte;
    *n += 1;
    if *n < 3 {
        return;
    }
    *n = 0;
    let flags = bytes[0];
    if flags & 0xC0 != 0 {
        return; // overflow: discard
    }
    // 9-bit two's complement deltas: sign bits in the first byte.
    let dx = i16::from(bytes[1]) - if flags & 0x10 != 0 { 256 } else { 0 };
    let dy = i16::from(bytes[2]) - if flags & 0x20 != 0 { 256 } else { 0 };
    push_event(dx, dy, flags & 0x07);
}

/// Queues one event (`dy` positive upwards). Interrupt context only: the
/// PS/2 handlers and the timer tick for USB mice (docs/h1b-design.md),
/// which never nest.
pub fn push_event(dx: i16, dy: i16, buttons: u8) {
    push_event_at(dx, dy, buttons, tsc());
}

/// Like `push_event`, for an event that arrived at time-stamp `stamp`
/// (the USB controller's interrupt, docs/u1c-design.md).
pub fn push_event_at(dx: i16, dy: i16, buttons: u8, stamp: u64) {
    let event = (u64::from(buttons) << 32) | (u64::from(dy as u16) << 16) | u64::from(dx as u16);
    let head = HEAD.load(Ordering::Relaxed);
    if head.wrapping_sub(TAIL.load(Ordering::Acquire)) >= CAPACITY {
        DROPPED.fetch_add(1, Ordering::Relaxed);
        return;
    }
    QUEUE[head % CAPACITY].store(event, Ordering::Relaxed);
    STAMP[head % CAPACITY].store(stamp, Ordering::Relaxed);
    HEAD.store(head.wrapping_add(1), Ordering::Release);
    crate::process::kernel_has_input();
}

/// IRQ12: read the byte if it is the mouse's.
pub fn on_interrupt() {
    // SAFETY: status read, then the data byte that raised the interrupt.
    unsafe {
        let status = Port::<u8>::new(STATUS_PORT).read();
        if status & STATUS_OUTPUT_FULL != 0 && status & STATUS_AUX != 0 {
            on_byte(Port::<u8>::new(DATA_PORT).read());
        }
    }
}

/// (dx, dy, buttons) of the oldest event. `dy` is positive upwards, as the
/// mouse reports it.
pub fn pop() -> Option<(i16, i16, u8)> {
    let tail = TAIL.load(Ordering::Relaxed);
    if tail == HEAD.load(Ordering::Acquire) {
        return None;
    }
    let e = QUEUE[tail % CAPACITY].load(Ordering::Relaxed);
    let waited = tsc().wrapping_sub(STAMP[tail % CAPACITY].load(Ordering::Relaxed));
    WAIT_COUNT.fetch_add(1, Ordering::Relaxed);
    WAIT_SUM.fetch_add(waited, Ordering::Relaxed);
    WAIT_MAX.fetch_max(waited, Ordering::Relaxed);
    TAIL.store(tail.wrapping_add(1), Ordering::Release);
    Some((e as u16 as i16, (e >> 16) as u16 as i16, (e >> 32) as u8))
}

/// True if an event is waiting.
pub fn pending() -> bool {
    TAIL.load(Ordering::Relaxed) != HEAD.load(Ordering::Acquire)
}

/// (events, total wait, longest wait) in TSC cycles since the last call,
/// from arrival to `mouse_read`; the counters restart.
pub fn take_wait_stats() -> (u64, u64, u64) {
    (
        WAIT_COUNT.swap(0, Ordering::Relaxed),
        WAIT_SUM.swap(0, Ordering::Relaxed),
        WAIT_MAX.swap(0, Ordering::Relaxed),
    )
}

/// Drops queued events (when nobody owns the display).
pub fn clear() {
    while pop().is_some() {}
}
