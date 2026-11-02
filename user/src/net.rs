//! A small IPv4 stack over the raw network device (docs/n1-design.md):
//! Ethernet, ARP, IPv4, ICMP echo, UDP, a DHCP client, and a TCP client
//! (docs/n2-design.md). One process
//! holds the device (`net_attach`); this runs inside it.

use crate::{Error, Handle, result, sleep, syscall, ticks};
use alloc::vec::Vec;
use zoo_abi::{NET_MAX_FRAME, sys};

pub type Mac = [u8; 6];
pub type Ip = [u8; 4];

pub const BROADCAST_MAC: Mac = [0xFF; 6];
const ETH_IPV4: u16 = 0x0800;
const ETH_ARP: u16 = 0x0806;
const PROTO_ICMP: u8 = 1;
const PROTO_UDP: u8 = 17;
const DHCP_CLIENT: u16 = 68;
const DHCP_SERVER: u16 = 67;
/// DHCP's magic cookie, after the fixed BOOTP fields.
const COOKIE: [u8; 4] = [99, 130, 83, 99];

/// A handle to the network device, for this process alone.
pub fn net_attach() -> Result<Handle, Error> {
    result(syscall(sys::NET_ATTACH, 0, 0, 0))
}

/// The device's MAC address.
pub fn net_mac(h: Handle) -> Result<Mac, Error> {
    let v = result(syscall(sys::NET_MAC, h, 0, 0))?;
    Ok(core::array::from_fn(|k| (v >> (8 * k)) as u8))
}

pub fn net_send(h: Handle, frame: &[u8]) -> Result<(), Error> {
    result(syscall(
        sys::NET_SEND,
        h,
        frame.as_ptr() as u64,
        frame.len() as u64,
    ))
    .map(|_| ())
}

/// One received frame, or `Error::Again` if none is waiting.
pub fn net_recv(h: Handle, buf: &mut [u8]) -> Result<usize, Error> {
    result(syscall(
        sys::NET_RECV,
        h,
        buf.as_mut_ptr() as u64,
        buf.len() as u64,
    ))
    .map(|n| n as usize)
}

