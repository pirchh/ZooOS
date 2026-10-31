//! USB host controller (xHCI), first step (docs/h1-design.md): find the
//! controller, take it from the firmware, start it, and enumerate the
//! devices on its root ports (address each one, read its device and
//! configuration descriptors, log what it is). Polled, no interrupts, one
//! command ring, one event ring. HID keyboards and mice in boot protocol
//! (docs/h1b-design.md) are polled on every timer tick; their reports feed
//! the same keyboard and mouse queues as the PS/2 devices.

use crate::kprintln;
use crate::memory::Memory;
use spin::Mutex;
use x86_64::PhysAddr;
use x86_64::instructions::port::Port;
use x86_64::structures::paging::PhysFrame;

const PCI_ADDR: u16 = 0xCF8;
const PCI_DATA: u16 = 0xCFC;
/// Serial bus controller / USB / xHCI.
const CLASS_XHCI: u32 = 0x0C_03_30;

/// Bounded polling: iterations before an operation counts as timed out.
const SPINS: u64 = 50_000_000;
/// TRBs per ring page (16 bytes each); the last one links back.
const RING_TRBS: usize = 256;
/// Devices handled (root ports enumerated).
const MAX_PORTS: usize = 16;

// TRB types.
const TRB_NORMAL: u32 = 1;
const TRB_SETUP: u32 = 2;
const TRB_DATA: u32 = 3;
const TRB_STATUS: u32 = 4;
const TRB_LINK: u32 = 6;
const TRB_ENABLE_SLOT: u32 = 9;
const TRB_ADDRESS_DEVICE: u32 = 11;
const TRB_CONFIGURE_ENDPOINT: u32 = 12;
const TRB_TRANSFER_EVENT: u32 = 32;
const TRB_COMMAND_COMPLETION: u32 = 33;

const CC_SUCCESS: u32 = 1;
const CC_SHORT_PACKET: u32 = 13;

// PORTSC bits.
const PORT_CCS: u32 = 1 << 0;
const PORT_PED: u32 = 1 << 1;
const PORT_PR: u32 = 1 << 4;
const PORT_PRC: u32 = 1 << 21;
/// Bits that keep their value when PORTSC is written back: CCS, OCA,
/// PLS, PP, speed, PIC, CAS, the wake enables and DR. The write-1-to-clear
/// bits (PED and the change bits) and PR are left 0.
const PORT_NEUTRAL: u32 = 0x4F00_FFE9;
/// Largest register space mapped (pages).
const MAX_MMIO_PAGES: u64 = 64;

fn pci_read(bus: u8, dev: u8, func: u8, off: u8) -> u32 {
    let addr = 0x8000_0000u32
        | (u32::from(bus) << 16)
        | (u32::from(dev) << 11)
        | (u32::from(func) << 8)
        | u32::from(off & 0xFC);
    // SAFETY: PCI configuration mechanism #1; selecting an address and
    // reading it has no other effect.
    unsafe {
        Port::<u32>::new(PCI_ADDR).write(addr);
        Port::<u32>::new(PCI_DATA).read()
    }
}

fn pci_write(bus: u8, dev: u8, func: u8, off: u8, value: u32) {
    let addr = 0x8000_0000u32
        | (u32::from(bus) << 16)
        | (u32::from(dev) << 11)
        | (u32::from(func) << 8)
        | u32::from(off & 0xFC);
    // SAFETY: as in `pci_read`; only the xHCI controller's command register
    // is written.
    unsafe {
        Port::<u32>::new(PCI_ADDR).write(addr);
        Port::<u32>::new(PCI_DATA).write(value);
    }
}

/// A register block mapped uncached at `base`.
#[derive(Clone, Copy)]
struct Regs {
    base: u64,
}

impl Regs {
    fn r32(self, off: u64) -> u32 {
        // SAFETY: `base` is the controller's mapped register space (see
        // `init`) and every offset used is inside it.
        unsafe { core::ptr::read_volatile((self.base + off) as *const u32) }
    }
    fn w32(self, off: u64, v: u32) {
        // SAFETY: as in `r32`.
        unsafe { core::ptr::write_volatile((self.base + off) as *mut u32, v) }
    }
    fn w64(self, off: u64, v: u64) {
        self.w32(off, v as u32);
        self.w32(off + 4, (v >> 32) as u32);
    }
}

/// Polls until `f` holds; false on timeout.
fn wait_until(mut f: impl FnMut() -> bool) -> bool {
    for _ in 0..SPINS {
        if f() {
            return true;
        }
        core::hint::spin_loop();
    }
    false
}

/// One zeroed page of DMA memory: (physical address, kernel pointer).
fn page(mem: &mut Memory) -> Option<(u64, *mut u8)> {
    let frame: PhysFrame = mem.frames.allocate()?;
    let phys = frame.start_address().as_u64();
    let virt = mem.phys_to_virt(PhysAddr::new(phys));
    // SAFETY: a frame just taken from the allocator, mapped at `virt`
    // through the physical-memory window; nothing else uses it.
    unsafe { core::ptr::write_bytes(virt, 0, 4096) };
    Some((phys, virt))
}

/// A producer ring (command or transfer): one page, the last TRB a link.
struct Ring {
    phys: u64,
    virt: *mut u32,
    index: usize,
    cycle: u32,
}

