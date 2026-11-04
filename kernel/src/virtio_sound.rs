//! virtio-sound playback and recording over the modern virtio PCI
//! transport (docs/audio-design.md, docs/microphone-design.md).
//!
//! Output stream 0, 16-bit stereo at 48 kHz. The device takes the samples
//! at playing speed, so a write never waits for it: `write` copies what
//! fits into this driver's free period buffers, hands them to the device
//! and returns how many bytes it took; buffers the device has played are
//! taken back on the next call. The control queue's few requests (set up,
//! prepare, start) are synchronous and bounded.
//!
//! Recording: input stream 1, 16-bit mono at 16 kHz (what speech to text
//! reads). Every receive buffer stays queued; one the device has filled is
//! copied into an eight-second ring and handed straight back, so `read` never
//! waits either. When the ring (8 s) is full the oldest samples are dropped.

use crate::kprintln;
use crate::memory::Memory;
use spin::Mutex;
use x86_64::instructions::port::Port;

const PCI_ADDR: u16 = 0xCF8;
const PCI_DATA: u16 = 0xCFC;
const VIRTIO_VENDOR: u32 = 0x1AF4;
/// virtio device id 25 (sound) on the modern transport: 0x1040 + 25.
const VIRTIO_SOUND: u32 = 0x1059;

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
const NO_VECTOR: u16 = 0xFFFF;

// Requests and status (virtio 1.2 §5.14.6).
const R_PCM_INFO: u32 = 0x0100;
const R_PCM_SET_PARAMS: u32 = 0x0101;
const R_PCM_PREPARE: u32 = 0x0102;
const R_PCM_START: u32 = 0x0104;
const R_PCM_STOP: u32 = 0x0105;
const R_PCM_RELEASE: u32 = 0x0103;
const S_OK: u32 = 0x8000;
/// `VIRTIO_SND_PCM_FMT_S16` and `VIRTIO_SND_PCM_RATE_48000`.
const FMT_S16: u8 = 5;
const RATE_48000: u8 = 7;
/// `VIRTIO_SND_PCM_RATE_16000`.
const RATE_16000: u8 = 3;
const DIRECTION_OUTPUT: u8 = 0;
const DIRECTION_INPUT: u8 = 1;
/// The input stream QEMU's device has after its output stream.
const INPUT_STREAM: u32 = 1;
/// One receive period: 40 ms of 16-bit mono at 16 kHz.
const RPERIOD: usize = 1280;
/// Receive periods queued: 400 ms.
const RBUFS: usize = 10;
/// The recording ring: eight seconds (a program busy hearing, or a
/// machine busy starting the desktop, reads late).
const RING: usize = 256_000;
/// Receive headers start here in the header page (transmit ones below).
const RHDR: usize = 1024;

/// One period: 25 ms of 16-bit stereo at 48 kHz.
const PERIOD: usize = 4800;
/// Periods in flight at most: 200 ms ahead of the speaker.
const BUFS: usize = 8;

/// Queues: control 0, event 1, tx 2, rx 3.
const CONTROLQ: u16 = 0;
const TXQ: u16 = 2;
const RXQ: u16 = 3;
const QUEUE_ENTRIES: u16 = 32;
const SPINS: u64 = 200_000_000;

// Ring page layout (32 entries fit with room to spare).
const DESC: usize = 0;
const AVAIL: usize = 1024;
const USED: usize = 2048;

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

/// A capability's structure, mapped.
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
        return None;
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

/// One split virtqueue in a page of its own.
struct Queue {
    index: u16,
    size: u16,
    ring_phys: u64,
    ring: *mut u8,
    avail_idx: u16,
    last_used: u16,
    notify: u64,
}

impl Queue {
    /// Descriptor `d`: buffer, length, device-writable or not, next.
    fn desc(&mut self, d: usize, addr: u64, len: usize, write: bool, next: Option<usize>) {
        let flags = u64::from(next.is_some()) | if write { 2 } else { 0 };
        // SAFETY: descriptor `d` < `size`, inside this queue's ring page.
        unsafe {
            let p = self.ring.add(DESC) as *mut u64;
            p.add(2 * d).write_volatile(addr);
            p.add(2 * d + 1)
                .write_volatile(len as u64 | flags << 32 | (next.unwrap_or(0) as u64) << 48);
        }
    }

