//! CaribouTest: the thread test (docs/posix1-design.md §5). Four worker
//! threads share a counter behind a futex mutex while each keeps its own
//! thread-local value (FS base); the main thread joins them, checks the
//! results, and finally exits while one more thread is still running, which
//! must end the whole process. Exit codes name the failed check.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, Ordering};
use zoo_rt::thread::{self, Mutex};
use zoo_rt::{Error, close, wait};

zoo_rt::entry!(main);

const WORKERS: u64 = 4;
const ROUNDS: u64 = 20_000;

static COUNTER: Mutex<u64> = Mutex::new(0);
/// Thread-local values seen wrong (should stay 0).
static TLS_ERRORS: AtomicU32 = AtomicU32::new(0);
/// Times the lock passed from one worker to another (interleaving).
static HANDOVERS: AtomicU32 = AtomicU32::new(0);
static LAST_OWNER: AtomicU32 = AtomicU32::new(u32::MAX);

/// A thread-local block: FS:[0] points to itself, FS:[8] is the owner id.
#[repr(C)]
struct Tls {
    me: u64,
    id: u64,
}

fn install_tls(id: u64) -> bool {
    let block = Box::leak(Box::new(Tls { me: 0, id }));
    block.me = block as *mut Tls as u64;
    thread::set_fs_base(block.me).is_ok()
}

fn worker(id: u64) -> u8 {
    if !install_tls(id) {
        return 1;
    }
    for round in 0..ROUNDS {
        {
            let mut c = COUNTER.lock();
            if LAST_OWNER.swap(id as u32, Ordering::Relaxed) != id as u32 {
                HANDOVERS.fetch_add(1, Ordering::Relaxed);
            }
            let v = *c;
            // Now and then sleep while holding the lock, so the other
            // threads find it taken and must wait on the futex.
            if round % 5000 == 0 {
                zoo_rt::sleep(2);
            }
            *c = v + 1;
        }
        if round % 500 == 0 && thread::fs_read_u64::<8>() != id {
            TLS_ERRORS.fetch_add(1, Ordering::Relaxed);
        }
    }
    if thread::fs_read_u64::<8>() != id {
        TLS_ERRORS.fetch_add(1, Ordering::Relaxed);
    }
    (10 + id) as u8
}

fn main(_start_handle: u64) -> u8 {
    if !install_tls(100) {
        return 2;
    }
    let t0 = zoo_rt::ticks();
    let mut handles = Vec::new();
    for id in 0..WORKERS {
        match thread::spawn(move || worker(id)) {
            Ok(h) => handles.push((id, h)),
            Err(e) => {
                zoo_rt::log!("CaribouTest: thread_create failed: {}", e.name());
                return 3;
            }
        }
    }
    for &(id, h) in &handles {
        match wait(h) {
            Ok(code) if code as u64 == 10 + id => {}
            other => {
                zoo_rt::log!("CaribouTest: worker {} ended with {:?}", id, other);
                return 4;
            }
        }
        let _ = close(h);
    }
    let total = *COUNTER.lock();
    let tls_errors = TLS_ERRORS.load(Ordering::Relaxed);
    let main_tls = thread::fs_read_u64::<8>();
    zoo_rt::log!(
        "CaribouTest: {} threads x {} rounds: counter {}, lock waited {} times, {} handovers, TLS errors {}, main TLS {}, {} ticks",
        WORKERS,
        ROUNDS,
        total,
        COUNTER.contended(),
        HANDOVERS.load(Ordering::Relaxed),
        tls_errors,
        main_tls,
        zoo_rt::ticks() - t0
    );
    if total != WORKERS * ROUNDS {
        return 5;
    }
    if tls_errors != 0 || main_tls != 100 {
        return 6;
    }
    if COUNTER.contended() == 0 {
        // The futex path was never exercised; the test proved too little.
        return 7;
    }
    // A futex wait whose value never changes must time out.
    let word = AtomicU32::new(7);
    match thread::futex_wait(&word, 7, 5) {
        Err(Error::TimedOut) => {}
        other => {
            zoo_rt::log!("CaribouTest: futex timeout gave {:?}", other);
            return 8;
        }
    }
    // Only futex_wake can release this waiter: the value never changes.
    static GATE: AtomicU32 = AtomicU32::new(0);
    let Ok(waiter) = thread::spawn(|| match thread::futex_wait(&GATE, 0, 0) {
        Ok(()) => 0,
        Err(_) => 1,
    }) else {
        return 10;
    };
    // Give the waiter time to block, then wake it without changing GATE.
    zoo_rt::sleep(20);
    let woken = thread::futex_wake(&GATE, 1);
    if woken != 1 || wait(waiter) != Ok(0) {
        zoo_rt::log!("CaribouTest: futex_wake woke {} (expected 1)", woken);
        return 10;
    }
    let _ = close(waiter);
    // A thread still running when the process exits must end with it.
    if thread::spawn(|| {
        loop {
            zoo_rt::sleep(10);
        }
    })
    .is_err()
    {
        return 9;
    }
    let _ =
        zoo_rt::log("CaribouTest: all thread checks passed; exiting with a thread still running");
    0
}