impl Ring {
    fn new(mem: &mut Memory) -> Option<Ring> {
        let (phys, virt) = page(mem)?;
        let ring = Ring {
            phys,
            virt: virt as *mut u32,
            index: 0,
            cycle: 1,
        };
        // Link TRB back to the start, toggling the cycle bit.
        ring.write(
            RING_TRBS - 1,
            [phys as u32, (phys >> 32) as u32, 0, TRB_LINK << 10 | 1 << 1],
        );
        Some(ring)
    }

    /// Writes TRB `i`; the control word (with the cycle bit) goes last.
    fn write(&self, i: usize, t: [u32; 4]) {
        // SAFETY: `virt` is this ring's page and `i < RING_TRBS`.
        unsafe {
            let p = self.virt.add(i * 4);
            core::ptr::write_volatile(p, t[0]);
            core::ptr::write_volatile(p.add(1), t[1]);
            core::ptr::write_volatile(p.add(2), t[2]);
            core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
            core::ptr::write_volatile(p.add(3), t[3]);
        }
    }

    /// Queues a TRB (the cycle bit is added here); returns its address.
    fn push(&mut self, param: u64, status: u32, control: u32) -> u64 {
        let at = self.phys + (self.index * 16) as u64;
        self.write(
            self.index,
            [
                param as u32,
                (param >> 32) as u32,
                status,
                control | self.cycle,
            ],
        );
        self.index += 1;
        if self.index == RING_TRBS - 1 {
            // Hand the link TRB to the controller, then wrap.
            let link = TRB_LINK << 10 | 1 << 1 | self.cycle;
            self.write(
                RING_TRBS - 1,
                [self.phys as u32, (self.phys >> 32) as u32, 0, link],
            );
            self.index = 0;
            self.cycle ^= 1;
        }
        at
    }
}

/// The event ring (one segment) and where we are in it.
struct Events {
    phys: u64,
    virt: *mut u32,
    index: usize,
    cycle: u32,
}

/// HID devices polled after start-up.
const MAX_HIDS: usize = 4;

#[derive(Clone, Copy, PartialEq, Eq)]
enum HidKind {
    Keyboard,
    Mouse,
}

/// A HID boot-protocol device's interrupt IN endpoint.
struct Hid {
    kind: HidKind,
    slot: u32,
    /// Device context index of the endpoint (its doorbell target).
    dci: u32,
    ring: Ring,
    buf_phys: u64,
    buf: *mut u8,
    /// Bytes asked for per report.
    len: u32,
    /// The previous keyboard report (to find presses and releases).
    last: [u8; 8],
    /// Reports received.
    reports: u64,
}

struct Xhci {
    op: Regs,
    rt: Regs,
    db: Regs,
    ctx_size: usize,
    dcbaa: *mut u64,
    cmd: Ring,
    events: Events,
    hids: [Option<Hid>; MAX_HIDS],
}

// SAFETY: the raw pointers point into pages owned by this driver (DMA
// memory and the mapped registers); the controller is only ever used
// through `HC`'s lock, from one CPU.
unsafe impl Send for Xhci {}

/// The controller after start-up, polled from the timer tick.
static HC: Mutex<Option<Xhci>> = Mutex::new(None);

impl Xhci {
    /// The next event, if the controller has written one.
    fn next_event(&mut self) -> Option<[u32; 4]> {
        // SAFETY: the event ring page; `index < RING_TRBS`.
        let t = unsafe {
            let p = self.events.virt.add(self.events.index * 4);
            let control = core::ptr::read_volatile(p.add(3));
            if control & 1 != self.events.cycle {
                return None;
            }
            core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
            [
                core::ptr::read_volatile(p),
                core::ptr::read_volatile(p.add(1)),
                core::ptr::read_volatile(p.add(2)),
                control,
            ]
        };
        self.events.index += 1;
        if self.events.index == RING_TRBS {
            self.events.index = 0;
            self.events.cycle ^= 1;
        }
        // Tell the controller how far we have read (EHB cleared).
        let erdp = self.events.phys + (self.events.index * 16) as u64;
        self.rt.w64(0x20 + 0x18, erdp | 1 << 3);
        Some(t)
    }

    /// Queues the next report transfer on a HID endpoint.
    fn queue_report(&mut self, i: usize) {
        if let Some(h) = self.hids[i].as_mut() {
            // Normal TRB: interrupt on completion and on a short packet.
            h.ring
                .push(h.buf_phys, h.len, TRB_NORMAL << 10 | 1 << 5 | 1 << 2);
            let (slot, dci) = (h.slot, h.dci);
            self.db.w32(4 * u64::from(slot), dci);
        }
    }