/// The internet checksum (RFC 1071) of `data`.
pub fn checksum(data: &[u8]) -> u16 {
    let mut sum = 0u32;
    for pair in data.chunks(2) {
        let w = u16::from_be_bytes([pair[0], *pair.get(1).unwrap_or(&0)]);
        sum += u32::from(w);
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    !(sum as u16)
}

fn be16(b: &[u8], at: usize) -> u16 {
    u16::from_be_bytes([b[at], b[at + 1]])
}

/// What DHCP gave us.
#[derive(Debug, Clone, Copy, Default)]
pub struct Lease {
    pub ip: Ip,
    pub mask: Ip,
    pub gateway: Ip,
    pub dns: Ip,
    pub server: Ip,
    pub seconds: u32,
}

/// The stack's state: the device, our addresses, and a frame buffer.
pub struct Stack {
    pub h: Handle,
    pub mac: Mac,
    pub ip: Ip,
    pub gateway_mac: Option<Mac>,
    pub lease: Lease,
    ident: u16,
    frame: [u8; NET_MAX_FRAME],
    rx: [u8; NET_MAX_FRAME],
}

/// A received IPv4 packet: (protocol, source, payload start, payload end)
/// in `Stack::rx`.
struct Packet {
    proto: u8,
    src: Ip,
    start: usize,
    end: usize,
}

impl Stack {
    pub fn new(h: Handle) -> Result<Stack, Error> {
        Ok(Stack {
            h,
            mac: net_mac(h)?,
            ip: [0; 4],
            gateway_mac: None,
            lease: Lease::default(),
            ident: 1,
            frame: [0; NET_MAX_FRAME],
            rx: [0; NET_MAX_FRAME],
        })
    }

    /// Sends `payload` (already in `frame[14..14 + len]`) to `dst`.
    fn send_eth(&mut self, dst: Mac, ethertype: u16, len: usize) -> Result<(), Error> {
        self.frame[..6].copy_from_slice(&dst);
        self.frame[6..12].copy_from_slice(&self.mac);
        self.frame[12..14].copy_from_slice(&ethertype.to_be_bytes());
        // Ethernet's minimum: 60 bytes before the CRC.
        let total = (14 + len).max(60);
        self.frame[14 + len..total].fill(0);
        net_send(self.h, &self.frame[..total])
    }

    /// Writes an IPv4 header for `len` payload bytes and sends it.
    fn send_ip(
        &mut self,
        dst_mac: Mac,
        src: Ip,
        dst: Ip,
        proto: u8,
        len: usize,
    ) -> Result<(), Error> {
        let total = 20 + len;
        let h = &mut self.frame[14..34];
        h[0] = 0x45;
        h[1] = 0;
        h[2..4].copy_from_slice(&(total as u16).to_be_bytes());
        h[4..6].copy_from_slice(&self.ident.to_be_bytes());
        h[6..8].copy_from_slice(&[0x40, 0]); // don't fragment
        h[8] = 64;
        h[9] = proto;
        h[10..12].fill(0);
        h[12..16].copy_from_slice(&src);
        h[16..20].copy_from_slice(&dst);
        let c = checksum(h);
        h[10..12].copy_from_slice(&c.to_be_bytes());
        self.ident = self.ident.wrapping_add(1);
        self.send_eth(dst_mac, ETH_IPV4, total)
    }

    /// Sends a UDP datagram of `len` bytes already at `frame[42..]`.
    fn send_udp(
        &mut self,
        dst_mac: Mac,
        src: Ip,
        dst: Ip,
        ports: (u16, u16),
        len: usize,
    ) -> Result<(), Error> {
        let u = &mut self.frame[34..42];
        u[0..2].copy_from_slice(&ports.0.to_be_bytes());
        u[2..4].copy_from_slice(&ports.1.to_be_bytes());
        u[4..6].copy_from_slice(&((8 + len) as u16).to_be_bytes());
        u[6..8].fill(0); // no checksum (allowed over IPv4)
        self.send_ip(dst_mac, src, dst, PROTO_UDP, 8 + len)
    }

    /// Waits up to `timeout` ticks for an IPv4 packet that `want` accepts,
    /// answering ARP requests for our address meanwhile.
    fn wait_ip(&mut self, timeout: u64, want: impl Fn(&Packet, &[u8]) -> bool) -> Option<Packet> {
        let end = ticks() + timeout;
        loop {
            let n = match net_recv(self.h, &mut self.rx) {
                Ok(n) => n,
                Err(Error::Again) => {
                    if ticks() >= end {
                        return None;
                    }
                    sleep(1);
                    continue;
                }
                Err(_) => return None,
            };
            if n < 14 {
                continue;
            }
            match be16(&self.rx, 12) {
                ETH_ARP => self.answer_arp(n),
                ETH_IPV4 if n >= 34 => {
                    let ihl = usize::from(self.rx[14] & 0x0F) * 4;
                    let total = usize::from(be16(&self.rx, 16));
                    if ihl < 20 || 14 + total > n || total < ihl {
                        continue;
                    }
                    let p = Packet {
                        proto: self.rx[23],
                        src: self.rx[26..30].try_into().unwrap_or([0; 4]),
                        start: 14 + ihl,
                        end: 14 + total,
                    };
                    if want(&p, &self.rx[p.start..p.end]) {
                        return Some(p);
                    }
                }
                _ => {}
            }
        }
    }

    /// Replies to an ARP request for our address in `rx[..n]`.
    fn answer_arp(&mut self, n: usize) {
        let a = &self.rx[14..n];
        if self.ip == [0; 4] || a.len() < 28 || be16(a, 6) != 1 || a[24..28] != self.ip {
            return;
        }
        let (sender_mac, sender_ip): (Mac, Ip) = (
            a[8..14].try_into().unwrap_or([0; 6]),
            a[14..18].try_into().unwrap_or([0; 4]),
        );
        self.arp(2, sender_mac, sender_mac, sender_ip);
    }

    /// Sends an ARP packet: `op` 1 request, 2 reply.
    fn arp(&mut self, op: u16, eth_dst: Mac, target_mac: Mac, target_ip: Ip) {
        let p = &mut self.frame[14..42];
        p[0..2].copy_from_slice(&1u16.to_be_bytes()); // Ethernet
        p[2..4].copy_from_slice(&ETH_IPV4.to_be_bytes());
        p[4] = 6;
        p[5] = 4;
        p[6..8].copy_from_slice(&op.to_be_bytes());
        p[8..14].copy_from_slice(&self.mac);
        p[14..18].copy_from_slice(&self.ip);
        p[18..24].copy_from_slice(&target_mac);
        p[24..28].copy_from_slice(&target_ip);
        let _ = self.send_eth(eth_dst, ETH_ARP, 28);
    }

    /// The MAC address of `ip` on our network (ARP), within `timeout`.
    pub fn resolve(&mut self, ip: Ip, timeout: u64) -> Option<Mac> {
        self.arp(1, BROADCAST_MAC, [0; 6], ip);
        let end = ticks() + timeout;
        loop {
            let n = match net_recv(self.h, &mut self.rx) {
                Ok(n) => n,
                Err(Error::Again) => {
                    if ticks() >= end {
                        return None;
                    }
                    sleep(1);
                    continue;
                }
                Err(_) => return None,
            };
            if n < 42 || be16(&self.rx, 12) != ETH_ARP {
                continue;
            }
            let a = &self.rx[14..42];
            if be16(a, 6) == 2 && a[14..18] == ip {
                return a[8..14].try_into().ok();
            }
            self.answer_arp(n);
        }
    }

    /// Writes a DHCP message of `kind` (1 discover, 3 request) at
    /// `frame[42..]`; returns its length.
    fn dhcp_message(&mut self, xid: u32, kind: u8, offer: Option<(Ip, Ip)>) -> usize {
        let d = &mut self.frame[42..42 + 300];
        d.fill(0);
        d[0] = 1; // request
        d[1] = 1; // Ethernet
        d[2] = 6;
        d[4..8].copy_from_slice(&xid.to_be_bytes());
        d[10] = 0x80; // broadcast replies: we have no address yet
        d[28..34].copy_from_slice(&self.mac);
        d[236..240].copy_from_slice(&COOKIE);
        let mut at = 240;
        let mut opt = |code: u8, data: &[u8]| {
            d[at] = code;
            d[at + 1] = data.len() as u8;
            d[at + 2..at + 2 + data.len()].copy_from_slice(data);
            at += 2 + data.len();
        };
        opt(53, &[kind]);
        if let Some((ip, server)) = offer {
            opt(50, &ip);
            opt(54, &server);
        }
        opt(55, &[1, 3, 6, 51]); // mask, router, DNS, lease
        d[at] = 255;
        300
    }

    /// Gets an address by DHCP (discover, offer, request, ack) within
    /// `timeout` ticks per step; logs each step.
    pub fn dhcp(&mut self, timeout: u64) -> Result<Lease, &'static str> {
        let xid = 0x5A00_0000 | (ticks() as u32 & 0xFFFF);
        let len = self.dhcp_message(xid, 1, None);
        self.send_udp(
            BROADCAST_MAC,
            [0; 4],
            [255; 4],
            (DHCP_CLIENT, DHCP_SERVER),
            len,
        )
        .map_err(|_| "cannot send")?;
        let offer = self.dhcp_reply(xid, 2, timeout).ok_or("no DHCP offer")?;
        crate::log!(
            "net: DHCP offer {} from {}",
            IpText(offer.ip),
            IpText(offer.server)
        );
        let len = self.dhcp_message(xid, 3, Some((offer.ip, offer.server)));
        self.send_udp(
            BROADCAST_MAC,
            [0; 4],
            [255; 4],
            (DHCP_CLIENT, DHCP_SERVER),
            len,
        )
        .map_err(|_| "cannot send")?;
        let lease = self
            .dhcp_reply(xid, 5, timeout)
            .ok_or("no DHCP acknowledgement")?;
        self.ip = lease.ip;
        self.lease = lease;
        Ok(lease)
    }

    /// Waits for a DHCP reply of `kind` (2 offer, 5 ack) to `xid`.
    fn dhcp_reply(&mut self, xid: u32, kind: u8, timeout: u64) -> Option<Lease> {
        let p = self.wait_ip(timeout, |p, data| {
            p.proto == PROTO_UDP
                && data.len() >= 8 + 240
                && be16(data, 2) == DHCP_CLIENT
                && data[8] == 2
                && data[12..16] == xid.to_be_bytes()
                && data[8 + 236..8 + 240] == COOKIE
                && options(&data[8 + 240..], 53).is_some_and(|v| v.first() == Some(&kind))
        })?;
        let data = &self.rx[p.start + 8..p.end];
        let ip4 = |code| options(&data[240..], code).and_then(|v| v.get(..4)?.try_into().ok());
        Some(Lease {
            ip: data[16..20].try_into().ok()?,
            mask: ip4(1).unwrap_or([255, 255, 255, 0]),
            gateway: ip4(3).unwrap_or([0; 4]),
            dns: ip4(6).unwrap_or([0; 4]),
            server: ip4(54).unwrap_or(p.src),
            seconds: ip4(51).map_or(0, u32::from_be_bytes),
        })
    }

    /// Pings `ip` (on our network) once: the reply's round trip in ticks.
    pub fn ping(&mut self, dst_mac: Mac, ip: Ip, seq: u16, timeout: u64) -> Option<u64> {
        let id: u16 = 0x5A5A;
        let e = &mut self.frame[34..34 + 40];
        e.fill(0);
        e[0] = 8; // echo request
        e[4..6].copy_from_slice(&id.to_be_bytes());
        e[6..8].copy_from_slice(&seq.to_be_bytes());
        for (k, b) in e[8..].iter_mut().enumerate() {
            *b = b'a' + (k % 26) as u8;
        }
        let c = checksum(e);
        e[2..4].copy_from_slice(&c.to_be_bytes());
        let start = ticks();
        self.send_ip(dst_mac, self.ip, ip, PROTO_ICMP, 40).ok()?;
        self.wait_ip(timeout, |p, data| {
            p.proto == PROTO_ICMP
                && p.src == ip
                && data.len() >= 8
                && data[0] == 0
                && be16(data, 4) == id
                && be16(data, 6) == seq
        })?;
        Some(ticks() - start)
    }
}

