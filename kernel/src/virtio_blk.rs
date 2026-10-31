//! A minimal virtio block driver (docs/s5-design.md §1): QEMU's
//! `virtio-blk-pci` through the legacy I/O-port interface, one queue,
//! polled (no interrupts), whole 4 KiB blocks. Enough for the filesystem
//! service's snapshots; interrupts and the modern interface come later.

use crate::memory::Memory;
use spin::Mutex;
use x86_64::instructions::port::Port;
use x86_64::structures::paging::PhysFrame;

const PCI_ADDR: u16 = 0xCF8;
const PCI_DATA: u16 = 0xCFC;
const VIRTIO_VENDOR: u16 = 0x1AF4;
/// Transitional (legacy-capable) virtio block device.
const VIRTIO_BLK_LEGACY: u16 = 0x1001;

// Legacy virtio PCI registers, relative to BAR0 (I/O space).
const REG_GUEST_FEATURES: u16 = 0x04;
const REG_QUEUE_PFN: u16 = 0x08;
const REG_QUEUE_SIZE: u16 = 0x0C;
const REG_QUEUE_SELECT: u16 = 0x0E;
const REG_QUEUE_NOTIFY: u16 = 0x10;
const REG_STATUS: u16 = 0x12;
const REG_CAPACITY: u16 = 0x14;

const STATUS_ACK: u8 = 1;
const STATUS_DRIVER: u8 = 2;
const STATUS_DRIVER_OK: u8 = 4;

const DESC_NEXT: u16 = 1;
const DESC_WRITE: u16 = 2;

const REQ_IN: u32 = 0; // read
const REQ_OUT: u32 = 1; // write

/// Bytes per block the service reads and writes.
pub const BLOCK: usize = 4096;
/// Polling bound for one request (iterations); hitting it is an error.
const POLL_LIMIT: u64 = 200_000_000;

struct Device {
    io: u16,
    queue_size: u16,
    /// The queue's pages: descriptors, available ring, (aligned) used ring.
    queue: PhysFrame,
    /// One page: request header at 0, status byte at 64.
    req: PhysFrame,
    /// Bounce buffer for one block.
    data: PhysFrame,
    /// Next available-ring index we will use.
    avail_idx: u16,
    /// Used-ring index we have seen.
    used_seen: u16,
    capacity_sectors: u64,
}

static DEVICE: Mutex<Option<Device>> = Mutex::new(None);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockError {
    NoDevice,
    OutOfRange,
    Io,
    Timeout,
}

fn pci_read(bus: u8, dev: u8, func: u8, off: u8) -> u32 {
    let addr = 0x8000_0000u32
        | (u32::from(bus) << 16)
        | (u32::from(dev) << 11)
        | (u32::from(func) << 8)
        | u32::from(off & 0xFC);
    // SAFETY: 0xCF8/0xCFC are the PCI configuration mechanism #1 ports;
    // selecting an address and reading it has no other effect.
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
    // SAFETY: as in `pci_read`; only the command register of the virtio
    // device we found is written.
    unsafe {
        Port::<u32>::new(PCI_ADDR).write(addr);
        Port::<u32>::new(PCI_DATA).write(value);
    }
}

/// Legacy virtqueue layout for `n` entries: (bytes before the used ring,
/// total bytes), the used ring starting at a 4 KiB boundary.
fn queue_layout(n: usize) -> (usize, usize) {
    let desc = 16 * n;
    let avail = 6 + 2 * n;
    let used_at = (desc + avail).div_ceil(BLOCK) * BLOCK;
    let used = 6 + 8 * n;
    (used_at, used_at + used)
}

/// Finds and initialises the device. Returns its capacity in bytes, or
/// `None` if there is no virtio block device.
pub fn init(mem: &mut Memory) -> Option<u64> {
    let mut found = None;
    'scan: for dev in 0..32u8 {
        for func in 0..8u8 {
            let id = pci_read(0, dev, func, 0);
            if id & 0xFFFF == u32::from(VIRTIO_VENDOR) && (id >> 16) as u16 == VIRTIO_BLK_LEGACY {
                found = Some((dev, func));
                break 'scan;
            }
        }
    }
    let (dev, func) = found?;
    let bar0 = pci_read(0, dev, func, 0x10);
    if bar0 & 1 == 0 {
        return None; // not an I/O BAR: no legacy interface
    }
    let io = (bar0 & 0xFFFC) as u16;
    // Enable I/O space and bus mastering (DMA).
    let cmd = pci_read(0, dev, func, 0x04);
    pci_write(0, dev, func, 0x04, cmd | 0x5);

    // SAFETY (all port accesses below): `io` is this device's legacy
    // virtio register block, owned by this driver alone.
    let status = |v: u8| unsafe { Port::<u8>::new(io + REG_STATUS).write(v) };
    status(0); // reset
    status(STATUS_ACK);
    status(STATUS_ACK | STATUS_DRIVER);
    unsafe { Port::<u32>::new(io + REG_GUEST_FEATURES).write(0) };
    unsafe { Port::<u16>::new(io + REG_QUEUE_SELECT).write(0) };
    let n = unsafe { Port::<u16>::new(io + REG_QUEUE_SIZE).read() };
    if n == 0 {
        return None;
    }
    let (_, total) = queue_layout(n as usize);
    let pages = total.div_ceil(BLOCK);
    let queue = mem.frames.allocate_contiguous(pages)?;
    let req = mem.frames.allocate()?;
    let data = mem.frames.allocate()?;
    for (f, count) in [(queue, pages), (req, 1), (data, 1)] {
        // SAFETY: fresh frames owned by this driver, mapped in the physical
        // window; zeroed before the device sees them.
        unsafe { core::ptr::write_bytes(mem.phys_to_virt(f.start_address()), 0, count * BLOCK) };
    }
    let pfn = (queue.start_address().as_u64() / BLOCK as u64) as u32;
    unsafe { Port::<u32>::new(io + REG_QUEUE_PFN).write(pfn) };
    status(STATUS_ACK | STATUS_DRIVER | STATUS_DRIVER_OK);
    let lo = unsafe { Port::<u32>::new(io + REG_CAPACITY).read() } as u64;
    let hi = unsafe { Port::<u32>::new(io + REG_CAPACITY + 4).read() } as u64;
    let capacity_sectors = lo | (hi << 32);
    *DEVICE.lock() = Some(Device {
        io,
        queue_size: n,
        queue,
        req,
        data,
        avail_idx: 0,
        used_seen: 0,
        capacity_sectors,
    });
    Some(capacity_sectors * 512)
}