    /// Handles finished HID transfers (timer tick).
    fn poll(&mut self) {
        while let Some(e) = self.next_event() {
            if (e[3] >> 10) & 0x3F != TRB_TRANSFER_EVENT {
                continue;
            }
            let (slot, dci) = (e[3] >> 24, (e[3] >> 16) & 0x1F);
            let Some(i) = self
                .hids
                .iter()
                .position(|h| h.as_ref().is_some_and(|h| h.slot == slot && h.dci == dci))
            else {
                continue;
            };
            if matches!(e[2] >> 24, CC_SUCCESS | CC_SHORT_PACKET)
                && let Some(h) = self.hids[i].as_mut()
            {
                let got = h.len.saturating_sub(e[2] & 0xFF_FFFF) as usize;
                let mut report = [0u8; 8];
                // SAFETY: the endpoint's own buffer page, `got <= len <= 8`.
                for (k, b) in report.iter_mut().enumerate().take(got.min(8)) {
                    *b = unsafe { h.buf.add(k).read_volatile() };
                }
                if h.reports == 0 {
                    crate::serial_println!(
                        "ZOO_OS INFO usb {} input",
                        if h.kind == HidKind::Keyboard {
                            "keyboard"
                        } else {
                            "mouse"
                        }
                    );
                }
                h.reports += 1;
                match h.kind {
                    HidKind::Keyboard => keyboard_report(&mut h.last, &report),
                    HidKind::Mouse => crate::mouse::push_event(
                        i16::from(report[1] as i8),
                        // USB reports y downwards; the queue holds y upwards.
                        -i16::from(report[2] as i8),
                        report[0] & 7,
                    ),
                }
            }
            self.queue_report(i);
        }
    }

    /// Waits for an event of `kind`, skipping others (port changes).
    fn wait_event(&mut self, kind: u32) -> Option<[u32; 4]> {
        for _ in 0..SPINS {
            if let Some(t) = self.next_event()
                && (t[3] >> 10) & 0x3F == kind
            {
                return Some(t);
            }
            core::hint::spin_loop();
        }
        None
    }

    /// Runs one command; returns the completion event's slot id.
    fn command(&mut self, param: u64, control: u32) -> Result<u32, &'static str> {
        self.cmd.push(param, 0, control);
        self.db.w32(0, 0);
        let e = self
            .wait_event(TRB_COMMAND_COMPLETION)
            .ok_or("command timed out")?;
        if e[2] >> 24 != CC_SUCCESS {
            return Err("command failed");
        }
        Ok(e[3] >> 24)
    }
}

/// What a device said about itself.
struct Device {
    vendor: u16,
    product: u16,
    class: u8,
}

/// Finds the first xHCI controller: scans bus 0 and every bus behind a
/// PCI-to-PCI bridge, instead of all 256 buses (each configuration read is
/// a slow port access under virtualisation).
fn find_controller() -> Option<(u8, u8, u8)> {
    let mut buses = [0u8; 32];
    let mut queued = 1;
    let mut next = 0;
    while next < queued {
        let bus = buses[next];
        next += 1;
        for dev in 0..32u8 {
            // No function 0: no device. Functions 1-7 only exist on
            // multi-function devices (header type bit 7).
            if pci_read(bus, dev, 0, 0) & 0xFFFF == 0xFFFF {
                continue;
            }
            let funcs = if pci_read(bus, dev, 0, 0x0C) & (1 << 23) != 0 {
                8
            } else {
                1
            };
            for func in 0..funcs {
                if pci_read(bus, dev, func, 0) & 0xFFFF == 0xFFFF {
                    continue;
                }
                let class = pci_read(bus, dev, func, 8) >> 8;
                if class == CLASS_XHCI {
                    return Some((bus, dev, func));
                }
                // A PCI-to-PCI bridge: its secondary bus comes later.
                if class >> 8 == 0x06_04 && queued < buses.len() {
                    let secondary = (pci_read(bus, dev, func, 0x18) >> 8) as u8;
                    if secondary != 0 && !buses[..queued].contains(&secondary) {
                        buses[queued] = secondary;
                        queued += 1;
                    }
                }
            }
        }
    }
    None
}

/// Finds an xHCI controller and enumerates its root ports. Returns how
/// many devices were enumerated, or `None` without a controller.
pub fn init(mem: &mut Memory) -> Option<usize> {
    let (bus, dev, func) = find_controller()?;
    let bar0 = pci_read(bus, dev, func, 0x10);
    let mut phys = u64::from(bar0 & !0xF);
    if (bar0 >> 1) & 3 == 2 {
        phys |= u64::from(pci_read(bus, dev, func, 0x14)) << 32;
    }
    // Size of the register space: write all ones to the BAR, read back,
    // restore (with decoding off meanwhile).
    let cmd = pci_read(bus, dev, func, 0x04);
    pci_write(bus, dev, func, 0x04, cmd & !0x3);
    pci_write(bus, dev, func, 0x10, 0xFFFF_FFFF);
    let mask = pci_read(bus, dev, func, 0x10) & !0xF;
    pci_write(bus, dev, func, 0x10, bar0);
    let size = u64::from((!mask).wrapping_add(1)).max(4096);
    let pages = size.div_ceil(4096).min(MAX_MMIO_PAGES);
    // Memory space and bus mastering (DMA).
    pci_write(bus, dev, func, 0x04, cmd | 0x6);
    let Ok(base) = mem.map_mmio(phys, pages) else {
        kprintln!("ZOO_OS INFO usb error=cannot-map-registers");
        return None;
    };
    match start(mem, Regs { base }) {
        Ok(n) => Some(n),
        Err(why) => {
            kprintln!("ZOO_OS INFO usb error={}", why);
            Some(0)
        }
    }
}

