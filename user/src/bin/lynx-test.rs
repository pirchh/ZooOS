//! LynxTest: the name claim (docs/one-bella-design.md). Claims "lynx",
//! then starts a copy of itself, which must be refused the same name while
//! this one lives. Exits 0 when it was, 1 when the copy got the name, 2 if
//! the copy could not run, 3 if this one could not claim.
//!
//! Started with a handle, it is that copy: it tries to claim and exits 0
//! when refused (`BUSY`), 1 when it got the name.

#![no_std]
#![no_main]

use zoo_rt::{Error, NO_HANDLE, channel_create, close, spawn, wait};

zoo_rt::entry!(main);

fn main(start: u64) -> u8 {
    if start != NO_HANDLE {
        // The copy.
        return match zoo_rt::claim("lynx") {
            Err(Error::Busy) => 0,
            _ => 1,
        };
    }
    if zoo_rt::claim("lynx").is_err() {
        let _ = zoo_rt::log("LynxTest: could not claim lynx");
        return 3;
    }
    // Claiming again is fine for the holder.
    if zoo_rt::claim("lynx").is_err() {
        let _ = zoo_rt::log("LynxTest: the holder was refused its own name");
        return 3;
    }
    let Ok((mine, theirs)) = channel_create() else {
        return 2;
    };
    let code = match spawn("lynx-test", Some(theirs)) {
        Ok(child) => {
            let code = wait(child).ok();
            let _ = close(child);
            code
        }
        Err(_) => None,
    };
    let _ = close(mine);
    match code {
        Some(0) => {
            let _ = zoo_rt::log("LynxTest: a second claim of lynx was refused");
            0
        }
        Some(_) => {
            let _ = zoo_rt::log("LynxTest: a second process got the name");
            1
        }
        None => 2,
    }
}