/// Capacity in bytes, if a device was found.
pub fn capacity() -> Option<u64> {
    DEVICE.lock().as_ref().map(|d| d.capacity_sectors * 512)
}

/// Reads (`write` false) or writes the 4 KiB block `block` through the
/// bounce page, copying from/to `buf`. Called with interrupts disabled.
pub fn transfer(
    mem: &Memory,
    block: u64,
    buf: &mut [u8; BLOCK],
    write: bool,
) -> Result<(), BlockError> {
    let mut g = DEVICE.lock();
    let d = g.as_mut().ok_or(BlockError::NoDevice)?;
    let sectors = (BLOCK / 512) as u64;
    if (block + 1) * sectors > d.capacity_sectors {
        return Err(BlockError::OutOfRange);
    }
    let n = d.queue_size as usize;
    let virt = |f: PhysFrame| mem.phys_to_virt(f.start_address());
    let data_virt = virt(d.data);
    if write {
        // SAFETY: the bounce page is this driver's own frame.
        unsafe { core::ptr::copy_nonoverlapping(buf.as_ptr(), data_virt, BLOCK) };
    }
    let req_phys = d.req.start_address().as_u64();
    let req_virt = virt(d.req);
    // SAFETY: the request page is this driver's own frame; the device only
    // reads the header and writes the status byte, both inside it.
    unsafe {
        (req_virt as *mut u32).write_volatile(if write { REQ_OUT } else { REQ_IN });
        (req_virt.add(4) as *mut u32).write_volatile(0);
        (req_virt.add(8) as *mut u64).write_volatile(block * sectors);
        req_virt.add(64).write_volatile(0xFF);
    }
    let q = virt(d.queue);
    let (used_at, _) = queue_layout(n);
    // Three descriptors, always 0 -> 1 -> 2 (one request at a time).
    let desc = |i: usize, addr: u64, len: u32, flags: u16, next: u16| {
        // SAFETY: descriptor `i < n` lies inside the queue pages.
        unsafe {
            let p = q.add(16 * i);
            (p as *mut u64).write_volatile(addr);
            (p.add(8) as *mut u32).write_volatile(len);
            (p.add(12) as *mut u16).write_volatile(flags);
            (p.add(14) as *mut u16).write_volatile(next);
        }
    };
    desc(0, req_phys, 16, DESC_NEXT, 1);
    desc(
        1,
        d.data.start_address().as_u64(),
        BLOCK as u32,
        DESC_NEXT | if write { 0 } else { DESC_WRITE },
        2,
    );
    desc(2, req_phys + 64, 1, DESC_WRITE, 0);
    // SAFETY: the available ring follows the descriptors in the queue pages.
    unsafe {
        let avail = q.add(16 * n);
        let ring = avail.add(4) as *mut u16;
        ring.add(d.avail_idx as usize % n).write_volatile(0);
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        d.avail_idx = d.avail_idx.wrapping_add(1);
        (avail.add(2) as *mut u16).write_volatile(d.avail_idx);
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        Port::<u16>::new(d.io + REG_QUEUE_NOTIFY).write(0);
    }
    // Poll the used ring (bounded).
    let used_idx = unsafe { q.add(used_at + 2) as *const u16 };
    let mut spins = 0u64;
    // SAFETY: the used ring's index field lies inside the queue pages; the
    // device updates it, hence volatile reads.
    while unsafe { used_idx.read_volatile() } == d.used_seen {
        spins += 1;
        if spins > POLL_LIMIT {
            return Err(BlockError::Timeout);
        }
        core::hint::spin_loop();
    }
    d.used_seen = d.used_seen.wrapping_add(1);
    core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
    // SAFETY: the status byte inside our request page.
    let status = unsafe { req_virt.add(64).read_volatile() };
    if status != 0 {
        return Err(BlockError::Io);
    }
    if !write {
        // SAFETY: the bounce page holds the block the device just wrote.
        unsafe { core::ptr::copy_nonoverlapping(data_virt, buf.as_mut_ptr(), BLOCK) };
    }
    Ok(())
}