fn start(mem: &mut Memory, cap: Regs) -> Result<usize, &'static str> {
    let caplen = u64::from(cap.r32(0) & 0xFF);
    let hcs1 = cap.r32(0x04);
    let hcs2 = cap.r32(0x08);
    let hcc1 = cap.r32(0x10);
    let op = Regs {
        base: cap.base + caplen,
    };
    let db = Regs {
        base: cap.base + u64::from(cap.r32(0x14) & !0x3),
    };
    let rt = Regs {
        base: cap.base + u64::from(cap.r32(0x18) & !0x1F),
    };
    let max_slots = (hcs1 & 0xFF).min(32);
    let ports = ((hcs1 >> 24) as usize).min(MAX_PORTS);
    let ctx_size = if hcc1 & (1 << 2) != 0 { 64 } else { 32 };

    take_from_firmware(cap, hcc1);

    // Stop, then reset.
    op.w32(0, op.r32(0) & !1);
    if !wait_until(|| op.r32(4) & 1 != 0) {
        return Err("halt-timeout");
    }
    op.w32(0, 1 << 1);
    if !wait_until(|| op.r32(0) & (1 << 1) == 0 && op.r32(4) & (1 << 11) == 0) {
        return Err("reset-timeout");
    }
    op.w32(0x38, max_slots);

    // Device context base address array, with scratchpad buffers if the
    // controller asks for them.
    let (dcbaa_phys, dcbaa) = page(mem).ok_or("out-of-memory")?;
    let dcbaa = dcbaa as *mut u64;
    let scratch = (((hcs2 >> 21) & 0x1F) << 5 | (hcs2 >> 27)) as usize;
    if scratch > 0 {
        let (arr_phys, arr) = page(mem).ok_or("out-of-memory")?;
        for k in 0..scratch.min(512) {
            let (p, _) = page(mem).ok_or("out-of-memory")?;
            // SAFETY: `arr` is a zeroed page of 512 entries.
            unsafe { (arr as *mut u64).add(k).write_volatile(p) };
        }
        // SAFETY: entry 0 of the DCBAA page.
        unsafe { dcbaa.write_volatile(arr_phys) };
    }
    op.w64(0x30, dcbaa_phys);

    let cmd = Ring::new(mem).ok_or("out-of-memory")?;
    op.w64(0x18, cmd.phys | 1);

    // Event ring: one segment, and its one-entry segment table.
    let (seg_phys, seg) = page(mem).ok_or("out-of-memory")?;
    let (erst_phys, erst) = page(mem).ok_or("out-of-memory")?;
    // SAFETY: the zeroed ERST page; entry 0 is base (8 bytes), size (4).
    unsafe {
        (erst as *mut u64).write_volatile(seg_phys);
        (erst.add(8) as *mut u32).write_volatile(RING_TRBS as u32);
    }
    let ir = 0x20;
    rt.w32(ir + 0x08, 1);
    rt.w64(ir + 0x18, seg_phys);
    rt.w64(ir + 0x10, erst_phys);

    // Run.
    op.w32(0, op.r32(0) | 1);
    if !wait_until(|| op.r32(4) & 1 == 0) {
        return Err("start-timeout");
    }
    kprintln!(
        "ZOO_OS INFO usb xhci ports={} slots={} context={}",
        ports,
        max_slots,
        ctx_size
    );

    let mut hc = Xhci {
        op,
        rt,
        db,
        ctx_size,
        dcbaa,
        cmd,
        events: Events {
            phys: seg_phys,
            virt: seg as *mut u32,
            index: 0,
            cycle: 1,
        },
        hids: [const { None }; MAX_HIDS],
    };
    let mut count = 0;
    for port in 1..=ports {
        let portsc = 0x400 + 0x10 * (port as u64 - 1);
        if hc.op.r32(portsc) & PORT_CCS == 0 {
            continue;
        }
        match enumerate(mem, &mut hc, port, portsc) {
            Ok(n) => count += n,
            Err(why) => kprintln!("ZOO_OS INFO usb port={} error={}", port, why),
        }
    }
    kprintln!("ZOO_OS INFO usb devices={}", count);
    // Start the HID endpoints, then hand the controller to the timer.
    for i in 0..MAX_HIDS {
        hc.queue_report(i);
    }
    *HC.lock() = Some(hc);
    Ok(count)
}

/// Timer tick: HID reports that have arrived (docs/h1b-design.md).
pub fn poll() {
    if let Some(mut hc) = HC.try_lock()
        && let Some(hc) = hc.as_mut()
    {
        hc.poll();
    }
}

/// The USB legacy-support capability: ask the firmware to let go.
fn take_from_firmware(cap: Regs, hcc1: u32) {
    let mut off = u64::from(hcc1 >> 16) * 4;
    while off != 0 {
        let v = cap.r32(off);
        if v & 0xFF == 1 {
            cap.w32(off, v | 1 << 24);
            if !wait_until(|| cap.r32(off) & (1 << 16) == 0) {
                kprintln!("ZOO_OS INFO usb firmware did not release the controller");
            }
            return;
        }
        let next = u64::from((v >> 8) & 0xFF) * 4;
        if next == 0 {
            return;
        }
        off += next;
    }
}

/// Where a device is: its root port, the hub port it hangs off (if any),
/// and what the slot context needs for it.
#[derive(Clone, Copy)]
struct Location {
    root: usize,
    hub_port: Option<usize>,
    /// Route string: hub port numbers below the root port, 4 bits a tier.
    route: u32,
    speed: u32,
    /// (hub slot, hub port) when a full/low-speed device sits behind a
    /// high-speed hub's transaction translator.
    tt: Option<(u32, u32)>,
}

impl core::fmt::Display for Location {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self.hub_port {
            Some(p) => write!(f, "{}.{}", self.root, p),
            None => write!(f, "{}", self.root),
        }
    }
}

