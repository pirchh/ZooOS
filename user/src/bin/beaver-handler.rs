//! BeaverHandler: the log service (beavers handle logs; docs/s2-design.md).
//! Drains the kernel log buffer (every line any program logs) through the
//! log-reader handle Zookeeper gives it, keeps the last 256 lines, and hands
//! them out on request. Supervised by Zookeeper: answers `ping` with `pong`.
//! Other services can connect to it by name (docs/s3-design.md): Zookeeper
//! hands it each new client channel as a handle message (tag 1).
//!
//! Requests (from Zookeeper or a client): `dump [text]` (matching lines, at
//! most 30, then `end`), `ping`.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write as _;
use zoo_rt::app::send_wait;
use zoo_rt::{Error, Handle, Line, close, log_read, try_recv, try_recv_handle};

zoo_rt::entry2!(main);

const KEEP: usize = 256;
const DUMP_MAX: usize = 30;
/// Clients served at once.
const MAX_CLIENTS: usize = 8;

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

/// Answers one request on `chan`; false if the peer is gone.
fn answer(chan: Handle, msg: &str, lines: &VecDeque<String>) -> bool {
    if msg == "ping" {
        return send_wait(chan, b"pong").is_ok();
    }
    if let Some(rest) = msg.strip_prefix("dump") {
        let filter = rest.trim();
        let matching: Vec<&String> = lines.iter().filter(|l| l.contains(filter)).collect();
        let skip = matching.len().saturating_sub(DUMP_MAX);
        if skip > 0 {
            let mut l = Line::new();
            let _ = write!(l, "({} earlier matching lines not shown)", skip);
            if send_wait(chan, l.as_str().as_bytes()).is_err() {
                return false;
            }
        }
        for line in &matching[skip..] {
            let mut l = Line::new();
            let _ = l.write_str(line);
            if send_wait(chan, l.as_str().as_bytes()).is_err() {
                return false;
            }
        }
        return send_wait(chan, b"end").is_ok();
    }
    true
}

fn main(chan: u64, reader: u64) -> u8 {
    let mut lines: VecDeque<String> = VecDeque::new();
    let mut clients: Vec<Handle> = Vec::new();
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
        // The supervisor channel: requests, and new clients as handles.
        match try_recv(chan, &mut buf) {
            Ok(n) => {
                busy = true;
                let msg = String::from(core::str::from_utf8(&buf[..n]).unwrap_or(""));
                if !answer(chan, &msg, &lines) {
                    return 0;
                }
            }
            Err(Error::WrongKind) => {
                busy = true;
                if let Ok((client, _tag)) = try_recv_handle(chan) {
                    if clients.len() < MAX_CLIENTS {
                        clients.push(client);
                    } else {
                        let _ = close(client);
                    }
                }
            }
            Err(Error::Again) => {}
            Err(_) => return 0, // supervisor gone
        }
        // Clients.
        let mut k = 0;
        while k < clients.len() {
            match try_recv(clients[k], &mut buf) {
                Ok(n) => {
                    busy = true;
                    let msg = String::from(core::str::from_utf8(&buf[..n]).unwrap_or(""));
                    if !answer(clients[k], &msg, &lines) {
                        let _ = close(clients.swap_remove(k));
                        continue;
                    }
                }
                Err(Error::Again) => {}
                Err(_) => {
                    let _ = close(clients.swap_remove(k));
                    continue;
                }
            }
            k += 1;
        }
        if !busy {
            zoo_rt::sleep(5);
        }
    }
}
