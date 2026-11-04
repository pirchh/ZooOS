//! virtio-gpu over the modern virtio PCI transport (docs/gpu1-design.md).
//!
//! QEMU's `virtio-vga` is a VGA device with a virtio-gpu behind it. The
//! firmware, the bootloader and the kernel console use the VGA side as an
//! ordinary linear framebuffer. When a process takes the display, the
//! device is switched to virtio-gpu: the screen becomes a resource whose
//! memory is a contiguous block of RAM (mapped into the display owner), and
//! the owner pushes each changed rectangle with `flush`. QEMU then shows it
//! at once instead of on its display refresh timer. A second resource is a
//! hardware cursor: QEMU gives its image to the host window system, so
//! moving it needs no redraw at all. Releasing the display resets the
//! device, which puts it back in VGA mode for the console.
//!
//! Requests are synchronous: each waits (bounded) for the device to finish.
//! No interrupts are used.

use crate::kprintln;
use crate::memory::Memory;
use spin::Mutex;
use x86_64::instructions::port::Port;

const PCI_ADDR: u16 = 0xCF8;
const PCI_DATA: u16 = 0xCFC;
const VIRTIO_VENDOR: u32 = 0x1AF4;
/// virtio device id 16 (GPU) on the modern transport: 0x1040 + 16.
const VIRTIO_GPU: u32 = 0x1050;

// Common configuration structure (virtio 1.x §4.1.4.3).
const DEVICE_FEATURE_SELECT: u64 = 0x00;
const DEVICE_FEATURE: u64 = 0x04;
const DRIVER_FEATURE_SELECT: u64 = 0x08;
const DRIVER_FEATURE: u64 = 0x0C;
const MSIX_CONFIG: u64 = 0x10;
const DEVICE_STATUS: u64 = 0x14;
const QUEUE_SELECT: u64 = 0x16;
const QUEUE_SIZE: u64 = 0x18;
const QUEUE_MSIX_VECTOR: u64 = 0x1A;
const QUEUE_ENABLE: u64 = 0x1C;
const QUEUE_NOTIFY_OFF: u64 = 0x1E;
const QUEUE_DESC: u64 = 0x20;
const QUEUE_DRIVER: u64 = 0x28;
const QUEUE_DEVICE: u64 = 0x30;

const STATUS_ACK: u8 = 1;
const STATUS_DRIVER: u8 = 2;
const STATUS_DRIVER_OK: u8 = 4;
const STATUS_FEATURES_OK: u8 = 8;
/// No MSI-X vector: requests are polled.
const NO_VECTOR: u16 = 0xFFFF;

// Commands and responses (virtio 1.x §5.7.6).
const CMD_GET_DISPLAY_INFO: u32 = 0x0100;
const CMD_RESOURCE_CREATE_2D: u32 = 0x0101;
const CMD_SET_SCANOUT: u32 = 0x0103;
const CMD_RESOURCE_FLUSH: u32 = 0x0104;
const CMD_TRANSFER_TO_HOST_2D: u32 = 0x0105;
const CMD_RESOURCE_ATTACH_BACKING: u32 = 0x0106;
const CMD_UPDATE_CURSOR: u32 = 0x0300;
const CMD_MOVE_CURSOR: u32 = 0x0301;
const RESP_OK_NODATA: u32 = 0x1100;
const RESP_OK_DISPLAY_INFO: u32 = 0x1101;

/// Little-endian 0x00RRGGBB in memory: bytes B, G, R, X.
const FORMAT_B8G8R8X8: u32 = 2;
/// The cursor's format: bytes B, G, R, A.
const FORMAT_B8G8R8A8: u32 = 1;

const SCREEN_RESOURCE: u32 = 1;
const CURSOR_RESOURCE: u32 = 2;
/// The cursor image is CURSOR_SIZE x CURSOR_SIZE pixels.
pub const CURSOR_SIZE: usize = 64;

/// Entries per virtqueue (two descriptors a request).
const QUEUE_ENTRIES: u16 = 16;
/// Requests in flight at once on one queue (each has a 512-byte slot).
const SLOTS: usize = 4;
const SLOT: usize = 512;
/// Bounded wait for the device (iterations of a status read).
const SPINS: u64 = 200_000_000;
/// The largest screen used: the window server's limit (1920 x 1200
/// logical) at 2x.
const MAX_W: u32 = 3840;
const MAX_H: u32 = 2400;
/// The smallest screen used: the desktop's design size.
const MIN_W: u32 = 1920;
const MIN_H: u32 = 1080;
/// Without a size from the host, at most this (the preferred mode is a
/// QEMU window's size, which only says how large the window is now).
const DEFAULT_MAX: (u32, u32) = (1920, 1200);