/// Resets a root port and sets up the device on it (and, for a hub, the
/// devices behind it). Returns how many devices were set up.
fn enumerate(
    mem: &mut Memory,
    hc: &mut Xhci,
    port: usize,
    portsc: u64,
) -> Result<usize, &'static str> {
    let v = hc.op.r32(portsc);
    // USB 3 ports enable themselves; USB 2 ports need a reset.
    if v & PORT_PED == 0 {
        hc.op.w32(portsc, (v & PORT_NEUTRAL) | PORT_PR);
        if !wait_until(|| hc.op.r32(portsc) & PORT_PRC != 0) {
            return Err("port-reset-timeout");
        }
        let v = hc.op.r32(portsc);
        hc.op.w32(portsc, (v & PORT_NEUTRAL) | PORT_PRC);
        if hc.op.r32(portsc) & PORT_PED == 0 {
            return Err("port-not-enabled");
        }
    }
    let at = Location {
        root: port,
        hub_port: None,
        route: 0,
        speed: (hc.op.r32(portsc) >> 10) & 0xF,
        tt: None,
    };
    attach(mem, hc, at)
}

/// Addresses the device at `at`, reads its descriptors, logs it, and sets
/// up a HID interface or, for a hub on a root port, the hub's devices.
fn attach(mem: &mut Memory, hc: &mut Xhci, at: Location) -> Result<usize, &'static str> {
    let speed = at.speed;
    let max_packet: u32 = match speed {
        3 => 64,      // high speed
        4 | 5 => 512, // super speed
        _ => 8,       // full and low speed
    };

    let slot = hc.command(0, TRB_ENABLE_SLOT << 10)?;
    if slot == 0 {
        return Err("no-slot");
    }
    let (out_phys, _) = page(mem).ok_or("out-of-memory")?;
    // SAFETY: entry `slot` of the DCBAA page (slots are at most 32).
    unsafe { hc.dcbaa.add(slot as usize).write_volatile(out_phys) };

    // Input context: control, slot and endpoint 0 contexts.
    let mut ep0 = Ring::new(mem).ok_or("out-of-memory")?;
    let (in_phys, input) = page(mem).ok_or("out-of-memory")?;
    let cs = hc.ctx_size;
    let put = |off: usize, v: u32| {
        // SAFETY: inside the zeroed input-context page.
        unsafe { (input.add(off) as *mut u32).write_volatile(v) }
    };
    put(4, 0b11); // add the slot and endpoint 0 contexts
    put(cs, at.route | speed << 20 | 1 << 27);
    put(cs + 4, (at.root as u32) << 16);
    if let Some((hub_slot, hub_port)) = at.tt {
        put(cs + 8, hub_slot | hub_port << 8);
    }
    put(2 * cs + 4, 3 << 1 | 4 << 3 | max_packet << 16);
    put(2 * cs + 8, ep0.phys as u32 | 1);
    put(2 * cs + 12, (ep0.phys >> 32) as u32);
    put(2 * cs + 16, 8);
    hc.command(in_phys, TRB_ADDRESS_DEVICE << 10 | slot << 24)?;

    let (buf_phys, buf) = page(mem).ok_or("out-of-memory")?;
    control_in(hc, &mut ep0, slot, 0x0100, buf_phys, 18)?;
    // SAFETY: the zeroed buffer page the descriptor was read into.
    let d = unsafe { core::slice::from_raw_parts(buf, 64) };
    if d[1] != 1 {
        return Err("bad-device-descriptor");
    }
    let mut device = Device {
        vendor: u16::from_le_bytes([d[8], d[9]]),
        product: u16::from_le_bytes([d[10], d[11]]),
        class: d[4],
    };
    let port = at;
    // The configuration: report HID interfaces (keyboard 1, mouse 2) and
    // find the first boot-protocol one with an interrupt IN endpoint.
    control_in(hc, &mut ep0, slot, 0x0200, buf_phys, 255)?;
    // SAFETY: as above, up to the 255 bytes requested.
    let c = unsafe { core::slice::from_raw_parts(buf, 255) };
    let total = usize::from(u16::from_le_bytes([c[2], c[3]])).min(255);
    let config = c[5];
    // (interface, kind) of the boot HID interface being read, then its
    // endpoint (address, max packet, interval).
    let mut hid: Option<(u8, HidKind)> = None;
    let mut endpoint: Option<(u8, u16, u8)> = None;
    let mut at = 0;
    while at + 2 <= total && c[at] >= 2 {
        if c[at + 1] == 4 && at + 8 <= total {
            let (class, sub, proto) = (c[at + 5], c[at + 6], c[at + 7]);
            if device.class == 0 {
                device.class = class;
            }
            if class == 3 {
                let kind = match (sub, proto) {
                    (1, 1) => "keyboard",
                    (1, 2) => "mouse",
                    _ => "other",
                };
                kprintln!("ZOO_OS INFO usb port={} hid={}", port, kind);
                if hid.is_none() && endpoint.is_none() {
                    hid = match (sub, proto) {
                        (1, 1) => Some((c[at + 2], HidKind::Keyboard)),
                        (1, 2) => Some((c[at + 2], HidKind::Mouse)),
                        _ => None,
                    };
                }
            }
        } else if c[at + 1] == 5 && at + 7 <= total && hid.is_some() && endpoint.is_none() {
            let (addr, attrs) = (c[at + 2], c[at + 3]);
            if addr & 0x80 != 0 && attrs & 3 == 3 {
                endpoint = Some((addr, u16::from_le_bytes([c[at + 4], c[at + 5]]), c[at + 6]));
            }
        }
        at += usize::from(c[at]);
    }
    kprintln!(
        "ZOO_OS INFO usb port={} vendor={:#06x} product={:#06x} class={}",
        port,
        device.vendor,
        device.product,
        device.class
    );
    if let (Some((iface, kind)), Some(ep)) = (hid, endpoint) {
        let setup = HidSetup {
            slot,
            at: port,
            config,
            iface,
            ep,
        };
        match start_hid(mem, hc, &mut ep0, &setup, kind) {
            Ok(()) => kprintln!("ZOO_OS INFO usb port={} input ready", port),
            Err(why) => kprintln!("ZOO_OS INFO usb port={} input error={}", port, why),
        }
    }
    // A hub on a root port: its own ports (one tier of hubs).
    if device.class == 9 && port.hub_port.is_none() {
        return match hub(mem, hc, &mut ep0, slot, port, config, buf_phys, buf) {
            Ok(n) => Ok(1 + n),
            Err(why) => {
                kprintln!("ZOO_OS INFO usb port={} hub error={}", port, why);
                Ok(1)
            }
        };
    }
    Ok(1)
}

