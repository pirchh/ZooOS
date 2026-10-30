//! BeaverHandler: a floating-point worker (docs/m5a-design.md §5). Receives
//! a parameter on its channel, runs `beaver_series` (long enough to be
//! preempted many times, alongside another BeaverHandler), and sends back the
//! result's exact bits. Also uses a few MiB of heap.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use zoo_rt::{recv, send};

zoo_rt::entry!(main);

/// Iterations: long enough to span many timer ticks under emulation.
pub const ITERATIONS: u64 = 3_000_000;

fn main(channel: u64) -> u8 {
    let mut buf = [0u8; 16];
    let Ok(8) = recv(channel, &mut buf) else {
        return 2;
    };
    let param = f64::from_bits(u64::from_le_bytes(buf[..8].try_into().unwrap_or([0; 8])));

    // Exercise heap growth beyond the first 1 MiB chunk.
    let mut big: Vec<u8> = Vec::with_capacity(4 << 20);
    big.resize(4 << 20, 0xBE);
    if big.iter().step_by(4096).any(|&b| b != 0xBE) {
        return 3;
    }

    let mut trace = Vec::new();
    let result = zoo_rt::beaver_series(param, ITERATIONS, &mut trace);
    if send(channel, &result.to_bits().to_le_bytes()).is_err() {
        return 4;
    }
    0
}
