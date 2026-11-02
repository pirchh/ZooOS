//! GyrfalconTest: the TCP check (docs/n2-design.md). Gets an address by
//! DHCP, opens a TCP connection to the test server the harness forwards to
//! 10.0.2.100:80, sends an HTTP request, and logs the answer. Exit 0 only
//! for a `200 OK` with the body the harness serves.

#![no_std]
#![no_main]

use zoo_rt::net::{IpText, Stack, net_attach};

zoo_rt::entry!(main);

/// Ticks to wait for each reply (100 ticks = 1 s).
const WAIT: u64 = 300;
/// Where the harness's server appears on QEMU's network.
const SERVER: [u8; 4] = [10, 0, 2, 100];
/// What it answers with.
const BODY: &str = "Hello from the host, ZooOS!";

fn main(_channel: u64) -> u8 {
    let Ok(h) = net_attach() else {
        zoo_rt::log!("GyrfalconTest: no network device");
        return 2;
    };
    let Ok(mut net) = Stack::new(h) else {
        return 3;
    };
    let lease = match net.dhcp(WAIT) {
        Ok(l) => l,
        Err(why) => {
            zoo_rt::log!("GyrfalconTest: DHCP failed: {}", why);
            return 4;
        }
    };
    // QEMU's gateway carries everything off our machine.
    let gw = if lease.gateway == [0; 4] {
        lease.server
    } else {
        lease.gateway
    };
    let Some(mac) = net.resolve(gw, WAIT) else {
        zoo_rt::log!("GyrfalconTest: no ARP reply from {}", IpText(gw));
        return 5;
    };
    let mut c = match net.connect(mac, SERVER, 80, WAIT) {
        Ok(c) => c,
        Err(why) => {
            zoo_rt::log!(
                "GyrfalconTest: cannot connect to {}:80: {}",
                IpText(SERVER),
                why
            );
            return 6;
        }
    };
    zoo_rt::log!("GyrfalconTest: connected to {}:80", IpText(SERVER));
    let request = b"GET /zoo HTTP/1.0\r\nHost: 10.0.2.100\r\nUser-Agent: ZooOS\r\n\r\n";
    if let Err(why) = net.write(&mut c, request, WAIT) {
        zoo_rt::log!("GyrfalconTest: sending failed: {}", why);
        return 7;
    }
    let reply = match net.read_to_end(&mut c, WAIT) {
        Ok(r) => r,
        Err(why) => {
            zoo_rt::log!("GyrfalconTest: receiving failed: {}", why);
            return 8;
        }
    };
    net.close(&mut c, WAIT);
    let text = core::str::from_utf8(&reply).unwrap_or("");
    let status = text.lines().next().unwrap_or("");
    let body = text.split("\r\n\r\n").nth(1).unwrap_or("").trim_end();
    zoo_rt::log!(
        "GyrfalconTest: {} bytes, status `{}`, body `{}`",
        reply.len(),
        status,
        body
    );
    if !status.ends_with("200 OK") || body != BODY {
        return 9;
    }
    zoo_rt::log!("GyrfalconTest: http ok");
    0
}
