//! seal-test: an Arctic test service (docs/s1-design.md §4). It answers
//! `ping` like any service; `crash` makes it fault and `hang` makes it stop
//! answering, so the supervisor's restart and health checks can be tested
//! without debug hooks in real services.

#![no_std]
#![no_main]

use zoo_rt::app::send_wait;
use zoo_rt::recv;

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
            _ => {}
        }
    }
}