/// A screen size the host asked for with `-fw_cfg name=opt/zooos/display,
/// string=WxH` (docs/u2b-design.md): `xtask run` passes the host monitor's
/// size, so a 4K monitor gets a 4K screen drawn at 2x.
/// How many monitors the host gives (`opt/zooos/monitors`, 1 to
/// `MAX_MONITORS`), if it says (docs/monitors-design.md).
fn host_monitors() -> Option<u32> {
    let mut buf = [0u8; 8];
    let n = crate::fw_cfg::read_file("opt/zooos/monitors", &mut buf)?;
    let m: u32 = core::str::from_utf8(&buf[..n]).ok()?.trim().parse().ok()?;
    Some(m.clamp(1, MAX_MONITORS))
}

/// Most monitors side by side.
const MAX_MONITORS: u32 = 4;

fn host_size() -> Option<(u32, u32)> {
    let mut buf = [0u8; 32];
    let n = crate::fw_cfg::read_file("opt/zooos/display", &mut buf)?;
    let text = core::str::from_utf8(&buf[..n]).ok()?.trim();
    let (w, h) = text.split_once('x')?;
    Some((w.parse().ok()?, h.parse().ok()?))
}

fn pci_read(dev: u8, func: u8, off: u8) -> u32 {
    let addr = 0x8000_0000 | u32::from(dev) << 11 | u32::from(func) << 8 | u32::from(off & 0xFC);
    // SAFETY: PCI configuration mechanism 1: write the address, then read
    // the data port; only bus 0 configuration space is touched.
    unsafe {
        Port::<u32>::new(PCI_ADDR).write(addr);
        Port::<u32>::new(PCI_DATA).read()
    }
}

fn pci_write(dev: u8, func: u8, off: u8, value: u32) {
    let addr = 0x8000_0000 | u32::from(dev) << 11 | u32::from(func) << 8 | u32::from(off & 0xFC);
    // SAFETY: as in `pci_read`, for this driver's own device.
    unsafe {
        Port::<u32>::new(PCI_ADDR).write(addr);
        Port::<u32>::new(PCI_DATA).write(value);
    }
}

/// Mapped device registers.
#[derive(Clone, Copy)]
struct Regs(u64);

impl Regs {
    fn r8(self, off: u64) -> u8 {
        // SAFETY: inside a register block mapped by `map_cap` for this
        // driver; every offset used is inside its structure.
        unsafe { core::ptr::read_volatile((self.0 + off) as *const u8) }
    }
    fn w8(self, off: u64, v: u8) {
        // SAFETY: as in `r8`.
        unsafe { core::ptr::write_volatile((self.0 + off) as *mut u8, v) }
    }
    fn r16(self, off: u64) -> u16 {
        // SAFETY: as in `r8`.
        unsafe { core::ptr::read_volatile((self.0 + off) as *const u16) }
    }
    fn w16(self, off: u64, v: u16) {
        // SAFETY: as in `r8`.
        unsafe { core::ptr::write_volatile((self.0 + off) as *mut u16, v) }
    }
    fn r32(self, off: u64) -> u32 {
        // SAFETY: as in `r8`.
        unsafe { core::ptr::read_volatile((self.0 + off) as *const u32) }
    }
    fn w32(self, off: u64, v: u32) {
        // SAFETY: as in `r8`.
        unsafe { core::ptr::write_volatile((self.0 + off) as *mut u32, v) }
    }
    fn w64(self, off: u64, v: u64) {
        self.w32(off, v as u32);
        self.w32(off + 4, (v >> 32) as u32);
    }
}

/// One split virtqueue in a page of its own, with request and response
/// pages: slot `k` uses descriptors `2k` and `2k + 1`.
struct Queue {
    index: u16,
    size: u16,
    /// Physical and kernel addresses of the ring page.
    ring_phys: u64,
    ring: *mut u8,
    req_phys: u64,
    req: *mut u8,
    resp_phys: u64,
    resp: *mut u8,
    avail_idx: u16,
    notify: u64,
}

