//! WolverineTest: the exception-delivery test (docs/posix3-design.md §5).
//! Started without a handle it runs the checks and exits 0 only if all
//! pass. Started with a channel, it runs one failure mode named by the
//! first message, whose outcome the parent checks.

#![no_std]
#![no_main]

use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use zoo_rt::exception::{self, ExceptionContext};
use zoo_rt::vm::{self, prot};
use zoo_rt::{Error, NO_HANDLE, channel_create, close, recv, send, spawn, thread, wait};

zoo_rt::entry!(main);

const PAGE: u64 = 4096;
const PAGES: u64 = 16;
const VEC_DIVIDE: u64 = 0;
const VEC_PAGE_FAULT: u64 = 14;
/// Page-fault error bit: the page was present (a protection fault).
const PF_PRESENT: u64 = 1;

/// Demand-committed region: [DEMAND, DEMAND + PAGES * PAGE).
static DEMAND: AtomicU64 = AtomicU64::new(0);
static COMMITS: AtomicU32 = AtomicU32::new(0);
static DIVIDES: AtomicU32 = AtomicU32::new(0);
/// Child failure mode: 0 none, 1 nested fault, 2 bad rip.
static MODE: AtomicU32 = AtomicU32::new(0);

fn handler(ctx: &mut ExceptionContext) -> bool {
    match MODE.load(Ordering::Relaxed) {
        1 => {
            // Fault again inside the handler: must be fatal.
            // SAFETY: deliberately reads an unmapped address.
            let v = unsafe { (0x20 as *const u64).read_volatile() };
            core::hint::black_box(v);
        }
        2 => {
            ctx.rip = 0xFFFF_8000_0000_0000;
            return true;
        }
        _ => {}
    }
    // Clobber registers the interrupted code relies on; the kernel must
    // restore them from the context.
    // SAFETY: only overwrites registers declared as clobbered.
    unsafe {
        core::arch::asm!(
            "mov r12, 0", "mov r13, 0", "xorps xmm0, xmm0",
            out("r12") _, out("r13") _, out("xmm0") _,
            options(nomem, nostack)
        );
    }
    let base = DEMAND.load(Ordering::Relaxed);
    if ctx.vector == VEC_PAGE_FAULT
        && ctx.error & PF_PRESENT == 0
        && ctx.addr >= base
        && ctx.addr < base + PAGES * PAGE
    {
        let page = ctx.addr & !(PAGE - 1);
        if vm::commit(page, PAGE, prot::READ | prot::WRITE).is_err() {
            return false;
        }
        COMMITS.fetch_add(1, Ordering::Relaxed);
        return true;
    }
    if ctx.vector == VEC_DIVIDE {
        // Skip `div rcx` (48 F7 F1) and supply a result.
        ctx.rip += 3;
        ctx.rax = 777;
        DIVIDES.fetch_add(1, Ordering::Relaxed);
        return true;
    }
    false
}

fn main(start: u64) -> u8 {
    if start != NO_HANDLE {
        return child(start);
    }
    match checks() {
        Ok(()) => {
            let _ = zoo_rt::log("WolverineTest: all exception checks passed");
            0
        }
        Err((code, what)) => {
            zoo_rt::log!("WolverineTest: check {} failed: {}", code, what);
            code
        }
    }
}

fn check(ok: bool, code: u8, what: &'static str) -> Result<(), (u8, &'static str)> {
    if ok { Ok(()) } else { Err((code, what)) }
}

/// Writes then reads one word in each demand page.
fn touch_all(base: u64, salt: u64) -> bool {
    for k in 0..PAGES {
        let p = (base + k * PAGE + 8) as *mut u64;
        // SAFETY: inside this process's reservation; a missing page faults
        // and the handler commits it before the access is retried.
        unsafe { p.write_volatile(k ^ salt) };
    }
    (0..PAGES).all(|k| {
        // SAFETY: as above.
        unsafe { ((base + k * PAGE + 8) as *const u64).read_volatile() == k ^ salt }
    })
}

