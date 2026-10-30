//! Zookeeper: the ZooOS supervisor (docs/m3-design.md §9). In M3 it runs a
//! fixed start-up plan: start LionHandler and talk to it, supervise
//! MonkeyHandler with a one-restart policy, confirm LionHandler survived,
//! then stop everything. Each failed expectation exits with its own code.

#![no_std]
#![no_main]

use zoo_rt::{Error, Handle, channel_create, close, kill, log, recv, send, spawn, wait};

zoo_rt::entry!(main);

/// Monkey runs once, then is restarted at most this many times.
const MONKEY_RESTARTS: u32 = 1;

/// PeacockHandler may be restarted this many times in one session.
const DESKTOP_RESTARTS: u32 = 3;

fn main(start_handle: u64) -> u8 {
    // Started with a control channel: its first message says what to do.
    if start_handle != zoo_rt::NO_HANDLE {
        let mut mode = [0u8; 32];
        return match recv(start_handle, &mut mode) {
            Ok(n) if &mode[..n] == b"session" => session(),
            _ => 30,
        };
    }
    startup_checks()
}

/// Desktop session (docs/m4-design.md §3): run the window server, restart it
/// if it ends abnormally, finish when it exits cleanly.
fn session() -> u8 {
    let _ = log("Zookeeper: starting the desktop session");
    let mut restarts = 0;
    loop {
        let Ok(peacock) = spawn("peacock-handler", None) else {
            return 2;
        };
        let outcome = wait(peacock);
        let _ = close(peacock);
        if outcome == Ok(0) {
            let _ = log("Zookeeper: desktop session ended");
            return 0;
        }
        if restarts == DESKTOP_RESTARTS {
            zoo_rt::log!(
                "Zookeeper: PeacockHandler ended ({:?}); restart limit reached",
                outcome
            );
            return 1;
        }
        restarts += 1;
        zoo_rt::log!(
            "Zookeeper: PeacockHandler ended ({:?}); restarting ({} of {})",
            outcome,
            restarts,
            DESKTOP_RESTARTS
        );
    }
}

/// The M3 start-up plan (docs/m3-design.md §9).
fn startup_checks() -> u8 {
    let abi = zoo_rt::abi_version();
    zoo_rt::log!("Zookeeper starting (kernel ABI {})", abi);
    if abi != zoo_abi::ABI_VERSION {
        return 10;
    }

    let Ok((mine, lions)) = channel_create() else {
        return 11;
    };
    let Ok(lion) = spawn("lion-handler", Some(lions)) else {
        return 12;
    };
    if !ping(mine, "ping 1") {
        return 13;
    }

    for run in 1..=MONKEY_RESTARTS + 1 {
        let Ok(monkey) = spawn("monkey-handler", None) else {
            return 14;
        };
        match wait(monkey) {
            Err(Error::Crashed) => {
                zoo_rt::log!(
                    "MonkeyHandler crashed (run {} of {})",
                    run,
                    MONKEY_RESTARTS + 1
                )
            }
            other => {
                zoo_rt::log!("MonkeyHandler did not crash: {:?}", other);
                return 15;
            }
        }
        let _ = close(monkey);
        if run <= MONKEY_RESTARTS {
            let _ = log("restart policy allows it: restarting MonkeyHandler");
        } else {
            let _ = log("restart limit reached: giving up on MonkeyHandler");
        }
    }

    // HamsterHandler never yields. Talking to LionHandler while it runs is
    // only possible if the kernel preempts it.
    let Ok(hamster) = spawn("hamster-handler", None) else {
        return 19;
    };
    // LionHandler must have survived MonkeyHandler's crashes.
    if !ping(mine, "ping 2") {
        return 16;
    }
    if kill(hamster).is_err() {
        return 20;
    }
    match wait(hamster) {
        Err(Error::Killed) => {
            let _ = log("HamsterHandler stopped by the supervisor");
        }
        other => {
            zoo_rt::log!("HamsterHandler did not end as killed: {:?}", other);
            return 21;
        }
    }
    let _ = close(hamster);
    if send(mine, b"stop").is_err() {
        return 17;
    }
    match wait(lion) {
        Ok(0) => {}
        other => {
            zoo_rt::log!("LionHandler did not exit cleanly: {:?}", other);
            return 18;
        }
    }
    let _ = log("Zookeeper: all checks passed");
    0
}

/// Sends `msg` and expects `pong: <msg>` back.
fn ping(h: Handle, msg: &str) -> bool {
    if send(h, msg.as_bytes()).is_err() {
        return false;
    }
    let mut buf = [0u8; zoo_abi::MAX_MESSAGE];
    let Ok(n) = recv(h, &mut buf) else {
        return false;
    };
    let reply = &buf[..n];
    let ok = reply.len() == 6 + msg.len()
        && reply.starts_with(b"pong: ")
        && &reply[6..] == msg.as_bytes();
    zoo_rt::log!(
        "sent `{}`, got `{}`",
        msg,
        core::str::from_utf8(reply).unwrap_or("<not utf-8>")
    );
    ok
}