// ---- TCP (docs/n2-design.md) ------------------------------------------

const PROTO_TCP: u8 = 6;
const TCP_FIN: u8 = 0x01;
const TCP_SYN: u8 = 0x02;
const TCP_RST: u8 = 0x04;
const TCP_PSH: u8 = 0x08;
const TCP_ACK: u8 = 0x10;
/// Largest segment we send (Ethernet's 1500 less the IPv4 and TCP
/// headers).
const MSS: usize = 1460;
/// Sends of one segment before giving up.
const TRIES: usize = 4;

/// One TCP connection: a client, data taken in order, one segment in
/// flight at a time (docs/n2-design.md).
pub struct Tcp {
    mac: Mac,
    ip: Ip,
    lport: u16,
    rport: u16,
    /// Next sequence number we send; the highest one acknowledged.
    snd: u32,
    una: u32,
    /// The next sequence number we expect.
    rcv: u32,
    /// Data received and not yet taken.
    pub inbox: Vec<u8>,
    /// The other side has finished sending (FIN received).
    pub closed: bool,
}

/// `a` is at or after `b`, in sequence-number order.
fn seq_ge(a: u32, b: u32) -> bool {
    a.wrapping_sub(b) as i32 >= 0
}

/// The TCP checksum of `seg` (header and data) from `src` to `dst`.
fn tcp_checksum(src: Ip, dst: Ip, seg: &[u8]) -> u16 {
    let mut sum = 0u32;
    for pair in src.chunks(2).chain(dst.chunks(2)) {
        sum += u32::from(u16::from_be_bytes([pair[0], pair[1]]));
    }
    sum += u32::from(PROTO_TCP) + seg.len() as u32;
    for pair in seg.chunks(2) {
        sum += u32::from(u16::from_be_bytes([pair[0], *pair.get(1).unwrap_or(&0)]));
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    !(sum as u16)
}

impl Stack {
    /// Sends one segment on `c`, at sequence number `seq`.
    fn tcp_send(&mut self, c: &Tcp, seq: u32, flags: u8, data: &[u8]) -> Result<(), Error> {
        let len = 20 + data.len();
        let t = &mut self.frame[34..34 + len];
        t[0..2].copy_from_slice(&c.lport.to_be_bytes());
        t[2..4].copy_from_slice(&c.rport.to_be_bytes());
        t[4..8].copy_from_slice(&seq.to_be_bytes());
        t[8..12].copy_from_slice(&c.rcv.to_be_bytes());
        t[12] = 5 << 4;
        t[13] = flags;
        t[14..16].copy_from_slice(&65535u16.to_be_bytes());
        t[16..20].fill(0);
        t[20..].copy_from_slice(data);
        let sum = tcp_checksum(self.ip, c.ip, t);
        t[16..18].copy_from_slice(&sum.to_be_bytes());
        self.send_ip(c.mac, self.ip, c.ip, PROTO_TCP, len)
    }

    /// Takes one segment of `c` (waiting up to `timeout`): records its
    /// acknowledgement, keeps in-order data, notes a FIN, and acknowledges
    /// what it received. False on timeout or reset.
    fn tcp_step(&mut self, c: &mut Tcp, timeout: u64) -> Result<bool, &'static str> {
        let (ip, lport, rport) = (c.ip, c.lport, c.rport);
        let Some(p) = self.wait_ip(timeout, |p, d| {
            p.proto == PROTO_TCP
                && p.src == ip
                && d.len() >= 20
                && be16(d, 0) == rport
                && be16(d, 2) == lport
        }) else {
            return Ok(false);
        };
        let d = &self.rx[p.start..p.end];
        let flags = d[13];
        if flags & TCP_RST != 0 {
            return Err("connection reset");
        }
        let seq = u32::from_be_bytes([d[4], d[5], d[6], d[7]]);
        let ack = u32::from_be_bytes([d[8], d[9], d[10], d[11]]);
        let off = (usize::from(d[12] >> 4) * 4).min(d.len());
        let data = &d[off..];
        if flags & TCP_ACK != 0 && seq_ge(ack, c.una) {
            c.una = ack;
        }
        let mut answer = false;
        if !data.is_empty() {
            if seq == c.rcv {
                c.inbox.extend_from_slice(data);
                c.rcv = c.rcv.wrapping_add(data.len() as u32);
            }
            // In order or not, say where we are.
            answer = true;
        }
        if flags & TCP_FIN != 0 && seq.wrapping_add(data.len() as u32) == c.rcv {
            c.rcv = c.rcv.wrapping_add(1);
            c.closed = true;
            answer = true;
        }
        if answer {
            self.tcp_send(c, c.snd, TCP_ACK, &[])
                .map_err(|_| "cannot send")?;
        }
        Ok(true)
    }

    /// Opens a connection to `ip`:`port`, sending through the machine at
    /// `mac` (the gateway, or `ip` itself on our network).
    pub fn connect(
        &mut self,
        mac: Mac,
        ip: Ip,
        port: u16,
        timeout: u64,
    ) -> Result<Tcp, &'static str> {
        let iss = (ticks() as u32).wrapping_mul(2_654_435_761) ^ 0x5A5A_0000;
        let mut c = Tcp {
            mac,
            ip,
            lport: 49152 + (ticks() % 16000) as u16,
            rport: port,
            snd: iss,
            una: iss,
            rcv: 0,
            inbox: Vec::new(),
            closed: false,
        };
        let (lport, rport) = (c.lport, c.rport);
        for _ in 0..TRIES {
            self.tcp_send(&c, iss, TCP_SYN, &[])
                .map_err(|_| "cannot send")?;
            let Some(p) = self.wait_ip(timeout, |p, d| {
                p.proto == PROTO_TCP
                    && p.src == ip
                    && d.len() >= 20
                    && be16(d, 0) == rport
                    && be16(d, 2) == lport
            }) else {
                continue;
            };
            let d = &self.rx[p.start..p.end];
            if d[13] & TCP_RST != 0 {
                return Err("connection refused");
            }
            let seq = u32::from_be_bytes([d[4], d[5], d[6], d[7]]);
            let ack = u32::from_be_bytes([d[8], d[9], d[10], d[11]]);
            if d[13] & (TCP_SYN | TCP_ACK) == TCP_SYN | TCP_ACK && ack == iss.wrapping_add(1) {
                c.snd = ack;
                c.una = ack;
                c.rcv = seq.wrapping_add(1);
                self.tcp_send(&c, c.snd, TCP_ACK, &[])
                    .map_err(|_| "cannot send")?;
                return Ok(c);
            }
        }
        Err("no answer")
    }

    /// Sends `data` a segment at a time, each acknowledged before the next
    /// (each resent up to `TRIES` times).
    pub fn write(&mut self, c: &mut Tcp, data: &[u8], timeout: u64) -> Result<(), &'static str> {
        for chunk in data.chunks(MSS) {
            let start = c.snd;
            let want = start.wrapping_add(chunk.len() as u32);
            c.snd = want;
            let mut tries = 0;
            while !seq_ge(c.una, want) {
                if tries == TRIES {
                    return Err("no acknowledgement");
                }
                tries += 1;
                self.tcp_send(c, start, TCP_ACK | TCP_PSH, chunk)
                    .map_err(|_| "cannot send")?;
                let end = ticks() + timeout;
                while !seq_ge(c.una, want) && ticks() < end {
                    if !self.tcp_step(c, end - ticks())? {
                        break;
                    }
                }
            }
        }
        Ok(())
    }

    /// Receives until the other side finishes, or nothing arrives for
    /// `idle` ticks; returns what came.
    pub fn read_to_end(&mut self, c: &mut Tcp, idle: u64) -> Result<Vec<u8>, &'static str> {
        while !c.closed {
            if !self.tcp_step(c, idle)? {
                break;
            }
        }
        Ok(core::mem::take(&mut c.inbox))
    }

    /// Finishes our side (FIN) and waits briefly for it to be acknowledged.
    pub fn close(&mut self, c: &mut Tcp, timeout: u64) {
        let fin = c.snd;
        c.snd = c.snd.wrapping_add(1);
        if self.tcp_send(c, fin, TCP_FIN | TCP_ACK, &[]).is_err() {
            return;
        }
        let end = ticks() + timeout;
        while !seq_ge(c.una, c.snd) && ticks() < end {
            if !matches!(self.tcp_step(c, end - ticks()), Ok(true)) {
                break;
            }
        }
    }
}

/// The value of DHCP option `code` in `opts`.
fn options(opts: &[u8], code: u8) -> Option<&[u8]> {
    let mut at = 0;
    while at < opts.len() {
        match opts[at] {
            255 => return None,
            0 => at += 1,
            c => {
                let len = usize::from(*opts.get(at + 1)?);
                let v = opts.get(at + 2..at + 2 + len)?;
                if c == code {
                    return Some(v);
                }
                at += 2 + len;
            }
        }
    }
    None
}

/// An address, written `a.b.c.d`.
pub struct IpText(pub Ip);

impl core::fmt::Display for IpText {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        let [a, b, c, d] = self.0;
        write!(f, "{a}.{b}.{c}.{d}")
    }
}

/// A MAC address, written `aa:bb:cc:dd:ee:ff`.
pub struct MacText(pub Mac);

impl core::fmt::Display for MacText {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        let m = self.0;
        write!(
            f,
            "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            m[0], m[1], m[2], m[3], m[4], m[5]
        )
    }
}
