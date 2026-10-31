//! Crash Test: a crash-test window (docs/m4-design.md §6). Enter makes it
//! read kernel memory, which must end only this process; the window server
//! then restarts it.

#![no_std]
#![no_main]

use zoo_rt::app::{Key, Window};
use zoo_rt::{Error, log, recv};

zoo_rt::entry!(main);

/// Kernel heap: mapped, but not user-accessible.
const KERNEL_ADDR: u64 = 0xFFFF_C000_0000_1000;

fn main(channel: u64) -> u8 {
    let win = Window::new(channel);
    let shown = (|| {
        win.title("Crash Test")?;
        win.lines(3)?;
        win.line(0, "This window is a crash test.")?;
        win.line(1, "Press Enter to make it read kernel memory.")?;
        win.line(2, "The window server will restart it.")?;
        win.select(None)?;
        win.show()
    })();
    if shown.is_err() {
        return 0;
    }
    let mut buf = [0u8; zoo_abi::MAX_MESSAGE];
    loop {
        let n = match recv(channel, &mut buf) {
            Ok(n) => n,
            Err(Error::PeerClosed) => return 0,
            Err(_) => return 4,
        };
        if Key::parse(&buf[..n]) == Some(Key::Enter) {
            let _ = log("Crash Test: reading kernel memory as asked");
            // SAFETY: deliberately invalid; the page is kernel-only, so this
            // must fault and end this process alone.
            let v = unsafe { core::ptr::read_volatile(KERNEL_ADDR as *const u64) };
            zoo_rt::log!("Crash Test: read returned {:#x}!", v);
            return 30;
        }
    }
}
