//! A minimal virtio network driver (docs/n1-design.md): QEMU's
//! `virtio-net-pci` through the legacy I/O-port interface, polled (no
//! interrupts). Queue 0 receives into `RX_BUFFERS` posted buffers; queue 1
//! sends one frame at a time. Frames carry the 10-byte legacy header
//! (no offloads were negotiated, so it is all zeroes when sending).

use crate::memory::Memory;
use spin::Mutex;
use x86_64::instructions::port::Port;
use x86_64::structures::paging::PhysFrame;

const PCI_ADDR: u16 = 0xCF8;
const PCI_DATA: u16 = 0xCFC;
const VIRTIO_VENDOR: u16 = 0x1AF4;
/// Transitional (legacy-capable) virtio network device.
const VIRTIO_NET_LEGACY: u16 = 0x1000;

// Legacy virtio PCI registers, relative to BAR0 (I/O space).
const REG_HOST_FEATURES: u16 = 0x00;
const REG_GUEST_FEATURES: u16 = 0x04;
const REG_QUEUE_PFN: u16 = 0x08;
const REG_QUEUE_SIZE: u16 = 0x0C;
const REG_QUEUE_SELECT: u16 = 0x0E;
const REG_QUEUE_NOTIFY: u16 = 0x10;
const REG_STATUS: u16 = 0x12;
/// Device-specific configuration (MSI-X off): the MAC address first.
const REG_CONFIG: u16 = 0x14;

const STATUS_ACK: u8 = 1;
const STATUS_DRIVER: u8 = 2;
const STATUS_DRIVER_OK: u8 = 4;

/// Feature bit: the device has a MAC address in its configuration.
const F_MAC: u32 = 1 << 5;

const DESC_WRITE: u16 = 2;
const PAGE: usize = 4096;
/// The legacy header before every frame (no `MRG_RXBUF`).
const HEADER: usize = 10;
/// Receive buffers, each half a page.
const RX_BUFFERS: usize = 16;
const RX_BUF: usize = 2048;
/// Largest frame (Ethernet without the CRC).
pub const MAX_FRAME: usize = 1514;
/// Polling bound for one send (iterations); hitting it is an error.
const POLL_LIMIT: u64 = 50_000_000;

struct Queue {
    size: usize,
    pages: PhysFrame,
    avail_idx: u16,
    used_seen: u16,
}

struct Device {
    io: u16,
    mac: [u8; 6],
    rx: Queue,
    tx: Queue,
    /// `RX_BUFFERS` / 2 pages of receive buffers.
    rx_bufs: PhysFrame,
    /// One page: the frame being sent.
    tx_buf: PhysFrame,
}

static DEVICE: Mutex<Option<Device>> = Mutex::new(None);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetError {
    NoDevice,
    TooLong,
    Timeout,
}

fn pci_read(dev: u8, func: u8, off: u8) -> u32 {
    let addr =
        0x8000_0000u32 | (u32::from(dev) << 11) | (u32::from(func) << 8) | u32::from(off & 0xFC);
    // SAFETY: 0xCF8/0xCFC are the PCI configuration mechanism #1 ports;
    // selecting an address and reading it has no other effect.
    unsafe {
        Port::<u32>::new(PCI_ADDR).write(addr);
        Port::<u32>::new(PCI_DATA).read()
    }
}

fn pci_write(dev: u8, func: u8, off: u8, value: u32) {
    let addr =
        0x8000_0000u32 | (u32::from(dev) << 11) | (u32::from(func) << 8) | u32::from(off & 0xFC);
    // SAFETY: as in `pci_read`; only the command register of the virtio
    // device we found is written.
    unsafe {
        Port::<u32>::new(PCI_ADDR).write(addr);
        Port::<u32>::new(PCI_DATA).write(value);
    }
}

/// Legacy virtqueue layout for `n` entries: (offset of the used ring,
/// total bytes), the used ring starting at a page boundary.
fn queue_layout(n: usize) -> (usize, usize) {
    let used_at = (16 * n + 6 + 2 * n).div_ceil(PAGE) * PAGE;
    (used_at, used_at + 6 + 8 * n)
}

