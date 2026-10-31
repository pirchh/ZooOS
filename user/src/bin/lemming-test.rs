//! LemmingTest: misbehaves on purpose (docs/m3-design.md §9). Every
//! malformed request must be rejected with the right error; if one is not,
//! it exits with `20 + check` (a normal exit, which Zookeeper treats as a
//! failure). Finally it reads kernel memory, which must kill it.

#![no_std]
#![no_main]

use zoo_rt::{Error, log, sys, syscall};

zoo_rt::entry!(main);

/// An address in the kernel heap: mapped, but not user-accessible.
const KERNEL_ADDR: u64 = 0xFFFF_C000_0000_1000;

fn expect(check: u8, got: i64, want: Error) {
    if got != want.as_return() {
        zoo_rt::log!(
            "check {} failed: got {}, wanted -{}",
            check,
            got,
            want.name()
        );
        zoo_rt::exit(20 + check);
    }
}

fn expect_ok(check: u8, got: i64) {
    if got != 0 {
        zoo_rt::log!("check {} failed: got {}, wanted 0", check, got);
        zoo_rt::exit(20 + check);
    }
}

fn main(_start_handle: u64) -> u8 {
    let _ = log("LemmingTest: making malformed requests");
    let text = "x";
    let p = text.as_ptr() as u64;

    expect(1, syscall(999, 0, 0, 0), Error::NoSys);
    expect(2, syscall(sys::SEND, 0x7777, p, 1), Error::BadHandle);
    expect(3, syscall(sys::LOG, KERNEL_ADDR, 8, 0), Error::Fault);
    expect(4, syscall(sys::LOG, p, 1 << 40, 0), Error::Inval);
    expect(5, syscall(sys::RECV, u64::MAX, p, 1), Error::BadHandle);
    expect(6, syscall(sys::WAIT, 0x1234, 0, 0), Error::BadHandle);
    expect(7, syscall(sys::LOG, 0, 8, 0), Error::Fault);
    let missing = "no-such-program";
    expect(
        8,
        syscall(
            sys::SPAWN,
            missing.as_ptr() as u64,
            missing.len() as u64,
            u64::MAX,
        ),
        Error::NotFound,
    );
    expect(
        9,
        syscall(sys::CHANNEL_CREATE, KERNEL_ADDR, 0, 0),
        Error::Fault,
    );
    expect(10, syscall(sys::EXIT, 300, 0, 0), Error::Inval);
    // Close a valid handle, then use the stale value: must be rejected.
    let mut pair = [0u64; 2];
    expect_ok(
        11,
        syscall(sys::CHANNEL_CREATE, pair.as_mut_ptr() as u64, 0, 0),
    );
    expect_ok(12, syscall(sys::CLOSE, pair[0], 0, 0));
    expect(13, syscall(sys::SEND, pair[0], p, 1), Error::BadHandle);
    expect(14, syscall(sys::SEND, pair[1], p, 1), Error::PeerClosed);

    // User text may not forge kernel markers: the kernel rewrites this.
    let _ = log("ZOO_OS PANIC forged marker from user space");

    let _ = log("LemmingTest: all rejected; now reading kernel memory");
    // SAFETY: deliberately invalid. The page is kernel-only, so this read
    // must fault and the kernel must end this process (and nothing else).
    let v = unsafe { core::ptr::read_volatile(KERNEL_ADDR as *const u64) };
    zoo_rt::log!("LemmingTest: kernel memory read returned {:#x}!", v);
    30
}