const DESC: usize = 0;
const AVAIL: usize = 1024;
const USED: usize = 2048;

impl Queue {
    /// Puts `n` requests (already written to slots 0..n) on the queue,
    /// tells the device, and waits for all of them. Returns each slot's
    /// response type.
    fn run(&mut self, lens: &[usize], resp_len: usize) -> Result<[u32; SLOTS], &'static str> {
        let n = lens.len().min(SLOTS);
        for (k, &len) in lens.iter().take(n).enumerate() {
            let d = 2 * k;
            // SAFETY: the ring page belongs to this queue; descriptors
            // `d`, `d + 1` < 2 * SLOTS <= size; avail entries < size.
            unsafe {
                let desc = self.ring.add(DESC) as *mut u64;
                // addr, len | flags << 32 | next << 48
                desc.add(2 * d)
                    .write_volatile(self.req_phys + (k * SLOT) as u64);
                desc.add(2 * d + 1)
                    .write_volatile(len as u64 | 1 << 32 | ((d + 1) as u64) << 48);
                desc.add(2 * (d + 1))
                    .write_volatile(self.resp_phys + (k * SLOT) as u64);
                desc.add(2 * (d + 1) + 1)
                    .write_volatile(resp_len as u64 | 2 << 32);
                let avail = self.ring.add(AVAIL) as *mut u16;
                let slot = self.avail_idx.wrapping_add(k as u16) % self.size;
                avail.add(2 + usize::from(slot)).write_volatile(d as u16);
            }
        }
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        self.avail_idx = self.avail_idx.wrapping_add(n as u16);
        // SAFETY: avail ring header: flags (u16), idx (u16).
        unsafe { (self.ring.add(AVAIL + 2) as *mut u16).write_volatile(self.avail_idx) };
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        // SAFETY: this queue's notify register in the mapped notify block.
        unsafe { core::ptr::write_volatile(self.notify as *mut u16, self.index) };
        let mut done = false;
        for _ in 0..SPINS {
            // SAFETY: used ring header: flags (u16), idx (u16).
            let used = unsafe { (self.ring.add(USED + 2) as *const u16).read_volatile() };
            if used == self.avail_idx {
                done = true;
                break;
            }
            core::hint::spin_loop();
        }
        if !done {
            return Err("gpu-request-timeout");
        }
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        let mut types = [0u32; SLOTS];
        for (k, t) in types.iter_mut().enumerate().take(n) {
            // SAFETY: response slot `k` in this queue's response page.
            *t = unsafe { (self.resp.add(k * SLOT) as *const u32).read_volatile() };
        }
        Ok(types)
    }

    /// Request slot `k` as 32-bit words, zeroed.
    fn slot(&mut self, k: usize) -> &mut [u32] {
        // SAFETY: slot `k` < SLOTS of this queue's own request page; no
        // request is in flight while it is written (`run` waits).
        let s = unsafe {
            core::slice::from_raw_parts_mut(self.req.add(k * SLOT) as *mut u32, SLOT / 4)
        };
        s.fill(0);
        s
    }
}

struct Gpu {
    common: Regs,
    notify_base: u64,
    notify_mult: u32,
    queues: [Queue; 2],
    /// The whole screen: `monitors` monitors side by side, each
    /// `width / monitors` wide (docs/monitors-design.md).
    width: u32,
    height: u32,
    monitors: u32,
    /// The screen's memory: contiguous frames (physical start).
    screen_phys: u64,
    cursor_phys: u64,
    cursor: *mut u8,
    /// The device shows the screen resource (a display owner holds it).
    active: bool,
}

// SAFETY: the raw pointers point at this driver's own frames and mapped
// registers; the driver is only used through `GPU`'s lock on one CPU.
unsafe impl Send for Gpu {}

static GPU: Mutex<Option<Gpu>> = Mutex::new(None);

