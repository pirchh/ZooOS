//! BeaverHandler: the log service (beavers handle logs; docs/s2-design.md).
//! Drains the kernel log buffer (every line any program logs) through the
//! log-reader handle Zookeeper gives it, keeps the last 256 lines, and hands
//! them out on request. Supervised by Zookeeper: answers `ping` with `pong`.
//!
//! Requests: `dump [text]` (matching lines, at most 30, then `end`), `ping`.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::collections::VecDeque;
use alloc::string::String;
use core::fmt::Write as _;
use zoo_rt::app::send_wait;
use zoo_rt::{Error, Line, log_read, try_recv};

zoo_rt::entry2!(main);

const KEEP: usize = 256;
const DUMP_MAX: usize = 30;

/// `<tick> <pid> <name> <text>` from the kernel, shown as
/// `[  secs.cc] [pid:name] text`.
fn format(raw: &str) -> String {
    let mut parts = raw.splitn(4, ' ');
    let (Some(tick), Some(pid), Some(name)) = (parts.next(), parts.next(), parts.next()) else {
        return String::from(raw);
    };
    let text = parts.next().unwrap_or("");
    let t: u64 = tick.parse().unwrap_or(0);
    let mut s = String::new();
    let _ = write!(
        s,
        "[{:>6}.{:02}] [{}:{}] {}",
        t / 100,
        t % 100,
        pid,
        name,
        text
    );
    s
}

fn main(chan: u64, reader: u64) -> u8 {
    let mut lines: VecDeque<String> = VecDeque::new();
    let mut buf = [0u8; zoo_abi::MAX_MESSAGE];
    loop {
        let mut busy = false;
        // Drain the kernel log buffer.
        if reader != zoo_rt::NO_HANDLE {
            while let Ok(n) = log_read(reader, &mut buf) {
                busy = true;
                if lines.len() == KEEP {
                    lines.pop_front();
                }
                lines.push_back(format(core::str::from_utf8(&buf[..n]).unwrap_or("?")));
            }
        }
        match try_recv(chan, &mut buf) {
            Ok(n) => {
                busy = true;
                let msg = String::from(core::str::from_utf8(&buf[..n]).unwrap_or(""));
                if msg == "ping" {
                    if send_wait(chan, b"pong").is_err() {
                        return 0;
                    }
                } else if let Some(rest) = msg.strip_prefix("dump") {
                    let filter = rest.trim();
                    let matching: alloc::vec::Vec<&String> =
                        lines.iter().filter(|l| l.contains(filter)).collect();
                    let skip = matching.len().saturating_sub(DUMP_MAX);
                    if skip > 0 {
                        let mut l = Line::new();
                        let _ = write!(l, "({} earlier matching lines not shown)", skip);
                        if send_wait(chan, l.as_str().as_bytes()).is_err() {
                            return 0;
                        }
                    }
                    for line in &matching[skip..] {
                        let mut l = Line::new();
                        let _ = l.write_str(line);
                        if send_wait(chan, l.as_str().as_bytes()).is_err() {
                            return 0;
                        }
                    }
                    if send_wait(chan, b"end").is_err() {
                        return 0;
                    }
                }
            }
            Err(Error::Again) => {}
            Err(_) => return 0, // supervisor gone
        }
        if !busy {
            zoo_rt::sleep(5);
        }
    }
}
