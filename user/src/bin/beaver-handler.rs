//! BeaverHandler: the log service (beavers handle logs; docs/s1-design.md
//! §4). Keeps the last 64
//! lines it is sent, with the tick they arrived at, and hands them out on
//! request. Supervised by Zookeeper: answers `ping` with `pong`.
//!
//! Requests: `log <text>`, `dump` (lines, then `end`), `ping`.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::collections::VecDeque;
use alloc::string::String;
use core::fmt::Write as _;
use zoo_rt::app::send_wait;
use zoo_rt::{Line, recv, ticks};

zoo_rt::entry!(main);

const KEEP: usize = 64;

fn main(chan: u64) -> u8 {
    let mut lines: VecDeque<(u64, String)> = VecDeque::new();
    let mut buf = [0u8; zoo_abi::MAX_MESSAGE];
    loop {
        let Ok(n) = recv(chan, &mut buf) else {
            return 0; // supervisor gone
        };
        let msg = core::str::from_utf8(&buf[..n]).unwrap_or("");
        if msg == "ping" {
            if send_wait(chan, b"pong").is_err() {
                return 0;
            }
        } else if let Some(text) = msg.strip_prefix("log ") {
            if lines.len() == KEEP {
                lines.pop_front();
            }
            lines.push_back((ticks(), String::from(text)));
        } else if msg == "dump" {
            for (t, text) in &lines {
                let mut l = Line::new();
                let _ = write!(l, "[{:>6}.{:02}] {}", t / 100, t % 100, text);
                if send_wait(chan, l.as_str().as_bytes()).is_err() {
                    return 0;
                }
            }
            if send_wait(chan, b"end").is_err() {
                return 0;
            }
        }
    }
}
