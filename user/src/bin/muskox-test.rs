//! MuskoxTest: runs on its wheel forever without ever making a system
//! call after its first log line. The rest of the system can only keep
//! working if the kernel preempts it; Zookeeper ends it with `kill`
//! (docs/m3-design.md §9).

#![no_std]
#![no_main]

use zoo_rt::log;

zoo_rt::entry!(main);

fn main(_start_handle: u64) -> u8 {
    let _ = log("MuskoxTest: running on the wheel, never yielding");
    let mut laps: u64 = 0;
    loop {
        laps = core::hint::black_box(laps.wrapping_add(1));
    }
}