/// Sets up queue `index`; returns it with zeroed pages.
fn setup_queue(mem: &mut Memory, io: u16, index: u16) -> Option<Queue> {
    // SAFETY: `io` is this device's legacy register block, owned by this
    // driver alone.
    let size = unsafe {
        Port::<u16>::new(io + REG_QUEUE_SELECT).write(index);
        Port::<u16>::new(io + REG_QUEUE_SIZE).read()
    } as usize;
    if size == 0 {
        return None;
    }
    let pages_n = queue_layout(size).1.div_ceil(PAGE);
    let pages = mem.frames.allocate_contiguous(pages_n)?;
    // SAFETY: fresh frames owned by this driver, mapped in the physical
    // window; zeroed before the device sees them.
    unsafe { core::ptr::write_bytes(mem.phys_to_virt(pages.start_address()), 0, pages_n * PAGE) };
    let pfn = (pages.start_address().as_u64() / PAGE as u64) as u32;
    // SAFETY: as above; queue `index` is selected.
    unsafe { Port::<u32>::new(io + REG_QUEUE_PFN).write(pfn) };
    Some(Queue {
        size,
        pages,
        avail_idx: 0,
        used_seen: 0,
    })
}

/// Writes descriptor `i` of the queue at `q`.
///
/// # Safety
/// `q` is the virtual address of a queue's pages and `i` below its size.
unsafe fn desc(q: *mut u8, i: usize, addr: u64, len: u32, flags: u16) {
    // SAFETY: the caller's contract; descriptors are 16 bytes from `q`.
    unsafe {
        let p = q.add(16 * i);
        (p as *mut u64).write_volatile(addr);
        (p.add(8) as *mut u32).write_volatile(len);
        (p.add(12) as *mut u16).write_volatile(flags);
        (p.add(14) as *mut u16).write_volatile(0);
    }
}

/// Offers descriptor `id` on the queue at `q` (of `n` entries).
///
/// # Safety
/// As for `desc`.
unsafe fn offer(q: *mut u8, n: usize, queue: &mut Queue, id: u16) {
    // SAFETY: the available ring follows the descriptors in the queue's
    // pages (the caller's contract).
    unsafe {
        let avail = q.add(16 * n);
        let ring = avail.add(4) as *mut u16;
        ring.add(queue.avail_idx as usize % n).write_volatile(id);
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        queue.avail_idx = queue.avail_idx.wrapping_add(1);
        (avail.add(2) as *mut u16).write_volatile(queue.avail_idx);
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
    }
}

/// Finds and initialises the device; returns its MAC address, or `None`
/// if there is no virtio network device.
pub fn init(mem: &mut Memory) -> Option<[u8; 6]> {
    let mut found = None;
    'scan: for dev in 0..32u8 {
        for func in 0..8u8 {
            let id = pci_read(dev, func, 0);
            if id & 0xFFFF == u32::from(VIRTIO_VENDOR) && (id >> 16) as u16 == VIRTIO_NET_LEGACY {
                found = Some((dev, func));
                break 'scan;
            }
        }
    }
    let (dev, func) = found?;
    let bar0 = pci_read(dev, func, 0x10);
    if bar0 & 1 == 0 {
        return None; // not an I/O BAR: no legacy interface
    }
    let io = (bar0 & 0xFFFC) as u16;
    // Enable I/O space and bus mastering (DMA).
    let cmd = pci_read(dev, func, 0x04);
    pci_write(dev, func, 0x04, cmd | 0x5);
    // SAFETY (port accesses in this function): `io` is this device's
    // legacy register block, owned by this driver alone.
    let status = |v: u8| unsafe { Port::<u8>::new(io + REG_STATUS).write(v) };
    status(0); // reset
    status(STATUS_ACK);
    status(STATUS_ACK | STATUS_DRIVER);
    let host = unsafe { Port::<u32>::new(io + REG_HOST_FEATURES).read() };
    unsafe { Port::<u32>::new(io + REG_GUEST_FEATURES).write(host & F_MAC) };
    let mut rx = setup_queue(mem, io, 0)?;
    let tx = setup_queue(mem, io, 1)?;
    let rx_bufs = mem.frames.allocate_contiguous(RX_BUFFERS * RX_BUF / PAGE)?;
    let tx_buf = mem.frames.allocate()?;
    // Every receive buffer offered to the device, device-writable.
    let q = mem.phys_to_virt(rx.pages.start_address());
    let n = rx.size;
    for k in 0..RX_BUFFERS.min(n) {
        let addr = rx_bufs.start_address().as_u64() + (k * RX_BUF) as u64;
        // SAFETY: `q` is the receive queue's pages and `k < n`.
        unsafe {
            desc(q, k, addr, RX_BUF as u32, DESC_WRITE);
            offer(q, n, &mut rx, k as u16);
        }
    }
    let mut mac = [0u8; 6];
    for (k, b) in mac.iter_mut().enumerate() {
        *b = unsafe { Port::<u8>::new(io + REG_CONFIG + k as u16).read() };
    }
    status(STATUS_ACK | STATUS_DRIVER | STATUS_DRIVER_OK);
    unsafe { Port::<u16>::new(io + REG_QUEUE_NOTIFY).write(0) };
    *DEVICE.lock() = Some(Device {
        io,
        mac,
        rx,
        tx,
        rx_bufs,
        tx_buf,
    });
    Some(mac)
}