/// Hub class requests (USB 2.0 chapter 11).
const HUB_PORT_RESET: u64 = 4;
const HUB_PORT_POWER: u64 = 8;
const HUB_C_PORT_CONNECTION: u64 = 16;
const HUB_C_PORT_RESET: u64 = 20;

/// Sets up a hub: marks its slot as a hub, powers and resets its ports,
/// and attaches the device on each connected port. Returns how many.
#[allow(clippy::too_many_arguments)]
fn hub(
    mem: &mut Memory,
    hc: &mut Xhci,
    ep0: &mut Ring,
    slot: u32,
    at: Location,
    config: u8,
    buf_phys: u64,
    buf: *mut u8,
) -> Result<usize, &'static str> {
    // The hub descriptor: number of ports, power-on time.
    control_get(hc, ep0, slot, 0xA0 | 6 << 8 | 0x29 << 24, buf_phys, 9)?;
    // SAFETY: the buffer page the descriptor was read into.
    let (ports, power_2ms) = unsafe { (buf.add(2).read_volatile(), buf.add(5).read_volatile()) };
    let ports = u32::from(ports).min(15);
    control_nodata(hc, ep0, slot, 9 << 8 | u64::from(config) << 16)?;

    // Tell the controller this slot is a hub (Configure Endpoint, slot
    // context only): Hub flag and number of ports.
    let (in_phys, input) = page(mem).ok_or("out-of-memory")?;
    let cs = hc.ctx_size;
    let put = |off: usize, v: u32| {
        // SAFETY: inside the zeroed input-context page.
        unsafe { (input.add(off) as *mut u32).write_volatile(v) }
    };
    put(4, 1);
    put(cs, at.route | at.speed << 20 | 1 << 26 | 1 << 27);
    put(cs + 4, (at.root as u32) << 16 | ports << 24);
    hc.command(in_phys, TRB_CONFIGURE_ENDPOINT << 10 | slot << 24)?;

    for p in 1..=u64::from(ports) {
        control_nodata(
            hc,
            ep0,
            slot,
            0x23 | 3 << 8 | HUB_PORT_POWER << 16 | p << 32,
        )?;
    }
    // Power-good time (in 2 ms units) as spins; ticks do not run here.
    spin(u64::from(power_2ms.max(10)) * 200_000);

    let mut count = 0;
    for p in 1..=ports {
        let (status, _) = port_status(hc, ep0, slot, p, buf_phys, buf)?;
        if status & 1 == 0 {
            continue;
        }
        let w = u64::from(p) << 32;
        control_nodata(
            hc,
            ep0,
            slot,
            0x23 | 1 << 8 | HUB_C_PORT_CONNECTION << 16 | w,
        )?;
        control_nodata(hc, ep0, slot, 0x23 | 3 << 8 | HUB_PORT_RESET << 16 | w)?;
        let mut reset = false;
        for _ in 0..1000 {
            let (_, change) = port_status(hc, ep0, slot, p, buf_phys, buf)?;
            if change & (1 << 4) != 0 {
                reset = true;
                break;
            }
            spin(100_000);
        }
        if !reset {
            kprintln!(
                "ZOO_OS INFO usb port={}.{} error=hub-port-reset-timeout",
                at,
                p
            );
            continue;
        }
        control_nodata(hc, ep0, slot, 0x23 | 1 << 8 | HUB_C_PORT_RESET << 16 | w)?;
        let (status, _) = port_status(hc, ep0, slot, p, buf_phys, buf)?;
        if status & (1 << 1) == 0 {
            kprintln!(
                "ZOO_OS INFO usb port={}.{} error=hub-port-not-enabled",
                at,
                p
            );
            continue;
        }
        // Hub port status speed bits → xHCI speed ids.
        let speed = if status & (1 << 9) != 0 {
            2 // low
        } else if status & (1 << 10) != 0 {
            3 // high
        } else {
            1 // full
        };
        let child = Location {
            root: at.root,
            hub_port: Some(p as usize),
            route: at.route | p,
            speed,
            // Full/low speed behind a high-speed hub: its translator.
            tt: (at.speed == 3 && speed != 3).then_some((slot, p)),
        };
        match attach(mem, hc, child) {
            Ok(n) => count += n,
            Err(why) => kprintln!("ZOO_OS INFO usb port={} error={}", child, why),
        }
    }
    Ok(count)
}

