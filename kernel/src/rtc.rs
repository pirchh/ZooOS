//! The CMOS real-time clock, read once at boot (docs/posix5b1-design.md
//! §2). Wall-clock time afterwards is the boot reading plus timer ticks.

use crate::interrupts;
use core::sync::atomic::{AtomicU64, Ordering};
use x86_64::instructions::port::Port;

const INDEX_PORT: u16 = 0x70;
const DATA_PORT: u16 = 0x71;
/// Bounded wait for the clock to finish an update.
const SPINS: u32 = 1_000_000;
const NS_PER_TICK: u64 = 1_000_000_000 / interrupts::TIMER_HZ as u64;

/// Unix seconds at boot (0: no clock), and the tick it was read at.
static BOOT_UNIX: AtomicU64 = AtomicU64::new(0);
static BOOT_TICK: AtomicU64 = AtomicU64::new(0);

fn read(reg: u8) -> u8 {
    // SAFETY: selects a CMOS register (bit 7 clear leaves NMIs enabled)
    // and reads it; CMOS reads have no side effects, and only this module
    // uses these ports.
    unsafe {
        Port::<u8>::new(INDEX_PORT).write(reg);
        Port::<u8>::new(DATA_PORT).read()
    }
}

/// Seconds, minutes, hours, day, month, year and status register B, read
/// when no update is in progress.
fn snapshot() -> Option<[u8; 7]> {
    for _ in 0..SPINS {
        if read(0x0A) & 0x80 == 0 {
            return Some([
                read(0x00),
                read(0x02),
                read(0x04),
                read(0x07),
                read(0x08),
                read(0x09),
                read(0x0B),
            ]);
        }
        core::hint::spin_loop();
    }
    None
}

/// Days from 1970-01-01 to the given civil date (proleptic Gregorian).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Reads the clock; returns Unix seconds, or `None` if it never settles or
/// reads as an impossible date.
pub fn init() -> Option<u64> {
    let mut a = snapshot()?;
    // Two equal reads in a row: no update happened in between.
    for _ in 0..10 {
        let b = snapshot()?;
        if a == b {
            break;
        }
        a = b;
    }
    let status_b = a[6];
    let bcd = status_b & 0x04 == 0;
    let dec = |v: u8| -> i64 {
        if bcd {
            i64::from((v & 0x0F) + (v >> 4) * 10)
        } else {
            i64::from(v)
        }
    };
    let (sec, min) = (dec(a[0]), dec(a[1]));
    let mut hour = dec(a[2] & 0x7F);
    if status_b & 0x02 == 0 {
        // 12-hour form: bit 7 of the hour is PM.
        hour %= 12;
        if a[2] & 0x80 != 0 {
            hour += 12;
        }
    }
    let (day, month, year) = (dec(a[3]), dec(a[4]), 2000 + dec(a[5]));
    if sec > 59 || min > 59 || hour > 23 || !(1..=31).contains(&day) || !(1..=12).contains(&month) {
        return None;
    }
    let unix = days_from_civil(year, month, day) * 86_400 + hour * 3600 + min * 60 + sec;
    let unix = u64::try_from(unix).ok()?;
    BOOT_TICK.store(interrupts::ticks(), Ordering::Relaxed);
    BOOT_UNIX.store(unix, Ordering::Relaxed);
    Some(unix)
}

/// Nanoseconds since boot (timer-tick resolution).
pub fn monotonic_ns() -> u64 {
    interrupts::ticks() * NS_PER_TICK
}

/// Nanoseconds since 1970-01-01 UTC, or `None` without a clock.
pub fn realtime_ns() -> Option<u64> {
    let boot = BOOT_UNIX.load(Ordering::Relaxed);
    if boot == 0 {
        return None;
    }
    let since = interrupts::ticks() - BOOT_TICK.load(Ordering::Relaxed);
    Some(boot * 1_000_000_000 + since * NS_PER_TICK)
}
