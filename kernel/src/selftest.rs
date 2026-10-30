//! Boot-time self-tests (docs/m2-design.md §9). Each prints
//! `ZOO_OS TEST <name> ok` or panics with `self-test <name>: <reason>`.

use crate::interrupts;
use crate::kprintln;
use crate::memory::frames::{FRAME_SIZE, FrameError, LOW_MEMORY_LIMIT};
use crate::memory::{self, Memory, heap};
use alloc::boxed::Box;
use alloc::vec::Vec;
use x86_64::PhysAddr;
use x86_64::structures::paging::{PageTableFlags, PhysFrame};

macro_rules! check {
    ($name:literal, $cond:expr, $($why:tt)*) => {
        if !$cond {
            panic!("self-test {}: {}", $name, format_args!($($why)*));
        }
    };
}

pub fn breakpoint() {
    let before = interrupts::breakpoints();
    x86_64::instructions::interrupts::int3();
    let after = interrupts::breakpoints();
    check!(
        "breakpoint",
        after == before + 1,
        "handler ran {} times, expected 1",
        after - before
    );
    kprintln!("ZOO_OS TEST breakpoint ok");
}

const FRAME_TEST_COUNT: usize = 64;

pub fn frames(mem: &mut Memory) {
    let start_free = mem.frames.available();
    let mut got = [PhysFrame::containing_address(PhysAddr::new(0)); FRAME_TEST_COUNT];
    for (i, slot) in got.iter_mut().enumerate() {
        let Some(f) = mem.frames.allocate() else {
            panic!("self-test frames: allocation {i} failed with {start_free} free");
        };
        let a = f.start_address().as_u64();
        check!("frames", a % FRAME_SIZE == 0, "frame {a:#x} not aligned");
        check!("frames", a >= LOW_MEMORY_LIMIT, "frame {a:#x} below 1 MiB");
        *slot = f;
    }
    for i in 0..FRAME_TEST_COUNT {
        for j in i + 1..FRAME_TEST_COUNT {
            check!(
                "frames",
                got[i] != got[j],
                "frame {:#x} handed out twice",
                got[i].start_address()
            );
        }
    }
    check!(
        "frames",
        mem.frames.available() == start_free - FRAME_TEST_COUNT,
        "free count did not drop by {FRAME_TEST_COUNT}"
    );
    // Distinct pattern per frame, written then read back through the window.
    for (i, f) in got.iter().enumerate() {
        let p = mem.phys_to_virt(f.start_address()) as *mut u64;
        for k in 0..(FRAME_SIZE as usize / 8) {
            // SAFETY: the frame is owned by this test (just allocated) and the
            // physical-memory window maps it writable; k stays within it.
            unsafe { p.add(k).write_volatile(pattern(i, k)) };
        }
    }
    for (i, f) in got.iter().enumerate() {
        let p = mem.phys_to_virt(f.start_address()) as *const u64;
        for k in 0..(FRAME_SIZE as usize / 8) {
            // SAFETY: as above, read-only access to a frame this test owns.
            let v = unsafe { p.add(k).read_volatile() };
            check!("frames", v == pattern(i, k), "frame {i} word {k} corrupted");
        }
    }
    for f in got {
        check!(
            "frames",
            mem.frames.deallocate(f).is_ok(),
            "freeing {:#x} failed",
            f.start_address()
        );
    }
    check!(
        "frames",
        mem.frames.available() == start_free,
        "free count {} after freeing, expected {start_free}",
        mem.frames.available()
    );
    check!(
        "frames",
        mem.frames.deallocate(got[0]) == Err(FrameError::DoubleFree),
        "double free not detected"
    );
    let zero = PhysFrame::containing_address(PhysAddr::new(0));
    check!(
        "frames",
        mem.frames.deallocate(zero) == Err(FrameError::NotManaged),
        "freeing unmanaged frame 0 not rejected"
    );
    check!(
        "frames",
        mem.frames.available() == start_free,
        "failed frees changed the count"
    );
    for (i, slot) in got.iter_mut().enumerate() {
        *slot = mem
            .frames
            .allocate()
            .unwrap_or_else(|| panic!("self-test frames: re-allocation {i} failed"));
    }
    for f in got {
        let _ = mem.frames.deallocate(f);
    }
    check!(
        "frames",
        mem.frames.available() == start_free,
        "free count not restored"
    );
    kprintln!(
        "ZOO_OS INFO frames total={} free={}",
        mem.frames.total(),
        mem.frames.available()
    );
    kprintln!("ZOO_OS TEST frames ok");
}

fn pattern(frame: usize, word: usize) -> u64 {
    0x0D15_EA5E_0000_0000 ^ ((frame as u64) << 20) ^ word as u64
}

