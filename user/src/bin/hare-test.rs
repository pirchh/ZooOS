//! HareTest: the kernel heap check (docs/heap-design.md). Run from the
//! shell (`run hare-test`). It checks that channel ends cannot be parked
//! inside channels where no close reaches them, then starts three copies
//! of itself that fill the kernel heap with queued messages, and checks
//! that they are refused with `NOMEM` instead of panicking the kernel, and
//! that the heap is usable again once they exit. Exit 0 only if all of
//! that held.

#![no_std]
#![no_main]

use zoo_rt::{
    Error, Handle, NO_HANDLE, channel_create, close, recv, send, send_handle, spawn, wait,
};

zoo_rt::entry!(main);

/// Copies of itself filling the heap at once.
const FLOODERS: usize = 3;
/// Holder channels per filler: 30 x 16 parked channels x 8 KiB, about
/// 3.8 MiB each, so three fill the 8 MiB heap.
const HOLDERS: usize = 30;

fn main(channel: Handle) -> u8 {
    if channel != NO_HANDLE {
        return flood(channel);
    }
    // 1. An end over its own channel: it would wait in its own queue.
    let Ok((a, b)) = channel_create() else {
        return 2;
    };
    match send_handle(a, b, 0) {
        Err(Error::Inval) => zoo_rt::log!("HareTest: an end over its own channel: refused"),
        r => {
            zoo_rt::log!(
                "HareTest: an end over its own channel: {:?}",
                r.err().map(|e| e.name())
            );
            return 3;
        }
    }
    let _ = close(a);
    let _ = close(b);
    // 2. An end whose channel already carries a handle: refused, so ends
    // can never hold each other.
    let (Ok((x0, x1)), Ok((y0, y1)), Ok((z0, z1))) =
        (channel_create(), channel_create(), channel_create())
    else {
        return 4;
    };
    if send_handle(x0, z1, 0).is_err() {
        return 5;
    }
    match send_handle(y0, x1, 0) {
        Err(Error::Busy) => zoo_rt::log!("HareTest: an end carrying a handle: refused"),
        r => {
            zoo_rt::log!(
                "HareTest: an end carrying a handle: {:?}",
                r.err().map(|e| e.name())
            );
            return 6;
        }
    }
    for h in [x0, x1, y0, y1, z0] {
        let _ = close(h);
    }
    // 3. Fill the heap from three processes at once.
    let mut kids = [(NO_HANDLE, NO_HANDLE); FLOODERS];
    for k in kids.iter_mut() {
        let Ok((mine, theirs)) = channel_create() else {
            return 7;
        };
        let Ok(p) = spawn("hare-test", Some(theirs)) else {
            return 8;
        };
        *k = (mine, p);
    }
    let mut refused = 0;
    let mut kept = 0usize;
    for (mine, _) in kids {
        let mut buf = [0u8; 16];
        let Ok(n) = recv(mine, &mut buf) else {
            return 9;
        };
        let r = core::str::from_utf8(&buf[..n]).unwrap_or("?");
        let (what, kib) = r.split_once(' ').unwrap_or((r, "0"));
        kept += kib.parse::<usize>().unwrap_or(0);
        if what == "nomem" {
            refused += 1;
        }
    }
    zoo_rt::log!(
        "HareTest: {} of {} fillers refused with NOMEM, {} KiB queued in all",
        refused,
        FLOODERS,
        kept
    );
    // Let them go; their queues are freed as they exit.
    for (mine, p) in kids {
        let _ = send(mine, b"go");
        let _ = wait(p);
        let _ = close(mine);
    }
    if refused == 0 {
        return 10;
    }
    // 4. The heap is usable again.
    let Ok((a, b)) = channel_create() else {
        zoo_rt::log!("HareTest: no channel after the fillers exited");
        return 11;
    };
    if send(a, &[0u8; 1024]).is_err() {
        return 12;
    }
    let _ = close(a);
    let _ = close(b);
    zoo_rt::log!("HareTest: kernel heap ok");
    0
}

/// Queues as much as one process can hold: channels full of 1 KiB
/// messages, parked in holder channels. Reports `nomem <KiB>` or
/// `full <KiB>` to the parent, then holds it all until told to go.
fn flood(parent: Handle) -> u8 {
    let page = [0x5au8; 1024];
    let mut kib = 0usize;
    let mut nomem = false;
    let mut holders = [(NO_HANDLE, NO_HANDLE); HOLDERS];
    let mut held = 0;
    // 30 holders keep within the 64 handles (each holder two, plus the
    // pair being filled), so a `NOMEM` here can only be the heap's.
    'outer: while held < HOLDERS {
        // A holder: both its ends stay here, each queue up to 8 ends.
        let (h0, h1) = match channel_create() {
            Ok(p) => p,
            Err(e) => {
                nomem = e == Error::NoMem;
                break;
            }
        };
        holders[held] = (h0, h1);
        held += 1;
        for holder in [h0, h1] {
            for _ in 0..8 {
                let (a, b) = match channel_create() {
                    Ok(p) => p,
                    Err(e) => {
                        nomem = e == Error::NoMem;
                        break 'outer;
                    }
                };
                for _ in 0..8 {
                    match send(a, &page) {
                        Ok(()) => kib += 1,
                        Err(e) => {
                            nomem = e == Error::NoMem;
                            break;
                        }
                    }
                }
                let parked = send_handle(holder, b, 0);
                let _ = close(a);
                if nomem || parked.is_err() {
                    break 'outer;
                }
            }
        }
    }
    let mut line = zoo_rt::Line::new();
    let _ = core::fmt::Write::write_fmt(
        &mut line,
        format_args!("{} {}", if nomem { "nomem" } else { "full" }, kib),
    );
    // With the heap full even this short report is refused: give back a
    // holder at a time until it goes, as a program should.
    while send(parent, line.as_str().as_bytes()) == Err(Error::NoMem) && held > 0 {
        held -= 1;
        let _ = close(holders[held].0);
        let _ = close(holders[held].1);
    }
    let mut buf = [0u8; 4];
    let _ = recv(parent, &mut buf);
    0
}