/// A capability's structure, mapped: (registers, length).
fn map_cap(mem: &mut Memory, dev: u8, func: u8, cap: u8) -> Option<Regs> {
    let bar = (pci_read(dev, func, cap + 4) & 0xFF) as u8;
    let offset = u64::from(pci_read(dev, func, cap + 8));
    let length = u64::from(pci_read(dev, func, cap + 12));
    if bar > 5 {
        return None;
    }
    let reg = 0x10 + 4 * bar;
    let lo = pci_read(dev, func, reg);
    if lo & 1 != 0 {
        return None; // an I/O BAR: not used here
    }
    let mut phys = u64::from(lo & !0xF);
    if (lo >> 1) & 3 == 2 {
        phys |= u64::from(pci_read(dev, func, reg + 4)) << 32;
    }
    let start = (phys + offset) & !0xFFF;
    let end = phys + offset + length.max(1);
    let pages = (end - start).div_ceil(4096);
    let virt = mem.map_mmio(start, pages).ok()?;
    Some(Regs(virt + (phys + offset - start)))
}

/// Finds a `virtio-vga`/`virtio-gpu` device and sets it up, without
/// taking the screen from the VGA side yet. Returns the screen size.
pub fn init(mem: &mut Memory) -> Option<(u32, u32)> {
    let mut found = None;
    'scan: for dev in 0..32u8 {
        for func in 0..8u8 {
            let id = pci_read(dev, func, 0);
            if id & 0xFFFF == VIRTIO_VENDOR && id >> 16 == VIRTIO_GPU {
                found = Some((dev, func));
                break 'scan;
            }
        }
    }
    let (dev, func) = found?;
    // Memory space and bus mastering.
    let cmd = pci_read(dev, func, 0x04);
    pci_write(dev, func, 0x04, (cmd & 0xFFFF) | 0x6);
    // Vendor capabilities (id 9): common (1), notify (2) configuration.
    let (mut common, mut notify) = (None, None);
    let mut at = (pci_read(dev, func, 0x34) & 0xFC) as u8;
    for _ in 0..48 {
        if at < 0x40 {
            break;
        }
        let head = pci_read(dev, func, at);
        if head & 0xFF == 9 {
            match (head >> 24) as u8 {
                1 if common.is_none() => common = Some(at),
                2 if notify.is_none() => notify = Some(at),
                _ => {}
            }
        }
        at = ((head >> 8) & 0xFC) as u8;
    }
    let common = map_cap(mem, dev, func, common?)?;
    let notify_cap = notify?;
    let notify_mult = pci_read(dev, func, notify_cap + 16);
    let notify_base = map_cap(mem, dev, func, notify_cap)?.0;

    let mut queues = [new_queue(mem, 0)?, new_queue(mem, 1)?];
    if let Err(why) = start_device(common, notify_base, notify_mult, &mut queues) {
        kprintln!("ZOO_OS INFO gpu error={}", why);
        return None;
    }
    // The preferred mode of scanout 0.
    let q = &mut queues[0];
    q.slot(0)[0] = CMD_GET_DISPLAY_INFO;
    let (mut w, mut h) = (0, 0);
    let mut enabled = 0;
    if let Ok(t) = q.run(&[24], 24 + 16 * 24)
        && t[0] == RESP_OK_DISPLAY_INFO
    {
        // SAFETY: response slot 0: header (24 bytes), then pmodes[0..16]:
        // x, y, width, height, enabled, flags.
        let p = unsafe { core::slice::from_raw_parts(q.resp.add(24) as *const u32, 6) };
        if p[4] != 0 {
            (w, h) = (p[2], p[3]);
        }
        for k in 0..MAX_MONITORS as usize {
            // SAFETY: pmodes[k], k < 16, inside the response slot.
            let m =
                unsafe { core::slice::from_raw_parts(q.resp.add(24 + 24 * k) as *const u32, 6) };
            enabled += u32::from(m[4] != 0);
        }
    }
    // The host's count, else the scanouts it shows (docs/monitors-design.md).
    let monitors = host_monitors().unwrap_or(enabled.max(1));
    // The host's size if it gave one. Otherwise the preferred mode, at
    // least the desktop's design size (QEMU's GTK window reports its own
    // size as the preferred mode, 640x480 before any guest mode is set,
    // docs/gpu1-design.md) and at most 1920x1200.
    let (w, h) = match host_size() {
        Some((hw, hh)) => {
            kprintln!("ZOO_OS INFO gpu host size {}x{}", hw, hh);
            (hw.clamp(MIN_W, MAX_W), hh.clamp(MIN_H, MAX_H))
        }
        None => (w.clamp(MIN_W, DEFAULT_MAX.0), h.clamp(MIN_H, DEFAULT_MAX.1)),
    };
    // One screen resource for every monitor, side by side; each scanout
    // shows its part.
    let w = w * monitors;
    let bytes = (w as usize) * (h as usize) * 4;
    let screen = mem.frames.allocate_contiguous(bytes.div_ceil(4096))?;
    let cursor = mem
        .frames
        .allocate_contiguous((CURSOR_SIZE * CURSOR_SIZE * 4).div_ceil(4096))?;
    let screen_virt = mem.phys_to_virt(screen.start_address());
    let cursor_virt = mem.phys_to_virt(cursor.start_address());
    // SAFETY: fresh frames owned by this driver, in the physical window.
    unsafe {
        core::ptr::write_bytes(screen_virt, 0, bytes);
        core::ptr::write_bytes(cursor_virt, 0, CURSOR_SIZE * CURSOR_SIZE * 4);
    }
    kprintln!(
        "ZOO_OS INFO gpu virtio {}x{} monitors={} (shown when the display is taken)",
        w,
        h,
        monitors
    );
    *GPU.lock() = Some(Gpu {
        common,
        notify_base,
        notify_mult,
        queues,
        width: w,
        monitors,
        height: h,
        screen_phys: screen.start_address().as_u64(),
        cursor_phys: cursor.start_address().as_u64(),
        cursor: cursor_virt,
        active: false,
    });
    Some((w, h))
}