    /// Offers the chain starting at descriptor `head` and tells the device.
    fn push(&mut self, head: usize) {
        // SAFETY: avail ring entries < `size`, in this queue's ring page.
        unsafe {
            let avail = self.ring.add(AVAIL) as *mut u16;
            avail
                .add(2 + usize::from(self.avail_idx % self.size))
                .write_volatile(head as u16);
        }
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        self.avail_idx = self.avail_idx.wrapping_add(1);
        // SAFETY: avail ring header: flags (u16), idx (u16).
        unsafe { (self.ring.add(AVAIL + 2) as *mut u16).write_volatile(self.avail_idx) };
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        // SAFETY: this queue's notify register in the mapped notify block.
        unsafe { core::ptr::write_volatile(self.notify as *mut u16, self.index) };
    }

    /// The used ring's index now.
    fn used_idx(&self) -> u16 {
        // SAFETY: used ring header: flags (u16), idx (u16).
        unsafe { (self.ring.add(USED + 2) as *const u16).read_volatile() }
    }

    /// The head descriptor of the next used entry, if the device has
    /// returned one since the last.
    fn take_used(&mut self) -> Option<u32> {
        self.take_used_len().map(|(id, _)| id)
    }

    /// As `take_used`, with the bytes the device wrote into the chain.
    fn take_used_len(&mut self) -> Option<(u32, u32)> {
        if self.last_used == self.used_idx() {
            return None;
        }
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        let slot = usize::from(self.last_used % self.size);
        // SAFETY: used ring entry `slot` (id u32, len u32) after the
        // 4-byte header, inside this queue's ring page.
        let (id, len) = unsafe {
            let e = self.ring.add(USED + 4 + 8 * slot) as *const u32;
            (e.read_volatile(), e.add(1).read_volatile())
        };
        self.last_used = self.last_used.wrapping_add(1);
        Some((id, len))
    }
}

struct Sound {
    ctl: Queue,
    tx: Queue,
    /// Control request (at 0) and response (at 2048).
    ctl_phys: u64,
    ctl_page: *mut u8,
    /// Per period buffer `i`: its transfer header at `16 i`, its status
    /// at `16 i + 8`.
    hdr_phys: u64,
    hdr: *mut u8,
    /// `BUFS` periods of samples, contiguous.
    data_phys: u64,
    data: *mut u8,
    busy: [bool; BUFS],
    started: bool,
    /// The input stream, when the device has one at 16 kHz mono.
    input: Option<Input>,
}

/// Recording state (docs/microphone-design.md).
struct Input {
    rx: Queue,
    /// `RBUFS` receive periods, contiguous.
    data_phys: u64,
    data: *mut u8,
    /// Buffers the device holds.
    queued: [bool; RBUFS],
    recording: bool,
    ring: alloc::collections::VecDeque<u8>,
    /// Bytes dropped because nobody read them in time.
    dropped: u64,
}

// SAFETY: the raw pointers point at this driver's own frames and mapped
// registers; the driver is only used through `SOUND`'s lock.
unsafe impl Send for Sound {}

static SOUND: Mutex<Option<Sound>> = Mutex::new(None);

/// Finds a virtio-sound device and brings it up; the stream starts on the
/// first write. False when there is none.
pub fn init(mem: &mut Memory) -> bool {
    let mut found = None;
    'scan: for dev in 0..32u8 {
        for func in 0..8u8 {
            let id = pci_read(dev, func, 0);
            if id & 0xFFFF == VIRTIO_VENDOR && id >> 16 == VIRTIO_SOUND {
                found = Some((dev, func));
                break 'scan;
            }
        }
    }
    let Some((dev, func)) = found else {
        return false;
    };
    match bring_up(mem, dev, func) {
        Ok(s) => {
            let has_input = s.input.is_some();
            *SOUND.lock() = Some(s);
            kprintln!("ZOO_OS INFO sound virtio output 48000 Hz stereo s16");
            if has_input {
                kprintln!("ZOO_OS INFO sound virtio input 16000 Hz mono s16");
            }
            true
        }
        Err(why) => {
            kprintln!("ZOO_OS INFO sound error={}", why);
            false
        }
    }
}

fn page(mem: &mut Memory, pages: usize) -> Result<(u64, *mut u8), &'static str> {
    let f = mem.frames.allocate_contiguous(pages).ok_or("no-memory")?;
    let virt = mem.phys_to_virt(f.start_address());
    // SAFETY: fresh frames owned by this driver.
    unsafe { core::ptr::write_bytes(virt, 0, pages * 4096) };
    Ok((f.start_address().as_u64(), virt))
}