pub fn paging(mem: &mut Memory) {
    let va = memory::SCRATCH_TEST;
    check!(
        "paging",
        !mem.is_mapped(va),
        "scratch page {va:#x} mapped before the test"
    );
    let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::NO_EXECUTE;
    let frame = mem
        .map_new(va, flags)
        .unwrap_or_else(|e| panic!("self-test paging: map: {e}"));
    let value = 0x0123_4567_89AB_CDEF_u64;
    // SAFETY: `va` was just mapped writable to a frame owned by this test.
    unsafe { (va as *mut u64).write_volatile(value) };
    // SAFETY: the same frame, read through the physical-memory window.
    let seen = unsafe { (mem.phys_to_virt(frame.start_address()) as *const u64).read_volatile() };
    check!(
        "paging",
        seen == value,
        "physical view read {seen:#x}, wrote {value:#x}"
    );
    let unmapped = mem
        .unmap(va)
        .unwrap_or_else(|e| panic!("self-test paging: unmap: {e}"));
    check!(
        "paging",
        unmapped == frame,
        "unmap returned a different frame"
    );
    check!(
        "paging",
        !mem.is_mapped(va),
        "page still translates after unmap"
    );
    check!(
        "paging",
        mem.frames.deallocate(frame).is_ok(),
        "could not free test frame"
    );

    check!(
        "paging",
        !mem.is_mapped(memory::HEAP_GUARD_LOW),
        "heap lower guard is mapped"
    );
    check!(
        "paging",
        !mem.is_mapped(memory::HEAP_GUARD_HIGH),
        "heap upper guard is mapped"
    );
    check!(
        "paging",
        mem.is_mapped(memory::HEAP_START),
        "heap start not mapped"
    );
    check!(
        "paging",
        mem.is_mapped(memory::HEAP_GUARD_HIGH - 1),
        "heap end not mapped"
    );
    let stack_guard = mem.stack_bottom - FRAME_SIZE;
    check!(
        "paging",
        !mem.is_mapped(stack_guard),
        "kernel stack guard {stack_guard:#x} is mapped"
    );
    check!(
        "paging",
        mem.is_mapped(mem.stack_bottom),
        "kernel stack bottom not mapped"
    );
    kprintln!("ZOO_OS TEST paging ok");
}

pub fn heap() {
    let base = heap::used();
    {
        let b = Box::new(41u64);
        check!("heap", *b + 1 == 42, "Box value wrong");
        let mut v: Vec<u64> = Vec::new();
        for i in 0..10_000u64 {
            v.push(i);
        }
        let sum: u64 = v.iter().sum();
        check!("heap", sum == 10_000 * 9_999 / 2, "Vec sum {sum} wrong");

        let layout = core::alloc::Layout::from_size_align(4096, 4096).expect("valid layout");
        // SAFETY: layout has non-zero size; the pointer is checked for null
        // and freed with the same layout.
        unsafe {
            let p = alloc::alloc::alloc(layout);
            check!("heap", !p.is_null(), "aligned allocation failed");
            check!(
                "heap",
                (p as usize).is_multiple_of(4096),
                "allocation {p:p} not 4096-aligned"
            );
            alloc::alloc::dealloc(p, layout);
        }
    }
    // Allocate and drop 8 MiB in total from a 1 MiB heap: only possible if
    // freed memory is reused.
    let mut total = 0usize;
    for round in 0..2048usize {
        let block = alloc::vec![round as u8; 4096];
        check!(
            "heap",
            block.iter().all(|&b| b == round as u8),
            "block {round} corrupted"
        );
        total += block.len();
    }
    for round in 0..8u8 {
        let big = alloc::vec![round; 512 * 1024];
        check!(
            "heap",
            big[big.len() - 1] == round,
            "512 KiB block {round} corrupted"
        );
        total += big.len();
    }
    let after = heap::used();
    check!(
        "heap",
        after == base,
        "heap usage {after} after frees, expected {base}"
    );
    kprintln!(
        "ZOO_OS INFO heap size={} churned_bytes={} used={}",
        memory::HEAP_SIZE,
        total,
        after
    );
    kprintln!("ZOO_OS TEST heap ok");
}

/// Upper bound on spin iterations while waiting for timer ticks, so a
/// non-working timer fails the test instead of hanging the kernel.
const TIMER_SPIN_LIMIT: u64 = 400_000_000;
const TIMER_TICKS_WANTED: u64 = 10;

pub fn timer() {
    let start = interrupts::ticks();
    x86_64::instructions::interrupts::enable();
    let mut spins = 0u64;
    while interrupts::ticks() < start + TIMER_TICKS_WANTED && spins < TIMER_SPIN_LIMIT {
        spins += 1;
        core::hint::spin_loop();
    }
    let got = interrupts::ticks() - start;
    check!(
        "timer",
        got >= TIMER_TICKS_WANTED,
        "only {got} ticks after {spins} spins (wanted {TIMER_TICKS_WANTED})"
    );
    kprintln!(
        "ZOO_OS INFO timer hz={} ticks={} spins={}",
        interrupts::TIMER_HZ,
        got,
        spins
    );
    kprintln!("ZOO_OS TEST timer ok");
}
