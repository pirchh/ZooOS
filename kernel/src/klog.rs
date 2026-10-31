//! The kernel log buffer (docs/s2-design.md): every line a program logs is
//! printed on the serial console as before and also kept here, bounded,
//! until the log service reads it.

use crate::interrupts;
use alloc::collections::VecDeque;
use alloc::string::String;
use core::fmt::Write as _;
use spin::Mutex;

/// Lines kept before the oldest are dropped.
const CAPACITY: usize = 128;

struct Buffer {
    lines: VecDeque<String>,
    /// Lines dropped because nobody read them in time, since the last read.
    dropped: u64,
}

static BUFFER: Mutex<Buffer> = Mutex::new(Buffer {
    lines: VecDeque::new(),
    dropped: 0,
});

/// Records one logged line as `<tick> <pid> <name> <text>`. Called with
/// interrupts disabled (from a system call).
pub fn push(pid: u64, name: &str, text: &str) {
    let mut line = String::new();
    let _ = write!(line, "{} {} {} {}", interrupts::ticks(), pid, name, text);
    let mut b = BUFFER.lock();
    if b.lines.len() == CAPACITY {
        b.lines.pop_front();
        b.dropped += 1;
    }
    b.lines.push_back(line);
}

/// The oldest line, preceded by a `<tick> 0 kernel N lines dropped` note
/// when lines were lost since the last read.
pub fn pop() -> Option<String> {
    let mut b = BUFFER.lock();
    if b.dropped > 0 {
        let mut note = String::new();
        let _ = write!(
            note,
            "{} 0 kernel {} earlier lines dropped",
            interrupts::ticks(),
            b.dropped
        );
        b.dropped = 0;
        return Some(note);
    }
    b.lines.pop_front()
}
