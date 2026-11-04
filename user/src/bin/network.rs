//! Network: the network window (docs/n3-design.md). It takes the network
//! device, gets an address by DHCP and shows the connection. Type a name or
//! an address and press Enter to fetch `http://<it>/`: its status and its
//! text (tags left out) are shown.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write as _;
use zoo_abi::ui::mark;
use zoo_rt::app::{Event, Key, MAX_LINES, Window};
use zoo_rt::net::{IpText, Lease, Mac, MacText, Stack, net_attach};
use zoo_rt::{Error, Line, recv};

zoo_rt::entry!(main);

/// Ticks to wait for each reply (100 ticks = 1 s).
const WAIT: u64 = 300;
/// Longest address typed.
const MAX_INPUT: usize = 60;
/// Lines of fetched text shown.
const SHOWN: usize = MAX_LINES - 5;
/// Characters per shown line.
const WRAP: usize = 80;

struct Net {
    stack: Stack,
    lease: Lease,
    /// The machine that carries traffic off our network.
    gateway: (Mac, [u8; 4]),
}

struct App {
    win: Window,
    net: Option<Net>,
    status: String,
    details: String,
    input: String,
    result: Vec<String>,
}

fn main(channel: u64) -> u8 {
    let mut app = App {
        win: Window::new(channel),
        net: None,
        status: String::from("Connecting..."),
        details: String::new(),
        input: String::new(),
        result: Vec::new(),
    };
    if app.draw().is_err() {
        return 0;
    }
    app.connect();
    let mut buf = [0u8; zoo_abi::MAX_MESSAGE];
    loop {
        if app.draw().is_err() {
            return 0;
        }
        let n = match recv(channel, &mut buf) {
            Ok(n) => n,
            Err(Error::PeerClosed) => return 0,
            Err(_) => return 4,
        };
        // Bella's `browse` (docs/actions-design.md): as if typed and Enter.
        if let Some(Event::Do(action, target)) = Event::parse(&buf[..n]) {
            let said = if action == "fetch" && !target.trim().is_empty() {
                app.input = String::from(target.trim());
                let target = app.input.clone();
                app.fetch(&target);
                let first = app.result.first().map_or("", |l| l.as_str());
                alloc::format!("Fetched {}: {}", target, first.trim())
            } else {
                alloc::format!("Error: Network cannot {}.", action)
            };
            let _ = app.win.did(&said);
            continue;
        }
        match Key::parse(&buf[..n]) {
            Some(Key::Char(c)) if app.input.len() < MAX_INPUT && !c.is_control() => {
                app.input.push(c)
            }
            Some(Key::Back) => {
                app.input.pop();
            }
            Some(Key::Enter) => {
                let target = String::from(app.input.trim());
                if !target.is_empty() {
                    app.fetch(&target);
                }
            }
            _ => {}
        }
    }
}

impl App {
    fn connect(&mut self) {
        let h = match net_attach() {
            Ok(h) => h,
            Err(e) => {
                self.status = String::from("No network");
                self.details =
                    alloc::format!("The network device is not available ({}).", e.name());
                zoo_rt::log!("Network: no network device: {}", e.name());
                return;
            }
        };
        let Ok(mut stack) = Stack::new(h) else {
            self.status = String::from("No network");
            return;
        };
        let lease = match stack.dhcp(WAIT) {
            Ok(l) => l,
            Err(why) => {
                self.status = String::from("Not connected");
                self.details = alloc::format!("No address: {}.", why);
                zoo_rt::log!("Network: DHCP failed: {}", why);
                return;
            }
        };
        // QEMU's isolated network gives no router: its DHCP server is it.
        let gw_ip = if lease.gateway == [0; 4] {
            lease.server
        } else {
            lease.gateway
        };
        let Some(gw_mac) = stack.resolve(gw_ip, WAIT) else {
            self.status = String::from("Not connected");
            self.details = alloc::format!("The gateway {} does not answer.", IpText(gw_ip));
            return;
        };
        self.status = alloc::format!("Connected  {}", IpText(lease.ip));
        self.details = alloc::format!(
            "Gateway {}   DNS {}   {}",
            IpText(gw_ip),
            if lease.dns == [0; 4] {
                String::from("none")
            } else {
                alloc::format!("{}", IpText(lease.dns))
            },
            MacText(stack.mac)
        );
        zoo_rt::log!(
            "Network: connected {} via {}",
            IpText(lease.ip),
            IpText(gw_ip)
        );
        self.net = Some(Net {
            stack,
            lease,
            gateway: (gw_mac, gw_ip),
        });
    }

