//! WalrusTest: the address-space test (docs/posix2-design.md §5). Started
//! without a handle it runs the checks and exits 0 only if all pass (the
//! exit code names the first failure). Started with a channel, it performs
//! one forbidden access named by the first message, which must crash it.

#![no_std]
#![no_main]

use zoo_rt::vm::{self, VM_COMMITTED, VM_FREE, VM_RESERVED, VM_START, prot};
use zoo_rt::{Error, NO_HANDLE, channel_create, close, recv, send, spawn, wait};

zoo_rt::entry!(main);

const PAGE: u64 = 4096;
const MIB: u64 = 1 << 20;
/// `mov eax, 42; ret`
const CODE: [u8; 6] = [0xB8, 0x2A, 0x00, 0x00, 0x00, 0xC3];

fn main(start: u64) -> u8 {
    if start != NO_HANDLE {
        return forbidden(start);
    }
    match checks() {
        Ok(()) => {
            let _ = zoo_rt::log("WalrusTest: all address-space checks passed");
            0
        }
        Err((code, what)) => {
            zoo_rt::log!("WalrusTest: check {} failed: {}", code, what);
            code
        }
    }
}

fn check(ok: bool, code: u8, what: &'static str) -> Result<(), (u8, &'static str)> {
    if ok { Ok(()) } else { Err((code, what)) }
}

/// Writes `CODE` at `addr` and calls it.
fn run_code(addr: u64) -> u32 {
    // SAFETY: `addr` is a committed writable page owned by this process.
    unsafe { core::ptr::copy_nonoverlapping(CODE.as_ptr(), addr as *mut u8, CODE.len()) };
    call(addr)
}

fn call(addr: u64) -> u32 {
    // SAFETY: `addr` holds `CODE`, a complete function with the C ABI
    // returning u32 in eax. If the page is not executable this faults,
    // which is what the forbidden-access child wants.
    let f: extern "C" fn() -> u32 = unsafe { core::mem::transmute(addr as usize) };
    f()
}