/// A hub port's (wPortStatus, wPortChange).
fn port_status(
    hc: &mut Xhci,
    ep0: &mut Ring,
    slot: u32,
    port: u32,
    buf_phys: u64,
    buf: *mut u8,
) -> Result<(u16, u16), &'static str> {
    control_get(hc, ep0, slot, 0xA3 | u64::from(port) << 32, buf_phys, 4)?;
    // SAFETY: the 4 bytes just read into the buffer page.
    let b = unsafe { [0, 1, 2, 3].map(|k| buf.add(k).read_volatile()) };
    Ok((
        u16::from_le_bytes([b[0], b[1]]),
        u16::from_le_bytes([b[2], b[3]]),
    ))
}

fn spin(n: u64) {
    for _ in 0..n {
        core::hint::spin_loop();
    }
}

/// Where a HID interface is and what its endpoint looks like.
struct HidSetup {
    slot: u32,
    at: Location,
    config: u8,
    iface: u8,
    /// (endpoint address, max packet size, interval)
    ep: (u8, u16, u8),
}

/// Configures the device, selects the boot protocol, and adds the
/// interrupt IN endpoint (Configure Endpoint); polling starts after
/// enumeration.
fn start_hid(
    mem: &mut Memory,
    hc: &mut Xhci,
    ep0: &mut Ring,
    s: &HidSetup,
    kind: HidKind,
) -> Result<(), &'static str> {
    let free = hc
        .hids
        .iter()
        .position(Option::is_none)
        .ok_or("too-many-hid-devices")?;
    let iface = u64::from(s.iface);
    // SET_CONFIGURATION, SET_PROTOCOL(boot), SET_IDLE(0: only changes).
    control_nodata(hc, ep0, s.slot, 9 << 8 | u64::from(s.config) << 16)?;
    control_nodata(hc, ep0, s.slot, 0x21 | 0x0B << 8 | iface << 32)?;
    // Some devices refuse SET_IDLE; that is allowed.
    let _ = control_nodata(hc, ep0, s.slot, 0x21 | 0x0A << 8 | iface << 32);

    let (addr, mps, interval) = s.ep;
    let dci = u32::from(addr & 0xF) * 2 + 1;
    let mps = u32::from(mps & 0x7FF).max(1);
    // xHCI interval: 2^n x 125 us. Full/low speed give milliseconds,
    // high and super speed give 2^(bInterval-1) x 125 us.
    let n = if s.at.speed >= 3 {
        u32::from(interval.clamp(1, 16)) - 1
    } else {
        (u32::from(interval.max(1)) * 8).ilog2().clamp(3, 10)
    };
    let ring = Ring::new(mem).ok_or("out-of-memory")?;
    let (in_phys, input) = page(mem).ok_or("out-of-memory")?;
    let cs = hc.ctx_size;
    let put = |off: usize, v: u32| {
        // SAFETY: inside the zeroed input-context page.
        unsafe { (input.add(off) as *mut u32).write_volatile(v) }
    };
    put(4, 1 | 1 << dci); // slot context and the new endpoint
    put(cs, s.at.route | s.at.speed << 20 | dci << 27);
    put(cs + 4, (s.at.root as u32) << 16);
    if let Some((hub_slot, hub_port)) = s.at.tt {
        put(cs + 8, hub_slot | hub_port << 8);
    }
    let ep = (dci as usize + 1) * cs;
    put(ep, n << 16);
    put(ep + 4, 3 << 1 | 7 << 3 | mps << 16); // interrupt IN
    put(ep + 8, ring.phys as u32 | 1);
    put(ep + 12, (ring.phys >> 32) as u32);
    put(ep + 16, mps | mps << 16);
    hc.command(in_phys, TRB_CONFIGURE_ENDPOINT << 10 | s.slot << 24)?;

    let (buf_phys, buf) = page(mem).ok_or("out-of-memory")?;
    hc.hids[free] = Some(Hid {
        kind,
        slot: s.slot,
        dci,
        ring,
        buf_phys,
        buf,
        len: mps.min(8),
        last: [0; 8],
        reports: 0,
    });
    Ok(())
}

/// A control request without a data stage on endpoint 0.
fn control_nodata(
    hc: &mut Xhci,
    ep0: &mut Ring,
    slot: u32,
    setup: u64,
) -> Result<(), &'static str> {
    ep0.push(setup, 8, TRB_SETUP << 10 | 1 << 6);
    // No data: the status stage is IN.
    let status = ep0.push(0, 0, TRB_STATUS << 10 | 1 << 5 | 1 << 16);
    hc.db.w32(4 * u64::from(slot), 1);
    loop {
        let e = hc
            .wait_event(TRB_TRANSFER_EVENT)
            .ok_or("transfer-timeout")?;
        let at = u64::from(e[0]) | u64::from(e[1]) << 32;
        match e[2] >> 24 {
            CC_SUCCESS if at == status => return Ok(()),
            CC_SUCCESS | CC_SHORT_PACKET => continue,
            _ => return Err("request-failed"),
        }
    }
}

