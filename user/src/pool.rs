//! A worker pool for the model's matrix work (docs/smp-design.md, step 3):
//! `zoo_infer::par::Pool` on runtime threads, which the kernel runs on any
//! processor.
//!
//! One "parallel for" at a time, from the thread that owns the pool. Each
//! batch has a generation number. A worker claims job indices by
//! compare-and-swap on one word holding the generation and the next index,
//! so a worker that wakes late can never run a job of a batch that has
//! moved on. Workers spin briefly between batches (inference sends them
//! back to back), then sleep on a futex.

use crate::thread::{futex_wait, futex_wake, spawn};
use core::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};
use zoo_infer::par::Pool;

/// Spins before a worker sleeps: about 50-100 µs.
const SPINS: u32 = 20_000;

pub struct Workers {
    /// Threads that work at once (the caller included).
    width: AtomicUsize,
    /// Bumped to publish a batch (futex word).
    generation: AtomicU32,
    /// (generation << 32) | next job index.
    next: AtomicU64,
    jobs: AtomicUsize,
    done: AtomicUsize,
    /// Address of the batch's `&dyn Fn(usize)` (a thin pointer to the
    /// reference, which lives in `run`'s frame).
    job: AtomicUsize,
    /// Workers asleep on `generation`.
    sleeping: AtomicU32,
}

impl Workers {
    /// Starts `extra` worker threads; the pool's width is `extra + 1`.
    /// Returns `None` when no thread could start.
    pub fn start(extra: usize) -> Option<&'static Workers> {
        let pool: &'static Workers = alloc::boxed::Box::leak(alloc::boxed::Box::new(Workers {
            width: AtomicUsize::new(1),
            generation: AtomicU32::new(0),
            next: AtomicU64::new(0),
            jobs: AtomicUsize::new(0),
            done: AtomicUsize::new(0),
            job: AtomicUsize::new(0),
            sleeping: AtomicU32::new(0),
        }));
        let mut started = 0;
        for _ in 0..extra {
            if spawn(move || pool.work()).is_ok() {
                started += 1;
            }
        }
        if started == 0 {
            return None;
        }
        pool.width.store(started + 1, Ordering::Release);
        Some(pool)
    }

    /// Runs jobs of the current batch until none is left, counting each.
    fn take_jobs(&self, generation: u32) {
        loop {
            let v = self.next.load(Ordering::Acquire);
            let (g, i) = ((v >> 32) as u32, (v & 0xFFFF_FFFF) as usize);
            if g != generation || i >= self.jobs.load(Ordering::Acquire) {
                return;
            }
            if self
                .next
                .compare_exchange(v, v + 1, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
            {
                continue;
            }
            let at = self.job.load(Ordering::Acquire) as *const &(dyn Fn(usize) + Sync);
            // SAFETY: the claim above succeeded for `generation`, so that
            // batch is not finished: `run` is still waiting, and the
            // reference `at` points to lives in its frame until every job
            // is done; it is rewritten only for the next batch.
            let job: &(dyn Fn(usize) + Sync) = unsafe { *at };
            job(i);
            self.done.fetch_add(1, Ordering::AcqRel);
        }
    }

    /// A worker thread's loop.
    fn work(&self) -> u8 {
        let mut seen = self.generation.load(Ordering::Acquire);
        loop {
            let mut spins = 0;
            let generation = loop {
                let g = self.generation.load(Ordering::Acquire);
                if g != seen {
                    break g;
                }
                spins += 1;
                if spins < SPINS {
                    core::hint::spin_loop();
                    continue;
                }
                self.sleeping.fetch_add(1, Ordering::AcqRel);
                // Re-checked by the kernel against `seen`: no lost wake.
                let _ = futex_wait(&self.generation, seen, 0);
                self.sleeping.fetch_sub(1, Ordering::AcqRel);
                spins = 0;
            };
            seen = generation;
            self.take_jobs(generation);
        }
    }
}

impl Pool for Workers {
    fn run(&self, jobs: usize, job: &(dyn Fn(usize) + Sync)) {
        // Workers read the job through this reference while this call waits.
        let job_ref: &(dyn Fn(usize) + Sync) = job;
        let generation = self.generation.load(Ordering::Acquire).wrapping_add(1);
        self.job
            .store(&raw const job_ref as usize, Ordering::Release);
        self.jobs.store(jobs, Ordering::Release);
        self.done.store(0, Ordering::Release);
        self.next
            .store(u64::from(generation) << 32, Ordering::Release);
        self.generation.store(generation, Ordering::Release);
        if self.sleeping.load(Ordering::Acquire) > 0 {
            futex_wake(&self.generation, self.width.load(Ordering::Relaxed) as u64);
        }
        // This thread works too, then waits for the rest.
        self.take_jobs(generation);
        while self.done.load(Ordering::Acquire) < jobs {
            core::hint::spin_loop();
        }
    }

    fn width(&self) -> usize {
        self.width.load(Ordering::Relaxed)
    }
}