fn bring_up(mem: &mut Memory, dev: u8, func: u8) -> Result<Sound, &'static str> {
    let cmd = pci_read(dev, func, 0x04);
    pci_write(dev, func, 0x04, (cmd & 0xFFFF) | 0x6);
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
    let common = map_cap(mem, dev, func, common.ok_or("no-common-cfg")?).ok_or("map")?;
    let notify_cap = notify.ok_or("no-notify-cfg")?;
    let notify_mult = pci_read(dev, func, notify_cap + 16);
    let notify_base = map_cap(mem, dev, func, notify_cap).ok_or("map")?.0;

    // Reset, then acknowledge with VIRTIO_F_VERSION_1 only.
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
    // The control, transmit and receive queues; the event queue is left
    // disabled.
    let mut queues = [CONTROLQ, TXQ, RXQ].map(|index| -> Result<Queue, &'static str> {
        let (ring_phys, ring) = page(mem, 1)?;
        Ok(Queue {
            index,
            size: QUEUE_ENTRIES,
            ring_phys,
            ring,
            avail_idx: 0,
            last_used: 0,
            notify: 0,
        })
    });
    for q in queues.iter_mut() {
        let q = q.as_mut().map_err(|e| *e)?;
        common.w16(QUEUE_SELECT, q.index);
        let max = common.r16(QUEUE_SIZE);
        if max < (3 * BUFS.max(RBUFS)) as u16 {
            return Err("queue-too-small");
        }
        q.size = max.min(QUEUE_ENTRIES);
        common.w16(QUEUE_SIZE, q.size);
        common.w16(QUEUE_MSIX_VECTOR, NO_VECTOR);
        common.w64(QUEUE_DESC, q.ring_phys + DESC as u64);
        common.w64(QUEUE_DRIVER, q.ring_phys + AVAIL as u64);
        common.w64(QUEUE_DEVICE, q.ring_phys + USED as u64);
        q.notify = notify_base + u64::from(common.r16(QUEUE_NOTIFY_OFF)) * u64::from(notify_mult);
        common.w16(QUEUE_ENABLE, 1);
    }
    common.w8(DEVICE_STATUS, s | STATUS_DRIVER_OK);
    let [ctl, tx, rx] = queues;
    let (ctl_phys, ctl_page) = page(mem, 1)?;
    let (hdr_phys, hdr) = page(mem, 1)?;
    let (data_phys, data) = page(mem, (BUFS * PERIOD).div_ceil(4096))?;
    let mut snd = Sound {
        ctl: ctl?,
        tx: tx?,
        ctl_phys,
        ctl_page,
        hdr_phys,
        hdr,
        data_phys,
        data,
        busy: [false; BUFS],
        started: false,
        input: None,
    };
    // Stream 0 must be an output taking 16-bit samples at 48 kHz.
    let info = snd.control(&[R_PCM_INFO, 0, 1, 32], 4 + 32)?;
    // pcm_info: hda_fn_nid u32, features u32, formats u64, rates u64,
    // direction u8, channels_min u8, channels_max u8.
    let formats = u64::from(info[2]) | u64::from(info[3]) << 32;
    let rates = u64::from(info[4]) | u64::from(info[5]) << 32;
    let direction = info[6] as u8;
    let channels_max = (info[6] >> 16) as u8;
    if direction != DIRECTION_OUTPUT
        || formats & (1 << FMT_S16) == 0
        || rates & (1 << RATE_48000) == 0
        || channels_max < 2
    {
        return Err("stream-0-not-48k-stereo-output");
    }
    // Stream 1, if it is an input taking 16-bit mono at 16 kHz.
    if let Ok(info) = snd.control(&[R_PCM_INFO, INPUT_STREAM, 1, 32], 4 + 32) {
        let formats = u64::from(info[2]) | u64::from(info[3]) << 32;
        let rates = u64::from(info[4]) | u64::from(info[5]) << 32;
        let direction = info[6] as u8;
        let channels_min = (info[6] >> 8) as u8;
        if direction == DIRECTION_INPUT
            && formats & (1 << FMT_S16) != 0
            && rates & (1 << RATE_16000) != 0
            && channels_min <= 1
        {
            let (data_phys, data) = page(mem, (RBUFS * RPERIOD).div_ceil(4096))?;
            snd.input = Some(Input {
                rx: rx?,
                data_phys,
                data,
                queued: [false; RBUFS],
                recording: false,
                ring: alloc::collections::VecDeque::with_capacity(RING),
                dropped: 0,
            });
        }
    }
    Ok(snd)
}

