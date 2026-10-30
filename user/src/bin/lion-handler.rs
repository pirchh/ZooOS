//! LionHandler: answers each message on its start-up channel with
//! `pong: <message>`; exits 0 on `stop`, 2 if the peer goes away.

#![no_std]
#![no_main]

use zoo_rt::{Error, log, recv, send};

zoo_rt::entry!(main);

fn main(channel: u64) -> u8 {
    let _ = log("LionHandler ready");
    let mut buf = [0u8; zoo_abi::MAX_MESSAGE];
    loop {
        match recv(channel, &mut buf) {
            Ok(n) if &buf[..n] == b"stop" => {
                let _ = log("LionHandler stopping");
                return 0;
            }
            Ok(n) => {
                let mut out = [0u8; zoo_abi::MAX_MESSAGE];
                let n = n.min(out.len() - 6);
                out[..6].copy_from_slice(b"pong: ");
                out[6..6 + n].copy_from_slice(&buf[..n]);
                if send(channel, &out[..6 + n]).is_err() {
                    return 3;
                }
            }
            Err(Error::PeerClosed) => return 2,
            Err(_) => return 4,
        }
    }
}
