//! BowheadTest: file throughput through BadgerHandler (docs/f1-design.md).
//! Starts a private BadgerHandler, writes 1 MiB with `file_write`, reads it
//! back with `file_read`, writes it again (warm), checks it, and logs the
//! times in CPU cycles (the timer's 10 ms is too coarse) and milliseconds.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec;
use zoo_rt::{close, file_create, file_read, file_write, kill, start_badger, wait};

zoo_rt::entry!(main);

const SIZE: usize = 1 << 20;
const PATH: &str = "bench.bin";

fn cycles() -> u64 {
    // SAFETY: `rdtsc` only reads the time-stamp counter.
    unsafe { core::arch::x86_64::_rdtsc() }
}

fn ms() -> u64 {
    zoo_rt::clock(zoo_abi::CLOCK_MONOTONIC).unwrap_or(0) / 1_000_000
}

fn main(_start: u64) -> u8 {
    let Ok((process, control, root)) = start_badger() else {
        zoo_rt::log!("BowheadTest: cannot start BadgerHandler");
        return 1;
    };
    let data: alloc::vec::Vec<u8> = (0..SIZE).map(|i| (i * 31 % 251) as u8).collect();
    let mut back = vec![0u8; SIZE];
    if file_create(root, PATH).is_err() {
        return 2;
    }
    let (c0, m0) = (cycles(), ms());
    if file_write(root, PATH, 0, &data) != Ok(SIZE) {
        return 3;
    }
    let (c1, m1) = (cycles(), ms());
    if file_read(root, PATH, 0, &mut back) != Ok(SIZE) {
        return 4;
    }
    let (c2, m2) = (cycles(), ms());
    let w2 = cycles();
    let _ = file_write(root, PATH, 0, &data);
    zoo_rt::log!("BowheadTest: warm write {} kcycles", (cycles() - w2) / 1000);
    if back != data {
        zoo_rt::log!("BowheadTest: the data read back differs");
        return 5;
    }
    zoo_rt::log!(
        "BowheadTest: 1 MiB write {} kcycles ({} ms), read {} kcycles ({} ms)",
        (c1 - c0) / 1000,
        m1 - m0,
        (c2 - c1) / 1000,
        m2 - m1
    );
    let _ = close(root);
    let _ = close(control);
    let _ = kill(process);
    let _ = wait(process);
    let _ = close(process);
    zoo_rt::log!("BowheadTest: done");
    0
}
