//! Address-space reservations and commits (docs/posix2-design.md): thin
//! wrappers over system calls 32-37.

use crate::{Error, result, sys, syscall};
pub use zoo_abi::{VM_COMMITTED, VM_END, VM_FREE, VM_RESERVED, VM_START, VmInfo, prot};

/// Reserves `len` bytes at `addr` (0: the kernel chooses); returns the base.
pub fn reserve(addr: u64, len: u64) -> Result<u64, Error> {
    result(syscall(sys::VM_RESERVE, addr, len, 0))
}

/// Backs `[addr, addr+len)` with zeroed pages (existing ones keep their
/// contents) and sets their protection.
pub fn commit(addr: u64, len: u64, prot: u64) -> Result<(), Error> {
    result(syscall(sys::VM_COMMIT, addr, len, prot)).map(|_| ())
}

/// Changes the protection of committed pages.
pub fn protect(addr: u64, len: u64, prot: u64) -> Result<(), Error> {
    result(syscall(sys::VM_PROTECT, addr, len, prot)).map(|_| ())
}

/// Frees the pages of `[addr, addr+len)`; the range stays reserved.
pub fn decommit(addr: u64, len: u64) -> Result<(), Error> {
    result(syscall(sys::VM_DECOMMIT, addr, len, 0)).map(|_| ())
}

/// Frees and unreserves the whole reservation starting at `base`.
pub fn release(base: u64) -> Result<(), Error> {
    result(syscall(sys::VM_RELEASE, base, 0, 0)).map(|_| ())
}

/// State of the run of pages starting at `addr`.
pub fn query(addr: u64) -> Result<VmInfo, Error> {
    let mut info = VmInfo::default();
    result(syscall(
        sys::VM_QUERY,
        addr,
        &mut info as *mut VmInfo as u64,
        0,
    ))?;
    Ok(info)
}