fn new_queue(mem: &mut Memory, index: u16) -> Option<Queue> {
    let mut page = || {
        let f = mem.frames.allocate()?;
        let virt = mem.phys_to_virt(f.start_address());
        // SAFETY: a fresh frame owned by this driver.
        unsafe { core::ptr::write_bytes(virt, 0, 4096) };
        Some((f.start_address().as_u64(), virt))
    };
    let (ring_phys, ring) = page()?;
    let (req_phys, req) = page()?;
    let (resp_phys, resp) = page()?;
    Some(Queue {
        index,
        size: QUEUE_ENTRIES,
        ring_phys,
        ring,
        req_phys,
        req,
        resp_phys,
        resp,
        avail_idx: 0,
        notify: 0,
    })
}

/// Resets the device and brings it up with both queues (virtio 1.x
/// §3.1.1). A reset also returns `virtio-vga` to its VGA mode.
fn start_device(
    common: Regs,
    notify_base: u64,
    notify_mult: u32,
    queues: &mut [Queue; 2],
) -> Result<(), &'static str> {
    common.w8(DEVICE_STATUS, 0);
    let mut reset = false;
    for _ in 0..SPINS {
        if common.r8(DEVICE_STATUS) == 0 {
            reset = true;
            break;
        }
        core::hint::spin_loop();
    }
    if !reset {
        return Err("reset-timeout");
    }
    common.w8(DEVICE_STATUS, STATUS_ACK);
    common.w8(DEVICE_STATUS, STATUS_ACK | STATUS_DRIVER);
    // VIRTIO_F_VERSION_1 (bit 32) only.
    common.w32(DEVICE_FEATURE_SELECT, 1);
    if common.r32(DEVICE_FEATURE) & 1 == 0 {
        return Err("no-version-1");
    }
    common.w32(DRIVER_FEATURE_SELECT, 0);
    common.w32(DRIVER_FEATURE, 0);
    common.w32(DRIVER_FEATURE_SELECT, 1);
    common.w32(DRIVER_FEATURE, 1);
    let s = STATUS_ACK | STATUS_DRIVER | STATUS_FEATURES_OK;
    common.w8(DEVICE_STATUS, s);
    if common.r8(DEVICE_STATUS) & STATUS_FEATURES_OK == 0 {
        return Err("features-refused");
    }
    common.w16(MSIX_CONFIG, NO_VECTOR);
    for q in queues.iter_mut() {
        common.w16(QUEUE_SELECT, q.index);
        let max = common.r16(QUEUE_SIZE);
        if max < 2 * SLOTS as u16 {
            return Err("queue-too-small");
        }
        q.size = max.min(QUEUE_ENTRIES);
        common.w16(QUEUE_SIZE, q.size);
        common.w16(QUEUE_MSIX_VECTOR, NO_VECTOR);
        // SAFETY: this queue's own ring page; cleared for the new start.
        unsafe { core::ptr::write_bytes(q.ring, 0, 4096) };
        // No interrupts wanted (VIRTQ_AVAIL_F_NO_INTERRUPT).
        // SAFETY: avail ring flags in the ring page.
        unsafe { (q.ring.add(AVAIL) as *mut u16).write_volatile(1) };
        q.avail_idx = 0;
        common.w64(QUEUE_DESC, q.ring_phys + DESC as u64);
        common.w64(QUEUE_DRIVER, q.ring_phys + AVAIL as u64);
        common.w64(QUEUE_DEVICE, q.ring_phys + USED as u64);
        q.notify = notify_base + u64::from(common.r16(QUEUE_NOTIFY_OFF)) * u64::from(notify_mult);
        common.w16(QUEUE_ENABLE, 1);
    }
    common.w8(DEVICE_STATUS, s | STATUS_DRIVER_OK);
    Ok(())
}