/// The device's MAC address, if there is a device.
pub fn mac() -> Option<[u8; 6]> {
    DEVICE.lock().as_ref().map(|d| d.mac)
}

/// Sends one Ethernet frame (without the CRC) and waits until the device
/// has taken it. Called with interrupts disabled.
pub fn send(mem: &Memory, frame: &[u8]) -> Result<(), NetError> {
    if frame.len() > MAX_FRAME {
        return Err(NetError::TooLong);
    }
    let mut g = DEVICE.lock();
    let d = g.as_mut().ok_or(NetError::NoDevice)?;
    let buf = mem.phys_to_virt(d.tx_buf.start_address());
    // SAFETY: the transmit page is this driver's own frame; header and
    // frame fit in it (`HEADER + MAX_FRAME < PAGE`).
    unsafe {
        core::ptr::write_bytes(buf, 0, HEADER);
        core::ptr::copy_nonoverlapping(frame.as_ptr(), buf.add(HEADER), frame.len());
    }
    let q = mem.phys_to_virt(d.tx.pages.start_address());
    let n = d.tx.size;
    let addr = d.tx_buf.start_address().as_u64();
    // SAFETY: `q` is the transmit queue's pages; descriptor 0 exists.
    unsafe {
        desc(q, 0, addr, (HEADER + frame.len()) as u32, 0);
        offer(q, n, &mut d.tx, 0);
        Port::<u16>::new(d.io + REG_QUEUE_NOTIFY).write(1);
    }
    let (used_at, _) = queue_layout(n);
    // SAFETY: the used ring's index lies inside the queue's pages; the
    // device updates it, hence volatile reads.
    let used_idx = unsafe { q.add(used_at + 2) as *const u16 };
    let mut spins = 0u64;
    while unsafe { used_idx.read_volatile() } == d.tx.used_seen {
        spins += 1;
        if spins > POLL_LIMIT {
            return Err(NetError::Timeout);
        }
        core::hint::spin_loop();
    }
    d.tx.used_seen = d.tx.used_seen.wrapping_add(1);
    Ok(())
}

/// Takes one received frame into `out`, returning its length (cut to
/// `out.len()`), or `None` if none is waiting. The buffer goes back to the
/// device. Called with interrupts disabled.
pub fn recv(mem: &Memory, out: &mut [u8]) -> Result<Option<usize>, NetError> {
    let mut g = DEVICE.lock();
    let d = g.as_mut().ok_or(NetError::NoDevice)?;
    let q = mem.phys_to_virt(d.rx.pages.start_address());
    let n = d.rx.size;
    let (used_at, _) = queue_layout(n);
    // SAFETY: the used ring lies inside the receive queue's pages; the
    // device writes it, hence volatile reads.
    let (idx, id, len) = unsafe {
        let used = q.add(used_at);
        let idx = (used.add(2) as *const u16).read_volatile();
        if idx == d.rx.used_seen {
            return Ok(None);
        }
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        let elem = used.add(4 + 8 * (d.rx.used_seen as usize % n));
        let id = (elem as *const u32).read_volatile() as usize;
        let len = (elem.add(4) as *const u32).read_volatile() as usize;
        (idx, id, len)
    };
    let _ = idx;
    d.rx.used_seen = d.rx.used_seen.wrapping_add(1);
    if id >= RX_BUFFERS.min(n) {
        return Ok(None); // not one of ours: ignore
    }
    let frame_len = len.saturating_sub(HEADER).min(RX_BUF - HEADER);
    let copy = frame_len.min(out.len());
    let buf = mem.phys_to_virt(d.rx_bufs.start_address());
    // SAFETY: buffer `id` lies inside the receive pages; `copy` bytes after
    // its header are inside it.
    unsafe {
        core::ptr::copy_nonoverlapping(buf.add(id * RX_BUF + HEADER), out.as_mut_ptr(), copy)
    };
    // The buffer goes back to the device.
    // SAFETY: `q` is the receive queue's pages and `id < n`.
    unsafe {
        offer(q, n, &mut d.rx, id as u16);
        Port::<u16>::new(d.io + REG_QUEUE_NOTIFY).write(0);
    }
    Ok(Some(copy))
}