fn checks() -> Result<(), (u8, &'static str)> {
    // 1. A kernel-chosen reservation starts the window and is reserved.
    let base = vm::reserve(0, 64 * MIB).map_err(|_| (10, "reserve"))?;
    check(
        base == VM_START,
        10,
        "first reservation at the window start",
    )?;
    let q = vm::query(base).map_err(|_| (10, "query"))?;
    check(
        q.state == VM_RESERVED && q.len == 64 * MIB && q.reservation == base,
        10,
        "reserved range reported",
    )?;

    // 2. Fixed placement and its errors.
    let fixed = 0x8000_0000;
    check(vm::reserve(fixed, MIB) == Ok(fixed), 11, "fixed reserve")?;
    check(
        vm::reserve(fixed + PAGE, PAGE) == Err(Error::Exists),
        11,
        "overlap refused",
    )?;
    check(
        vm::reserve(0x1000, PAGE) == Err(Error::Inval),
        11,
        "outside the window refused",
    )?;
    check(
        vm::reserve(fixed + 2 * MIB + 1, PAGE) == Err(Error::Inval),
        11,
        "unaligned refused",
    )?;
    // A huge reservation costs no memory and releases quickly.
    let huge = vm::reserve(0, 4 << 40).map_err(|_| (11, "4 TiB reserve"))?;
    check(vm::release(huge).is_ok(), 11, "4 TiB release")?;

    // 3. Commit three pages read/write; contents round-trip.
    let a = base + MIB;
    check(
        vm::commit(a, 3 * PAGE, prot::READ | prot::WRITE).is_ok(),
        12,
        "commit",
    )?;
    for k in 0..3 * PAGE / 8 {
        // SAFETY: committed, writable, owned by this process.
        unsafe { ((a + k * 8) as *mut u64).write_volatile(k ^ 0x5A5A) };
    }
    for k in 0..3 * PAGE / 8 {
        // SAFETY: as above.
        let v = unsafe { ((a + k * 8) as *const u64).read_volatile() };
        check(v == k ^ 0x5A5A, 12, "committed memory round-trip")?;
    }
    let q = vm::query(a).map_err(|_| (12, "query"))?;
    check(
        q.state == VM_COMMITTED && q.len == 3 * PAGE && q.prot == prot::READ | prot::WRITE,
        12,
        "committed run reported",
    )?;
    let q = vm::query(base).map_err(|_| (12, "query"))?;
    check(
        q.state == VM_RESERVED && q.len == MIB,
        12,
        "reserved run ends at the commit",
    )?;

    // 4. Errors: outside a reservation, uncommitted pages, bad protection.
    check(
        vm::commit(fixed + MIB, PAGE, prot::READ) == Err(Error::NotFound),
        13,
        "commit outside a reservation refused",
    )?;
    check(
        vm::protect(a, 4 * PAGE, prot::READ) == Err(Error::NotFound),
        13,
        "protect of an uncommitted page refused",
    )?;
    check(
        vm::commit(a, PAGE, 0) == Err(Error::Inval),
        13,
        "protection without read refused",
    )?;

    // 5. Protection splits a run.
    check(
        vm::protect(a + PAGE, PAGE, prot::READ).is_ok(),
        14,
        "protect",
    )?;
    let q = vm::query(a).map_err(|_| (14, "query"))?;
    check(q.len == PAGE, 14, "run split by protection")?;
    let q = vm::query(a + PAGE).map_err(|_| (14, "query"))?;
    check(
        q.prot == prot::READ && q.len == PAGE,
        14,
        "read-only page reported",
    )?;

    // 6. Code runs only after the page is made executable.
    let c = base + 2 * MIB;
    check(
        vm::commit(c, PAGE, prot::READ | prot::WRITE).is_ok(),
        15,
        "commit code page",
    )?;
    // SAFETY: committed, writable.
    unsafe { core::ptr::copy_nonoverlapping(CODE.as_ptr(), c as *mut u8, CODE.len()) };
    check(
        vm::protect(c, PAGE, prot::READ | prot::EXEC).is_ok(),
        15,
        "protect R+X",
    )?;
    check(call(c) == 42, 15, "generated code ran")?;

    // 7. Decommitted memory comes back zeroed.
    check(vm::decommit(a, 3 * PAGE).is_ok(), 16, "decommit")?;
    let q = vm::query(a).map_err(|_| (16, "query"))?;
    check(
        q.state == VM_RESERVED,
        16,
        "decommitted pages reserved again",
    )?;
    check(
        vm::commit(a, PAGE, prot::READ | prot::WRITE).is_ok(),
        16,
        "recommit",
    )?;
    // SAFETY: committed, readable.
    let v = unsafe { (a as *const u64).read_volatile() };
    check(v == 0, 16, "recommitted page is zero")?;

    // 8. The commit limit is enforced before any page is allocated.
    let big = vm::reserve(0, 2 << 30).map_err(|_| (17, "2 GiB reserve"))?;
    check(
        vm::commit(big, (1 << 30) + PAGE, prot::READ) == Err(Error::NoMem),
        17,
        "commit over the limit refused",
    )?;
    check(vm::release(big).is_ok(), 17, "release")?;

    // 9. Release frees everything.
    check(vm::release(base).is_ok(), 18, "release")?;
    check(
        vm::release(base) == Err(Error::NotFound),
        18,
        "double release refused",
    )?;
    let q = vm::query(base).map_err(|_| (18, "query"))?;
    check(q.state == VM_FREE, 18, "released range is free")?;
    check(vm::release(fixed).is_ok(), 18, "release fixed")?;

    // 10. Forbidden accesses crash (each in its own process).
    for mode in [
        "touch-reserved",
        "write-readonly",
        "exec-nx",
        "use-released",
    ] {
        let (mine, theirs) = channel_create().map_err(|_| (19, "channel"))?;
        let child = spawn("walrus-test", Some(theirs)).map_err(|_| (19, "spawn"))?;
        send(mine, mode.as_bytes()).map_err(|_| (19, "send"))?;
        let outcome = wait(child);
        let _ = close(child);
        let _ = close(mine);
        if outcome != Err(Error::Crashed) {
            zoo_rt::log!(
                "WalrusTest: `{}` ended with {:?}, expected a crash",
                mode,
                outcome
            );
            return Err((19, "forbidden access did not crash"));
        }
        zoo_rt::log!("WalrusTest: `{}` crashed as required", mode);
    }
    Ok(())
}

/// Child mode: performs the access named by the first message. Reaching the
/// end means the access did not fault, which the parent reports.
fn forbidden(chan: u64) -> u8 {
    let mut buf = [0u8; 32];
    let Ok(n) = recv(chan, &mut buf) else {
        return 2;
    };
    let Ok(base) = vm::reserve(0, MIB) else {
        return 3;
    };
    match &buf[..n] {
        b"touch-reserved" => {
            // SAFETY: deliberately reads a reserved, uncommitted page.
            let v = unsafe { (base as *const u64).read_volatile() };
            core::hint::black_box(v);
        }
        b"write-readonly" => {
            if vm::commit(base, PAGE, prot::READ).is_err() {
                return 4;
            }
            // SAFETY: deliberately writes a read-only page.
            unsafe { (base as *mut u64).write_volatile(1) };
        }
        b"exec-nx" => {
            if vm::commit(base, PAGE, prot::READ | prot::WRITE).is_err() {
                return 4;
            }
            core::hint::black_box(run_code(base));
        }
        b"use-released" => {
            if vm::commit(base, PAGE, prot::READ | prot::WRITE).is_err()
                || vm::release(base).is_err()
            {
                return 4;
            }
            // SAFETY: deliberately writes a page that was just released.
            unsafe { (base as *mut u64).write_volatile(1) };
        }
        _ => return 5,
    }
    1
}