/// The screen's memory for the display owner, once the device shows it:
/// (physical start, bytes, width, height). Creates the screen and cursor
/// resources and sets the scanout.
pub fn activate() -> Option<(u64, usize, u32, u32, u32)> {
    let mut g = GPU.lock();
    let gpu = g.as_mut()?;
    if !gpu.active {
        if let Err(why) = gpu.show() {
            kprintln!("ZOO_OS INFO gpu error={}", why);
            return None;
        }
        gpu.active = true;
        kprintln!(
            "ZOO_OS INFO gpu scanout {}x{} hardware-cursor monitors={}",
            gpu.width,
            gpu.height,
            gpu.monitors
        );
    }
    let bytes = (gpu.width as usize) * (gpu.height as usize) * 4;
    Some((gpu.screen_phys, bytes, gpu.width, gpu.height, gpu.monitors))
}

impl Gpu {
    fn show(&mut self) -> Result<(), &'static str> {
        let (w, h) = (self.width, self.height);
        let bytes = w * h * 4;
        let q = &mut self.queues[0];
        // Create both resources and give them their memory.
        let r = q.slot(0);
        r[0] = CMD_RESOURCE_CREATE_2D;
        r[6..10].copy_from_slice(&[SCREEN_RESOURCE, FORMAT_B8G8R8X8, w, h]);
        let r = q.slot(1);
        r[0] = CMD_RESOURCE_ATTACH_BACKING;
        r[6..8].copy_from_slice(&[SCREEN_RESOURCE, 1]);
        r[8..12].copy_from_slice(&[
            self.screen_phys as u32,
            (self.screen_phys >> 32) as u32,
            bytes,
            0,
        ]);
        let c = CURSOR_SIZE as u32;
        let r = q.slot(2);
        r[0] = CMD_RESOURCE_CREATE_2D;
        r[6..10].copy_from_slice(&[CURSOR_RESOURCE, FORMAT_B8G8R8A8, c, c]);
        let r = q.slot(3);
        r[0] = CMD_RESOURCE_ATTACH_BACKING;
        r[6..8].copy_from_slice(&[CURSOR_RESOURCE, 1]);
        r[8..12].copy_from_slice(&[
            self.cursor_phys as u32,
            (self.cursor_phys >> 32) as u32,
            c * c * 4,
            0,
        ]);
        let t = q.run(&[40, 48, 40, 48], 24)?;
        if t.iter().any(|&t| t != RESP_OK_NODATA) {
            return Err("resource-setup-refused");
        }
        // Scanout k shows monitor k's part of the screen resource.
        let mw = w / self.monitors;
        for k in 0..self.monitors {
            let r = q.slot(0);
            r[0] = CMD_SET_SCANOUT;
            r[6..12].copy_from_slice(&[k * mw, 0, mw, h, k, SCREEN_RESOURCE]);
            let t = q.run(&[48], 24)?;
            if t[0] != RESP_OK_NODATA {
                return Err("set-scanout-refused");
            }
        }
        Ok(())
    }

    /// TRANSFER_TO_HOST_2D then RESOURCE_FLUSH of one rectangle, as one
    /// batch.
    fn flush(
        &mut self,
        resource: u32,
        stride: u32,
        (x, y, w, h): (u32, u32, u32, u32),
    ) -> Result<(), &'static str> {
        let offset = u64::from(y) * u64::from(stride) * 4 + u64::from(x) * 4;
        let q = &mut self.queues[0];
        let r = q.slot(0);
        r[0] = CMD_TRANSFER_TO_HOST_2D;
        r[6..10].copy_from_slice(&[x, y, w, h]);
        r[10] = offset as u32;
        r[11] = (offset >> 32) as u32;
        r[12] = resource;
        let r = q.slot(1);
        r[0] = CMD_RESOURCE_FLUSH;
        r[6..11].copy_from_slice(&[x, y, w, h, resource]);
        let t = q.run(&[56, 48], 24)?;
        if t[0] != RESP_OK_NODATA || t[1] != RESP_OK_NODATA {
            return Err("flush-refused");
        }
        Ok(())
    }

    /// UPDATE_CURSOR (a new image) or MOVE_CURSOR on the cursor queue.
    fn cursor_cmd(
        &mut self,
        cmd: u32,
        x: u32,
        y: u32,
        hot: (u32, u32),
    ) -> Result<(), &'static str> {
        // The monitor under (x, y), and the position on it.
        let mw = (self.width / self.monitors).max(1);
        let scanout = (x / mw).min(self.monitors - 1);
        let x = x - scanout * mw;
        let q = &mut self.queues[1];
        let r = q.slot(0);
        r[0] = cmd;
        // pos: scanout, x, y, padding; then resource, hot_x, hot_y.
        r[6..13].copy_from_slice(&[scanout, x, y, 0, CURSOR_RESOURCE, hot.0, hot.1]);
        q.run(&[56], 24).map(|_| ())
    }
}

