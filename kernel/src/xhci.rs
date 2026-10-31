//! USB host controller (xHCI), first step (docs/h1-design.md): find the
//! controller, take it from the firmware, start it, and enumerate the
//! devices on its root ports (address each one, read its device and
//! configuration descriptors, log what it is). Polled, no interrupts, one
//! command ring, one event ring, control endpoint 0 only. Keyboard and
//! mouse input from USB HID devices is the next step.

use crate::kprintln;
use crate::memory::Memory;
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
const TRB_SETUP: u32 = 2;
const TRB_DATA: u32 = 3;
const TRB_STATUS: u32 = 4;
const TRB_LINK: u32 = 6;
const TRB_ENABLE_SLOT: u32 = 9;
const TRB_ADDRESS_DEVICE: u32 = 11;
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

struct Xhci {
    op: Regs,
    rt: Regs,
    db: Regs,
    ctx_size: usize,
    dcbaa: *mut u64,
    cmd: Ring,
    events: Events,
}

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
    };
    let mut count = 0;
    for port in 1..=ports {
        let portsc = 0x400 + 0x10 * (port as u64 - 1);
        if hc.op.r32(portsc) & PORT_CCS == 0 {
            continue;
        }
        match enumerate(mem, &mut hc, port, portsc) {
            Ok(d) => {
                count += 1;
                kprintln!(
                    "ZOO_OS INFO usb port={} vendor={:#06x} product={:#06x} class={}",
                    port,
                    d.vendor,
                    d.product,
                    d.class
                );
            }
            Err(why) => kprintln!("ZOO_OS INFO usb port={} error={}", port, why),
        }
    }
    kprintln!("ZOO_OS INFO usb devices={}", count);
    Ok(count)
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

/// Resets a port, gives its device an address and reads its descriptors.
fn enumerate(
    mem: &mut Memory,
    hc: &mut Xhci,
    port: usize,
    portsc: u64,
) -> Result<Device, &'static str> {
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
    let speed = (hc.op.r32(portsc) >> 10) & 0xF;
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
    put(cs, speed << 20 | 1 << 27);
    put(cs + 4, (port as u32) << 16);
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
    // The configuration: report HID interfaces (keyboard 1, mouse 2).
    control_in(hc, &mut ep0, slot, 0x0200, buf_phys, 255)?;
    // SAFETY: as above, up to the 255 bytes requested.
    let c = unsafe { core::slice::from_raw_parts(buf, 255) };
    let total = usize::from(u16::from_le_bytes([c[2], c[3]])).min(255);
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
            }
        }
        at += usize::from(c[at]);
    }
    Ok(device)
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
    let setup = 0x80u64 | 6 << 8 | u64::from(value) << 16 | u64::from(len) << 48;
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
