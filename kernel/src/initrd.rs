//! The initial filesystem (docs/m3-design.md §8): a read-only ustar archive
//! passed by the bootloader as its ramdisk. Validated in full at boot.

use bootloader_api::BootInfo;
use spin::Once;
use zoo_abi::tar;

static INITRD: Once<&'static [u8]> = Once::new();

/// Validates the archive and records it. Returns (file count, bytes).
pub fn init(boot_info: &BootInfo) -> Result<(usize, usize), &'static str> {
    let addr = boot_info
        .ramdisk_addr
        .into_option()
        .ok_or("no ramdisk was loaded")?;
    let len = boot_info.ramdisk_len as usize;
    // SAFETY: the bootloader mapped the ramdisk at `addr` (kernel half,
    // read-only use here) for `len` bytes, in memory that is not `Usable`,
    // so the frame allocator never hands it out; it stays mapped for the
    // kernel's lifetime.
    let data: &'static [u8] = unsafe { core::slice::from_raw_parts(addr as *const u8, len) };
    let mut files = 0;
    for entry in tar::read(data) {
        entry.map_err(|_| "initrd archive is malformed")?;
        files += 1;
    }
    INITRD.call_once(|| data);
    Ok((files, len))
}

/// Contents of `name`, if the archive has it.
pub fn find(name: &str) -> Option<&'static [u8]> {
    // Validated at init, so lookup errors cannot occur; treat them as absent.
    tar::find(INITRD.get()?, name).ok().flatten()
}

/// Calls `f(name, size)` for every file.
pub fn for_each(mut f: impl FnMut(&str, usize)) {
    if let Some(data) = INITRD.get() {
        for e in tar::read(data).flatten() {
            f(e.name, e.data.len());
        }
    }
}

/// The `index`-th file as (name, size, byte offset of its data in the
/// archive), if it exists.
pub fn entry(index: usize) -> Option<(&'static str, usize, usize)> {
    let data = INITRD.get()?;
    let e = tar::read(data).flatten().nth(index)?;
    let offset = e.data.as_ptr() as usize - data.as_ptr() as usize;
    Some((e.name, e.data.len(), offset))
}

/// Kernel virtual address where the archive starts.
pub fn base() -> Option<u64> {
    INITRD.get().map(|d| d.as_ptr() as u64)
}