impl Sound {
    /// One control request (`words`, little-endian u32s) and its response
    /// (`resp_len` bytes, first word the status), waited for. Returns the
    /// response words after the status.
    fn control(&mut self, words: &[u32], resp_len: usize) -> Result<[u32; 8], &'static str> {
        // SAFETY: the control page belongs to this driver; the request
        // fits its first half and the response its second.
        unsafe {
            let req = self.ctl_page as *mut u32;
            for (k, &w) in words.iter().enumerate() {
                req.add(k).write_volatile(w);
            }
            core::ptr::write_bytes(self.ctl_page.add(2048), 0, 64);
        }
        self.ctl
            .desc(0, self.ctl_phys, 4 * words.len(), false, Some(1));
        self.ctl.desc(1, self.ctl_phys + 2048, resp_len, true, None);
        self.ctl.push(0);
        let mut done = false;
        for _ in 0..SPINS {
            if self.ctl.take_used().is_some() {
                done = true;
                break;
            }
            core::hint::spin_loop();
        }
        if !done {
            return Err("control-timeout");
        }
        let mut out = [0u32; 8];
        // SAFETY: the response in the control page's second half.
        let status = unsafe {
            let resp = self.ctl_page.add(2048) as *const u32;
            for (k, o) in out.iter_mut().enumerate() {
                *o = resp.add(1 + k).read_volatile();
            }
            resp.read_volatile()
        };
        if status != S_OK {
            return Err("control-refused");
        }
        Ok(out)
    }

    /// Sets stream 0 up, prepares and starts it.
    fn start(&mut self) -> Result<(), &'static str> {
        let fmt = u32::from(2u8) | u32::from(FMT_S16) << 8 | u32::from(RATE_48000) << 16;
        self.control(
            &[
                R_PCM_SET_PARAMS,
                0,
                (BUFS * PERIOD) as u32,
                PERIOD as u32,
                0,
                fmt,
            ],
            4,
        )?;
        self.control(&[R_PCM_PREPARE, 0], 4)?;
        self.control(&[R_PCM_START, 0], 4)?;
        self.started = true;
        Ok(())
    }

    /// Takes back the period buffers the device has played.
    fn reclaim(&mut self) {
        while let Some(head) = self.tx.take_used() {
            if let Some(b) = self.busy.get_mut(head as usize / 3) {
                *b = false;
            }
        }
    }

    /// Copies what fits of `samples` into free period buffers and hands
    /// them to the device; returns the bytes taken (whole frames).
    fn write(&mut self, samples: &[u8]) -> usize {
        self.reclaim();
        let mut taken = 0;
        for i in 0..BUFS {
            let left = samples.len() - taken;
            if left < 4 {
                break;
            }
            if self.busy[i] {
                continue;
            }
            let n = left.min(PERIOD) & !3;
            // SAFETY: period `i` of this driver's data frames
            // (`BUFS * PERIOD` bytes), and its header slot in the header
            // page; the device does not hold buffer `i` (not busy).
            unsafe {
                core::ptr::copy_nonoverlapping(
                    samples.as_ptr().add(taken),
                    self.data.add(i * PERIOD),
                    n,
                );
                // virtio_snd_pcm_xfer: stream 0.
                (self.hdr.add(16 * i) as *mut u32).write_volatile(0);
            }
            let d = 3 * i;
            let hdr = self.hdr_phys + 16 * i as u64;
            self.tx.desc(d, hdr, 4, false, Some(d + 1));
            self.tx.desc(
                d + 1,
                self.data_phys + (i * PERIOD) as u64,
                n,
                false,
                Some(d + 2),
            );
            self.tx.desc(d + 2, hdr + 8, 8, true, None);
            self.busy[i] = true;
            self.tx.push(d);
            taken += n;
        }
        taken
    }
}

/// Plays as much of `samples` (16-bit stereo at 48 kHz, little-endian) as
/// the free period buffers take now; `Err` when there is no sound device
/// or it refused to start. Never waits for the speaker.
pub fn write(samples: &[u8]) -> Result<usize, &'static str> {
    let mut g = SOUND.lock();
    let snd = g.as_mut().ok_or("no-sound-device")?;
    if !snd.started {
        snd.start()?;
        kprintln!("ZOO_OS INFO sound stream started");
    }
    Ok(snd.write(samples))
}

