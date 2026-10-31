//! seal-test: an Arctic test service (docs/s1-design.md §4). It answers
//! `ping` like any service; `crash` makes it fault and `hang` makes it stop
//! answering, so the supervisor's restart and health checks can be tested
//! without debug hooks in real services. `connect` makes it reach
//! BeaverHandler by name through Zookeeper (docs/s3-design.md) and ask it
//! for lines directly.

#![no_std]
#![no_main]

use zoo_rt::app::send_wait;
use zoo_rt::{Error, close, recv, send, try_recv, try_recv_handle};

zoo_rt::entry!(main);

/// Kernel heap: mapped, but not user-accessible.
const KERNEL_ADDR: u64 = 0xFFFF_C000_0000_1000;

fn main(chan: u64) -> u8 {
    let mut buf = [0u8; 32];
    loop {
        let Ok(n) = recv(chan, &mut buf) else {
            return 0;
        };
        match &buf[..n] {
            b"ping" => {
                if send_wait(chan, b"pong").is_err() {
                    return 0;
                }
            }
            b"crash" => {
                // SAFETY: deliberately reads a kernel-only page: this must
                // fault and end this process alone.
                let v = unsafe { core::ptr::read_volatile(KERNEL_ADDR as *const u64) };
                zoo_rt::log!("seal-test: read returned {:#x}!", v);
                return 30;
            }
            b"hang" => loop {
                core::hint::spin_loop();
            },
            b"connect" if !connect_to_beaver(chan) => return 0,
            _ => {}
        }
    }
}

/// Asks Zookeeper for a channel to BeaverHandler, then talks to it
/// directly. Keeps answering pings meanwhile. False if Zookeeper is gone.
fn connect_to_beaver(chan: u64) -> bool {
    if send(chan, b"connect beaver-handler").is_err() {
        return false;
    }
    let mut buf = [0u8; zoo_abi::MAX_MESSAGE];
    let deadline = zoo_rt::ticks() + 300;
    let beaver = loop {
        if zoo_rt::ticks() > deadline {
            let _ = zoo_rt::log("seal-test: no connection from Zookeeper");
            return true;
        }
        match try_recv_handle(chan) {
            Ok((h, 2)) => break h,
            Ok((h, _)) => {
                let _ = close(h);
            }
            Err(Error::WrongKind) => match try_recv(chan, &mut buf) {
                Ok(n) if &buf[..n] == b"ping" => {
                    if send_wait(chan, b"pong").is_err() {
                        return false;
                    }
                }
                Ok(n) => {
                    zoo_rt::log!(
                        "seal-test: Zookeeper said `{}`",
                        core::str::from_utf8(&buf[..n]).unwrap_or("?")
                    );
                    return true;
                }
                Err(_) => return false,
            },
            Err(Error::Again) => zoo_rt::sleep(1),
            Err(_) => return false,
        }
    };
    // A request straight to BeaverHandler, not through Zookeeper.
    let mut lines = 0;
    if send(beaver, b"dump seal-test").is_ok() {
        let deadline = zoo_rt::ticks() + 200;
        while zoo_rt::ticks() < deadline {
            match try_recv(beaver, &mut buf) {
                Ok(n) if &buf[..n] == b"end" => break,
                Ok(_) => lines += 1,
                Err(Error::Again) => zoo_rt::sleep(1),
                Err(_) => break,
            }
        }
    }
    let _ = close(beaver);
    if lines > 0 {
        zoo_rt::log!(
            "seal-test: connected to beaver-handler by name; it returned {} lines",
            lines
        );
    } else {
        let _ = zoo_rt::log("seal-test: connected to beaver-handler but got no lines");
    }
    true
}