    /// Fetches `http://<target>/` and shows the answer.
    fn fetch(&mut self, target: &str) {
        self.result.clear();
        if self.net.is_none() {
            self.result.push(String::from("Not connected."));
            return;
        }
        self.result
            .push(alloc::format!("Fetching http://{}/ ...", target));
        let _ = self.draw();
        let Some(net) = self.net.as_mut() else {
            return;
        };
        let (mac, _) = net.gateway;
        let dns = if net.lease.dns == [0; 4] {
            net.gateway.1
        } else {
            net.lease.dns
        };
        let ip = match net.stack.lookup(mac, dns, target, WAIT) {
            Ok(ip) => ip,
            Err(why) => {
                self.result = alloc::vec![alloc::format!("Cannot find {}: {}.", target, why)];
                return;
            }
        };
        let result = (|| {
            let mut c = net.stack.connect(mac, ip, 80, WAIT)?;
            let req = alloc::format!(
                "GET / HTTP/1.0\r\nHost: {}\r\nUser-Agent: ZooOS\r\nConnection: close\r\n\r\n",
                target
            );
            net.stack.write(&mut c, req.as_bytes(), WAIT)?;
            let reply = net.stack.read_to_end(&mut c, WAIT)?;
            net.stack.close(&mut c, WAIT);
            Ok::<_, &'static str>(reply)
        })();
        let reply = match result {
            Ok(r) => r,
            Err(why) => {
                self.result = alloc::vec![alloc::format!("{} ({}): {}.", target, IpText(ip), why)];
                return;
            }
        };
        let text = String::from_utf8_lossy(&reply);
        let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
        let status = head.lines().next().unwrap_or("").trim();
        zoo_rt::log!(
            "Network: fetched http://{}/ ({}): {} bytes, `{}`",
            target,
            IpText(ip),
            reply.len(),
            status
        );
        self.result = alloc::vec![alloc::format!(
            "{}  {}  {} bytes",
            IpText(ip),
            status,
            reply.len()
        )];
        for line in plain_text(body).lines().filter(|l| !l.trim().is_empty()) {
            let mut l: String = line.trim().chars().filter(|c| !c.is_control()).collect();
            while !l.is_empty() && self.result.len() < SHOWN {
                let cut = l.char_indices().nth(WRAP).map_or(l.len(), |(i, _)| i);
                let rest = l.split_off(cut);
                self.result.push(l);
                l = rest;
            }
        }
    }

    fn draw(&self) -> Result<(), Error> {
        self.win.title("Network")?;
        let mut l = Line::new();
        let _ = write!(
            l,
            "{}{}",
            mark::COLOUR,
            if self.net.is_some() { 'f' } else { 'd' }
        );
        let _ = l.write_str(&self.status);
        self.win.line(0, l.as_str())?;
        let mut d = Line::new();
        let _ = write!(d, "{}d{}", mark::COLOUR, self.details);
        self.win.line(1, d.as_str())?;
        self.win.line(2, "")?;
        let mut input = Line::new();
        let _ = write!(input, "Address: {}{}", self.input, mark::CARET);
        self.win.line(3, input.as_str())?;
        for (k, r) in self.result.iter().take(SHOWN).enumerate() {
            self.win.line(4 + k, r)?;
        }
        self.win.lines(4 + self.result.len().min(SHOWN))?;
        self.win.select(None)?;
        self.win.show()
    }
}

/// HTML as text: tags and scripts left out, a few entities decoded, block
/// ends as new lines.
fn plain_text(html: &str) -> String {
    let mut out = String::new();
    let mut rest = html;
    while let Some(lt) = rest.find('<') {
        out.push_str(&rest[..lt]);
        let tag_end = rest[lt..].find('>').map_or(rest.len(), |i| lt + i + 1);
        let tag = rest[lt..tag_end].to_ascii_lowercase();
        rest = &rest[tag_end..];
        // Skip what scripts and styles contain.
        for skip in ["script", "style"] {
            if tag.starts_with(&alloc::format!("<{skip}")) {
                let close = alloc::format!("</{skip}");
                rest = rest
                    .to_ascii_lowercase()
                    .find(&close)
                    .map_or("", |i| &rest[i..]);
            }
        }
        if [
            "<p", "<br", "<div", "<h", "</p", "</h", "<li", "<tr", "<title", "</title",
        ]
        .iter()
        .any(|t| tag.starts_with(t))
        {
            out.push('\n');
        }
    }
    out.push_str(rest);
    out.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
}