/// Scancode set 1 for HID keyboard usages 0x04..=0x52; 0 = none, bit 8
/// set = extended (sent after 0xE0).
const USAGE_TO_SET1: [u16; 0x53] = {
    let mut t = [0u16; 0x53];
    let letters = [
        0x1E, 0x30, 0x2E, 0x20, 0x12, 0x21, 0x22, 0x23, 0x17, 0x24, 0x25, 0x26, 0x32, 0x31, 0x18,
        0x19, 0x10, 0x13, 0x1F, 0x14, 0x16, 0x2F, 0x11, 0x2D, 0x15, 0x2C,
    ];
    let mut i = 0;
    while i < 26 {
        t[0x04 + i] = letters[i];
        i += 1;
    }
    // 1-9, 0
    let mut d = 0;
    while d < 10 {
        t[0x1E + d] = 0x02 + d as u16;
        d += 1;
    }
    t[0x28] = 0x1C; // Enter
    t[0x29] = 0x01; // Escape
    t[0x2A] = 0x0E; // Backspace
    t[0x2B] = 0x0F; // Tab
    t[0x2C] = 0x39; // Space
    t[0x2D] = 0x0C; // -
    t[0x2E] = 0x0D; // =
    t[0x2F] = 0x1A; // [
    t[0x30] = 0x1B; // ]
    t[0x31] = 0x2B; // backslash
    t[0x33] = 0x27; // ;
    t[0x34] = 0x28; // '
    t[0x35] = 0x29; // `
    t[0x36] = 0x33; // ,
    t[0x37] = 0x34; // .
    t[0x38] = 0x35; // /
    t[0x39] = 0x3A; // Caps Lock
    let mut f = 0;
    while f < 10 {
        t[0x3A + f] = 0x3B + f as u16; // F1-F10
        f += 1;
    }
    t[0x44] = 0x57; // F11
    t[0x45] = 0x58; // F12
    t[0x4F] = 0x14D; // Right
    t[0x50] = 0x14B; // Left
    t[0x51] = 0x150; // Down
    t[0x52] = 0x148; // Up
    t
};

/// Set-1 codes of the modifier bits: LCtrl, LShift, LAlt, LGUI, RCtrl,
/// RShift, RAlt, RGUI.
const MODIFIERS: [u16; 8] = [0x1D, 0x2A, 0x38, 0x15B, 0x11D, 0x36, 0x138, 0x15C];

fn send(code: u16, released: bool) {
    if code & 0x100 != 0 {
        crate::keyboard::push(0xE0);
    }
    crate::keyboard::push((code & 0x7F) as u8 | if released { 0x80 } else { 0 });
}

/// Turns a boot-protocol keyboard report (modifiers, reserved, six key
/// usages) into set-1 press and release codes, like a PS/2 keyboard.
fn keyboard_report(last: &mut [u8; 8], report: &[u8; 8]) {
    // 0x01 in the key array: too many keys pressed; nothing reliable.
    if report[2..].contains(&1) {
        return;
    }
    for (bit, &code) in MODIFIERS.iter().enumerate() {
        let (was, is) = (last[0] >> bit & 1, report[0] >> bit & 1);
        if was != is {
            send(code, is == 0);
        }
    }
    for &u in &last[2..] {
        if u != 0
            && !report[2..].contains(&u)
            && let Some(&code) = USAGE_TO_SET1.get(usize::from(u))
            && code != 0
        {
            send(code, true);
        }
    }
    for &u in &report[2..] {
        if u != 0
            && !last[2..].contains(&u)
            && let Some(&code) = USAGE_TO_SET1.get(usize::from(u))
            && code != 0
        {
            send(code, false);
        }
    }
    *last = *report;
}

/// GET_DESCRIPTOR(`value`) into `buf` (`len` bytes) on endpoint 0.
fn control_in(
    hc: &mut Xhci,
    ep0: &mut Ring,
    slot: u32,
    value: u16,
    buf: u64,
    len: u16,
) -> Result<(), &'static str> {
    control_get(
        hc,
        ep0,
        slot,
        0x80 | 6 << 8 | u64::from(value) << 16,
        buf,
        len,
    )
}

/// A control request with an IN data stage: `setup` without its length
/// field, which `len` fills in.
fn control_get(
    hc: &mut Xhci,
    ep0: &mut Ring,
    slot: u32,
    setup: u64,
    buf: u64,
    len: u16,
) -> Result<(), &'static str> {
    let setup = setup | u64::from(len) << 48;
    ep0.push(setup, 8, TRB_SETUP << 10 | 1 << 6 | 3 << 16);
    ep0.push(buf, u32::from(len), TRB_DATA << 10 | 1 << 16);
    let status = ep0.push(0, 0, TRB_STATUS << 10 | 1 << 5);
    hc.db.w32(4 * u64::from(slot), 1);
    // Done when the status stage completes; an event for an earlier TRB
    // (a short data stage) only counts if it reports an error.
    loop {
        let e = hc
            .wait_event(TRB_TRANSFER_EVENT)
            .ok_or("transfer-timeout")?;
        let at = u64::from(e[0]) | u64::from(e[1]) << 32;
        match e[2] >> 24 {
            CC_SUCCESS | CC_SHORT_PACKET if at == status => return Ok(()),
            CC_SUCCESS | CC_SHORT_PACKET => continue,
            _ => return Err("transfer-failed"),
        }
    }
}
