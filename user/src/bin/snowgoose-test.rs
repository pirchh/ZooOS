//! SnowGooseTest: the network check (docs/n1-design.md). Takes the network
//! device, gets an address by DHCP, finds the gateway by ARP, and pings it
//! three times. Exit 0 only if all of that worked.

#![no_std]
#![no_main]

use zoo_rt::net::{IpText, MacText, Stack, net_attach};

zoo_rt::entry!(main);

/// Ticks to wait for each reply (100 ticks = 1 s).
const WAIT: u64 = 300;

fn main(_channel: u64) -> u8 {
    let h = match net_attach() {
        Ok(h) => h,
        Err(e) => {
            zoo_rt::log!("SnowGooseTest: no network device: {}", e.name());
            return 2;
        }
    };
    let Ok(mut net) = Stack::new(h) else {
        return 3;
    };
    zoo_rt::log!("SnowGooseTest: device {}", MacText(net.mac));
    let lease = match net.dhcp(WAIT) {
        Ok(l) => l,
        Err(why) => {
            zoo_rt::log!("SnowGooseTest: DHCP failed: {}", why);
            return 4;
        }
    };
    zoo_rt::log!(
        "SnowGooseTest: address {} mask {} gateway {} dns {} lease {} s",
        IpText(lease.ip),
        IpText(lease.mask),
        IpText(lease.gateway),
        IpText(lease.dns),
        lease.seconds
    );
    // No router given (QEMU's isolated network leaves it out): the DHCP
    // server is the next machine to talk to.
    let mut lease = lease;
    if lease.gateway == [0; 4] {
        lease.gateway = lease.server;
        zoo_rt::log!(
            "SnowGooseTest: no router given; using the DHCP server {}",
            IpText(lease.server)
        );
    }
    let Some(gw) = net.resolve(lease.gateway, WAIT) else {
        zoo_rt::log!("SnowGooseTest: no ARP reply from {}", IpText(lease.gateway));
        return 5;
    };
    zoo_rt::log!(
        "SnowGooseTest: gateway {} is at {}",
        IpText(lease.gateway),
        MacText(gw)
    );
    for seq in 1..=3 {
        match net.ping(gw, lease.gateway, seq, WAIT) {
            Some(t) => zoo_rt::log!(
                "SnowGooseTest: ping {} seq {}: reply within {} ms",
                IpText(lease.gateway),
                seq,
                (t + 1) * 10
            ),
            None => {
                zoo_rt::log!(
                    "SnowGooseTest: ping {} seq {}: no reply",
                    IpText(lease.gateway),
                    seq
                );
                return 6;
            }
        }
    }
    zoo_rt::log!("SnowGooseTest: network ok");
    0
}
