//! ptarmigan-test: the handle-passing test (docs/s3-design.md §3). Exits 0
//! only if every check passes; the exit code names the first failure.

#![no_std]
#![no_main]

use zoo_abi::WAIT_FOREVER;
use zoo_rt::{
    Error, channel_create, close, recv, recv_handle, send, send_handle, try_recv, try_recv_handle,
    wait_any,
};

zoo_rt::entry!(main);

fn check(ok: bool, code: u8, what: &'static str) -> Result<(), (u8, &'static str)> {
    if ok { Ok(()) } else { Err((code, what)) }
}

fn checks() -> Result<(), (u8, &'static str)> {
    let mut buf = [0u8; 32];
    let (a0, a1) = channel_create().map_err(|_| (10, "channel a"))?;
    let (b0, b1) = channel_create().map_err(|_| (10, "channel b"))?;

    // 1. Move b1 through a; it is gone here and works over there.
    check(send_handle(a0, b1, 7).is_ok(), 10, "send_handle")?;
    check(
        send(b1, b"x") == Err(Error::BadHandle),
        10,
        "moved handle gone from sender",
    )?;
    let (moved, tag) = recv_handle(a1).map_err(|_| (10, "recv_handle"))?;
    check(tag == 7, 10, "tag carried")?;
    check(send(b0, b"hello").is_ok(), 10, "send on b0")?;
    let n = recv(moved, &mut buf).map_err(|_| (10, "recv on moved end"))?;
    check(&buf[..n] == b"hello", 10, "moved end receives")?;

    // 2. The two message kinds never mix up; the other kind stays queued.
    check(send(a0, b"bytes").is_ok(), 11, "send bytes")?;
    check(
        try_recv_handle(a1) == Err(Error::WrongKind),
        11,
        "recv_handle on bytes",
    )?;
    let n = recv(a1, &mut buf).map_err(|_| (11, "bytes still queued"))?;
    check(&buf[..n] == b"bytes", 11, "bytes preserved")?;
    let (c0, c1) = channel_create().map_err(|_| (11, "channel c"))?;
    check(send_handle(a0, c1, 9).is_ok(), 11, "send handle")?;
    check(
        try_recv(a1, &mut buf) == Err(Error::WrongKind),
        11,
        "recv on a handle",
    )?;
    let (c1_again, tag) = try_recv_handle(a1).map_err(|_| (11, "handle still queued"))?;
    check(tag == 9, 11, "handle preserved")?;

    // 3. An end cannot carry itself.
    check(
        send_handle(a0, a0, 0) == Err(Error::Inval),
        12,
        "self-send refused",
    )?;

    // 4. Handles queued towards an end that closes are released: the peer
    //    of a queued channel end then sees PEERCLOSED.
    check(send_handle(a0, c1_again, 3).is_ok(), 13, "queue c1 again")?;
    check(close(a1).is_ok(), 13, "close receiving end")?;
    check(
        send(c0, b"x") == Err(Error::PeerClosed),
        13,
        "queued handle released",
    )?;

    for h in [a0, b0, moved, c0] {
        let _ = close(h);
    }

    // 5. wait_any (S4a): which handle is ready, timeouts, closed peers, and
    //    a wake-up from another thread.
    let (x0, x1) = channel_create().map_err(|_| (14, "channel x"))?;
    let (y0, y1) = channel_create().map_err(|_| (14, "channel y"))?;
    check(
        wait_any(&[x1, y1], 0) == Err(Error::TimedOut),
        14,
        "nothing ready",
    )?;
    check(send(y0, b"y").is_ok(), 14, "send y")?;
    check(
        wait_any(&[x1, y1], WAIT_FOREVER) == Ok(1),
        14,
        "second ready",
    )?;
    let _ = recv(y1, &mut buf);
    let t0 = zoo_rt::ticks();
    check(
        wait_any(&[x1, y1], 5) == Err(Error::TimedOut),
        14,
        "timeout",
    )?;
    check(zoo_rt::ticks() - t0 >= 5, 14, "timeout waited")?;
    let waker = zoo_rt::thread::spawn(move || {
        zoo_rt::sleep(3);
        if send(x0, b"wake").is_ok() { 0 } else { 1 }
    })
    .map_err(|_| (14, "thread"))?;
    check(
        wait_any(&[y1, x1], WAIT_FOREVER) == Ok(1),
        14,
        "woken by another thread",
    )?;
    check(zoo_rt::wait(waker) == Ok(0), 14, "waker done")?;
    let _ = recv(x1, &mut buf);
    check(close(y0).is_ok(), 14, "close y0")?;
    check(wait_any(&[x1, y1], 0) == Ok(1), 14, "closed peer is ready")?;
    for h in [x0, x1, y1] {
        let _ = close(h);
    }
    Ok(())
}

fn main(_start: u64) -> u8 {
    match checks() {
        Ok(()) => {
            let _ = zoo_rt::log("PtarmiganTest: all handle-passing checks passed");
            0
        }
        Err((code, what)) => {
            zoo_rt::log!("PtarmiganTest: check {} failed: {}", code, what);
            code
        }
    }
}