/// Shows rectangle (x, y, w, h) of the screen memory (display owner).
pub fn flush(x: u32, y: u32, w: u32, h: u32) -> Result<(), &'static str> {
    let mut g = GPU.lock();
    let gpu = g.as_mut().filter(|g| g.active).ok_or("gpu-inactive")?;
    let (sw, sh) = (gpu.width, gpu.height);
    if x >= sw || y >= sh || w == 0 || h == 0 {
        return Ok(());
    }
    let (w, h) = (w.min(sw - x), h.min(sh - y));
    gpu.flush(SCREEN_RESOURCE, sw, (x, y, w, h))
}

/// Sets the cursor image (`CURSOR_SIZE`² pixels, 0xAARRGGBB) with its hot
/// spot, at (x, y).
pub fn cursor_set(image: &[u32], hot: (u32, u32), x: u32, y: u32) -> Result<(), &'static str> {
    let mut g = GPU.lock();
    let gpu = g.as_mut().filter(|g| g.active).ok_or("gpu-inactive")?;
    let n = CURSOR_SIZE * CURSOR_SIZE;
    for (k, &p) in image.iter().take(n).enumerate() {
        // SAFETY: the cursor frames hold `n` pixels of 4 bytes.
        unsafe { (gpu.cursor as *mut u32).add(k).write_volatile(p) };
    }
    let c = CURSOR_SIZE as u32;
    let q = &mut gpu.queues[0];
    let r = q.slot(0);
    r[0] = CMD_TRANSFER_TO_HOST_2D;
    r[6..10].copy_from_slice(&[0, 0, c, c]);
    r[12] = CURSOR_RESOURCE;
    if q.run(&[56], 24)?[0] != RESP_OK_NODATA {
        return Err("cursor-transfer-refused");
    }
    gpu.cursor_cmd(CMD_UPDATE_CURSOR, x, y, hot)
}

/// Moves the cursor to (x, y).
pub fn cursor_move(x: u32, y: u32) -> Result<(), &'static str> {
    let mut g = GPU.lock();
    let gpu = g.as_mut().filter(|g| g.active).ok_or("gpu-inactive")?;
    gpu.cursor_cmd(CMD_MOVE_CURSOR, x, y, (0, 0))
}

/// The display owner let go: reset the device (back to VGA mode, where
/// the console draws) and bring it up again for the next owner.
pub fn deactivate() {
    let mut g = GPU.lock();
    let Some(gpu) = g.as_mut() else {
        return;
    };
    if !gpu.active {
        return;
    }
    gpu.active = false;
    match start_device(
        gpu.common,
        gpu.notify_base,
        gpu.notify_mult,
        &mut gpu.queues,
    ) {
        Ok(()) => kprintln!("ZOO_OS INFO gpu released (vga)"),
        Err(why) => kprintln!("ZOO_OS INFO gpu error={}", why),
    }
}