fn checks() -> Result<(), (u8, &'static str)> {
    // 1. exception_return outside a handler is refused.
    let blank = ExceptionContext {
        vector: 0,
        error: 0,
        addr: 0,
        rip: 0,
        rsp: 0,
        rflags: 0,
        rax: 0,
        rbx: 0,
        rcx: 0,
        rdx: 0,
        rsi: 0,
        rdi: 0,
        rbp: 0,
        r8: 0,
        r9: 0,
        r10: 0,
        r11: 0,
        r12: 0,
        r13: 0,
        r14: 0,
        r15: 0,
        fs_base: 0,
        fx: [0; 512],
    };
    check(
        exception::raw_return(&blank, exception::EXC_RESUME) == Err(Error::Inval),
        20,
        "exception_return outside a handler refused",
    )?;
    check(exception::set_handler(handler).is_ok(), 20, "set handler")?;
    check(
        exception::use_own_stack(64 * 1024).is_ok(),
        20,
        "exception stack",
    )?;

    // 2. Demand commit: every first touch faults once and is resumed.
    let base = vm::reserve(0, 1 << 20).map_err(|_| (21, "reserve"))?;
    DEMAND.store(base, Ordering::Relaxed);
    check(
        touch_all(base, 0xA5),
        21,
        "demand-committed memory round-trip",
    )?;
    check(
        COMMITS.load(Ordering::Relaxed) == PAGES as u32,
        21,
        "one fault per page",
    )?;
    check(touch_all(base, 0x3C), 21, "second pass")?;
    check(
        COMMITS.load(Ordering::Relaxed) == PAGES as u32,
        21,
        "no faults on committed pages",
    )?;

    // 3. Divide by zero: skipped and given a result; registers the handler
    // clobbered come back.
    let (out, r12, r13, x): (u64, u64, u64, f64);
    // SAFETY: `div rcx` with rcx = 0 faults; the handler skips it and sets
    // rax. Every register used is declared.
    unsafe {
        core::arch::asm!(
            "div rcx",
            in("rcx") 0u64,
            inout("rax") 5u64 => out,
            inout("rdx") 0u64 => _,
            inout("r12") 0x1212_1212_1212_1212u64 => r12,
            inout("r13") 0x1313_1313_1313_1313u64 => r13,
            inout("xmm0") 2.5f64 => x,
            options(nostack)
        );
    }
    check(out == 777, 22, "handler-supplied result")?;
    check(DIVIDES.load(Ordering::Relaxed) == 1, 22, "one divide fault")?;
    check(
        r12 == 0x1212_1212_1212_1212 && r13 == 0x1313_1313_1313_1313,
        22,
        "general registers restored",
    )?;
    check(x == 2.5, 22, "SSE register restored")?;

    // 4. A fault in another thread is handled on that thread's own stack.
    DEMAND.store(0, Ordering::Relaxed);
    let base2 = vm::reserve(0, 1 << 20).map_err(|_| (23, "reserve"))?;
    DEMAND.store(base2, Ordering::Relaxed);
    let worker = thread::spawn(move || {
        if exception::use_own_stack(64 * 1024).is_err() {
            return 1;
        }
        if touch_all(base2, 0x77) { 0 } else { 2 }
    })
    .map_err(|_| (23, "spawn"))?;
    check(wait(worker) == Ok(0), 23, "worker thread's faults handled")?;
    check(
        COMMITS.load(Ordering::Relaxed) == 2 * PAGES as u32,
        23,
        "worker faults counted",
    )?;
    let _ = close(worker);

    // 5. Failure modes, each in its own process.
    for (mode, want) in [
        ("unhandled", Err(Error::Crashed)),
        ("nested", Err(Error::Crashed)),
        ("bad-rip", Ok(103)),
    ] {
        let (mine, theirs) = channel_create().map_err(|_| (24, "channel"))?;
        let child = spawn("wolverine-test", Some(theirs)).map_err(|_| (24, "spawn"))?;
        send(mine, mode.as_bytes()).map_err(|_| (24, "send"))?;
        let got = wait(child);
        let _ = close(child);
        let _ = close(mine);
        if got != want {
            zoo_rt::log!(
                "WolverineTest: `{}` ended with {:?}, expected {:?}",
                mode,
                got,
                want
            );
            return Err((24, "failure mode"));
        }
        zoo_rt::log!("WolverineTest: `{}` ended as required ({:?})", mode, got);
    }
    Ok(())
}

/// Child: install the handler, select the mode, then fault on a write to
/// an unmapped low address (not in any demand region).
fn child(chan: u64) -> u8 {
    let mut buf = [0u8; 16];
    let Ok(n) = recv(chan, &mut buf) else {
        return 2;
    };
    let mode = match &buf[..n] {
        b"unhandled" => 0,
        b"nested" => 1,
        b"bad-rip" => 2,
        _ => return 3,
    };
    MODE.store(mode, Ordering::Relaxed);
    // A fixed exception stack: if the kernel re-delivered faults raised
    // inside the handler, this would loop forever instead of crashing.
    if exception::set_handler(handler).is_err() || exception::use_own_stack(16 * 1024).is_err() {
        return 4;
    }
    // SAFETY: deliberately writes an unmapped address.
    unsafe { (0x10 as *mut u64).write_volatile(1) };
    1
}
