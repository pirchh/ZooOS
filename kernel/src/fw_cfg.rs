//! QEMU's firmware configuration device (fw_cfg), read through its x86 I/O
//! ports (docs/u2b-design.md): named files the host passes to the guest
//! with `-fw_cfg name=...,string=...`. Only used after a QEMU-only device
//! (the virtio GPU) has been found, so real hardware never sees these
//! port accesses.

use x86_64::instructions::port::Port;

const SELECTOR: u16 = 0x510;
const DATA: u16 = 0x511;
const SIGNATURE: u16 = 0x0000;
const FILE_DIR: u16 = 0x0019;

fn select(key: u16) {
    // SAFETY: the fw_cfg selector port of QEMU's machine (checked by the
    // caller to be a QEMU guest); selecting an item has no other effect.
    unsafe { Port::<u16>::new(SELECTOR).write(key) };
}

fn read(buf: &mut [u8]) {
    for b in buf {
        // SAFETY: the fw_cfg data port: each read returns the next byte of
        // the selected item (zeros past its end).
        *b = unsafe { Port::<u8>::new(DATA).read() };
    }
}

/// Copies fw_cfg file `name` into `buf`; returns its length, or `None`
/// if there is no fw_cfg device or no such file.
pub fn read_file(name: &str, buf: &mut [u8]) -> Option<usize> {
    let mut sig = [0u8; 4];
    select(SIGNATURE);
    read(&mut sig);
    if &sig != b"QEMU" {
        return None;
    }
    select(FILE_DIR);
    let mut n = [0u8; 4];
    read(&mut n);
    let count = u32::from_be_bytes(n).min(1024);
    for _ in 0..count {
        // size (u32), select (u16), reserved (u16), name (56 bytes), all
        // big-endian.
        let mut entry = [0u8; 64];
        read(&mut entry);
        let size = u32::from_be_bytes([entry[0], entry[1], entry[2], entry[3]]) as usize;
        let key = u16::from_be_bytes([entry[4], entry[5]]);
        let raw = &entry[8..];
        let len = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
        if &raw[..len] == name.as_bytes() {
            let n = size.min(buf.len());
            select(key);
            read(&mut buf[..n]);
            return Some(n);
        }
    }
    None
}