impl Sound {
    /// Sets the input stream up, hands the device every receive buffer and
    /// starts it.
    fn start_recording(&mut self) -> Result<(), &'static str> {
        let fmt = 1 | u32::from(FMT_S16) << 8 | u32::from(RATE_16000) << 16;
        self.control(
            &[
                R_PCM_SET_PARAMS,
                INPUT_STREAM,
                (RBUFS * RPERIOD) as u32,
                RPERIOD as u32,
                0,
                fmt,
            ],
            4,
        )?;
        self.control(&[R_PCM_PREPARE, INPUT_STREAM], 4)?;
        let (hdr, hdr_phys) = (self.hdr, self.hdr_phys);
        let input = self.input.as_mut().ok_or("no-microphone")?;
        for i in 0..RBUFS {
            if !input.queued[i] {
                input.queue(i, hdr, hdr_phys);
            }
        }
        self.control(&[R_PCM_START, INPUT_STREAM], 4)?;
        if let Some(input) = self.input.as_mut() {
            input.recording = true;
        }
        Ok(())
    }

    /// Stops and releases the input stream and forgets what was recorded.
    fn stop_recording(&mut self) {
        let _ = self.control(&[R_PCM_STOP, INPUT_STREAM], 4);
        let _ = self.control(&[R_PCM_RELEASE, INPUT_STREAM], 4);
        if let Some(input) = self.input.as_mut() {
            input.recording = false;
            // Released buffers come back; take what has (bounded).
            for _ in 0..SPINS / 1000 {
                while let Some((head, _)) = input.rx.take_used_len() {
                    if let Some(q) = input.queued.get_mut(head as usize / 3) {
                        *q = false;
                    }
                }
                if !input.queued.iter().any(|q| *q) {
                    break;
                }
                core::hint::spin_loop();
            }
            input.ring.clear();
        }
    }
}

impl Input {
    /// Hands receive buffer `i` to the device: its header (stream id),
    /// samples, then status.
    fn queue(&mut self, i: usize, hdr: *mut u8, hdr_phys: u64) {
        let at = RHDR + 16 * i;
        // SAFETY: header slot `i` of the receive headers, inside this
        // driver's header page (RHDR + 16 * RBUFS < 4096); the device does
        // not hold buffer `i` (not queued).
        unsafe { (hdr.add(at) as *mut u32).write_volatile(INPUT_STREAM) };
        let d = 3 * i;
        let h = hdr_phys + at as u64;
        self.rx.desc(d, h, 4, false, Some(d + 1));
        self.rx.desc(
            d + 1,
            self.data_phys + (i * RPERIOD) as u64,
            RPERIOD,
            true,
            Some(d + 2),
        );
        self.rx.desc(d + 2, h + 8, 8, true, None);
        self.queued[i] = true;
        self.rx.push(d);
    }

    /// Moves every buffer the device has filled into the ring (dropping the
    /// oldest samples when full) and queues it again.
    fn drain(&mut self, hdr: *mut u8, hdr_phys: u64) {
        while let Some((head, len)) = self.rx.take_used_len() {
            let i = head as usize / 3;
            if i >= RBUFS {
                continue;
            }
            self.queued[i] = false;
            // The device wrote the samples and the 8-byte status.
            let n = (len as usize).saturating_sub(8).min(RPERIOD) & !1;
            for k in 0..n {
                if self.ring.len() == RING {
                    self.ring.pop_front();
                    self.dropped += 1;
                }
                // SAFETY: byte `k` < RPERIOD of receive period `i`, which
                // the device has returned (used), so it no longer writes it.
                self.ring
                    .push_back(unsafe { self.data.add(i * RPERIOD + k).read_volatile() });
            }
            if self.recording {
                self.queue(i, hdr, hdr_phys);
            }
        }
    }
}

/// Copies what has been recorded since the last call (16-bit mono at
/// 16 kHz, little-endian) into `out`, starting the microphone on the first
/// call; the bytes copied, 0 when nothing new is there yet. Never waits.
pub fn read(out: &mut [u8]) -> Result<usize, &'static str> {
    let mut g = SOUND.lock();
    let snd = g.as_mut().ok_or("no-sound-device")?;
    if snd.input.is_none() {
        return Err("no-microphone");
    }
    if !snd.input.as_ref().is_some_and(|i| i.recording) {
        snd.start_recording()?;
        kprintln!("ZOO_OS INFO microphone started");
    }
    let (hdr, hdr_phys) = (snd.hdr, snd.hdr_phys);
    let input = snd.input.as_mut().ok_or("no-microphone")?;
    input.drain(hdr, hdr_phys);
    let n = input.ring.len().min(out.len()) & !1;
    for (o, b) in out[..n].iter_mut().zip(input.ring.drain(..n)) {
        *o = b;
    }
    Ok(n)
}

/// Stops the microphone (its owner exited).
pub fn stop_recording() {
    let mut g = SOUND.lock();
    if let Some(snd) = g.as_mut()
        && snd.input.as_ref().is_some_and(|i| i.recording)
    {
        let dropped = snd.input.as_ref().map_or(0, |i| i.dropped);
        snd.stop_recording();
        kprintln!("ZOO_OS INFO microphone stopped dropped={}", dropped);
    }
}
