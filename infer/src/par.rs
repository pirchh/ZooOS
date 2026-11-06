//! Parallel matrix work (docs/smp-design.md, step 3). The engine has no
//! threads of its own (it also runs on the host, and in ZooOS threads come
//! from the runtime), so it asks a `Pool` installed by the program: a
//! "parallel for" over job indices. Without one, jobs run here, in order.

use core::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};

/// Runs `job(0..jobs)`, each index exactly once, on any threads, and
/// returns when all have finished.
pub trait Pool: Sync {
    fn run(&self, jobs: usize, job: &(dyn Fn(usize) + Sync));
    /// Threads that work at once (the caller included).
    fn width(&self) -> usize;
}

/// The installed pool (a `&'static dyn Pool`, boxed so it fits one
/// pointer), or null.
static POOL: AtomicPtr<&'static dyn Pool> = AtomicPtr::new(core::ptr::null_mut());
/// Below this many weight bytes a matrix-vector product is not split:
/// handing it out costs more than it saves.
pub const MIN_SPLIT_BYTES: usize = 256 * 1024;
/// Matrix products split since start, for logs.
pub static SPLITS: AtomicUsize = AtomicUsize::new(0);

/// Installs `pool` for the rest of the program.
pub fn set_pool(pool: &'static dyn Pool) {
    let boxed = alloc::boxed::Box::new(pool);
    POOL.store(alloc::boxed::Box::into_raw(boxed), Ordering::Release);
}

fn pool() -> Option<&'static dyn Pool> {
    let p = POOL.load(Ordering::Acquire);
    if p.is_null() {
        None
    } else {
        // SAFETY: `p` came from `Box::into_raw` in `set_pool` and is never
        // freed, so it points at a live `&'static dyn Pool`.
        Some(unsafe { *p })
    }
}

/// Threads a pool would use (1 without one).
pub fn width() -> usize {
    pool().map_or(1, |p| p.width())
}

/// Runs `f(start, end)` over `0..rows` in contiguous ranges, in parallel
/// when a pool is installed and the work (`bytes`) is large enough.
pub fn for_rows(rows: usize, bytes: usize, f: &(dyn Fn(usize, usize) + Sync)) {
    let Some(p) = pool().filter(|p| p.width() > 1 && bytes >= MIN_SPLIT_BYTES) else {
        f(0, rows);
        return;
    };
    // A few jobs per thread, so a slow thread does not hold the rest up.
    let jobs = (p.width() * 4).min(rows).max(1);
    let per = rows.div_ceil(jobs);
    SPLITS.fetch_add(1, Ordering::Relaxed);
    p.run(jobs, &|j| {
        let start = j * per;
        let end = (start + per).min(rows);
        if start < end {
            f(start, end);
        }
    });
}

/// A pointer shared between the jobs of one `fill`, each writing only its
/// own rows.
#[derive(Clone, Copy)]
struct Out(*mut f32);
// SAFETY: jobs of one `fill` call write disjoint indices through it, and
// the call returns only after every job has finished.
unsafe impl Send for Out {}
// SAFETY: as above.
unsafe impl Sync for Out {}

impl Out {
    /// Writes `v` at index `i`.
    ///
    /// # Safety
    /// `i` is inside the slice `Out` was made from, and no other job reads
    /// or writes index `i` during the same `fill`.
    unsafe fn put(&self, i: usize, v: f32) {
        // SAFETY: forwarded from the caller's contract.
        unsafe { *self.0.add(i) = v };
    }
}

/// `out[t * rows + r] = value(r, t)` for every row `r` and each of `n`
/// vectors `t`, rows split across the pool (row outer, so a weight row is
/// used for all `n` while in cache). `bytes` is the work's weight bytes.
pub fn fill(
    out: &mut [f32],
    rows: usize,
    n: usize,
    bytes: usize,
    value: &(dyn Fn(usize, usize) -> f32 + Sync),
) {
    assert!(out.len() >= rows * n, "output too small");
    let o = Out(out.as_mut_ptr());
    for_rows(rows, bytes, &|a, b| {
        for r in a..b {
            for t in 0..n {
                // SAFETY: `t * rows + r` < `rows * n` <= `out.len()`
                // (asserted); `for_rows` gives each job its own rows, so no
                // index is written twice or read meanwhile, and `out` is
                // borrowed mutably for the whole call.
                unsafe { o.put(t * rows + r, value(r, t)) };
            }
        }
    });
}

/// A `fill_rows` job: its rows `start..end` and `put(r, t, v)`.
pub type RowsJob<'a> = dyn Fn(usize, usize, &dyn Fn(usize, usize, f32)) + Sync + 'a;

/// Like `fill`, but each job gets a range of rows to compute as it likes:
/// `job(start, end, put)` calls `put(r, t, v)` to store
/// `out[t * rows + r] = v` for its own rows `start..end` only (checked).
pub fn fill_rows(out: &mut [f32], rows: usize, n: usize, bytes: usize, job: &RowsJob) {
    assert!(out.len() >= rows * n, "output too small");
    let o = Out(out.as_mut_ptr());
    for_rows(rows, bytes, &|a, b| {
        let put = |r: usize, t: usize, v: f32| {
            assert!(r >= a && r < b && t < n, "put outside the job's rows");
            // SAFETY: `t * rows + r` < `rows * n` <= `out.len()` (asserted
            // above and here); `r` is one of this job's rows (asserted), and
            // `for_rows` gives each job its own rows, so no index is written
            // by two jobs or read meanwhile; `out` is borrowed mutably for
            // the whole call.
            unsafe { o.put(t * rows + r, v) };
        };
        job(a, b, &put);
    });
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::sync::atomic::AtomicUsize as Counter;

    /// A pool on std threads: each `run` spreads the jobs over `width`
    /// scoped threads, taking indices from a shared counter.
    struct Scoped(usize);

    impl Pool for Scoped {
        fn run(&self, jobs: usize, job: &(dyn Fn(usize) + Sync)) {
            let next = Counter::new(0);
            std::thread::scope(|s| {
                for _ in 0..self.0 {
                    s.spawn(|| {
                        loop {
                            let i = next.fetch_add(1, Ordering::Relaxed);
                            if i >= jobs {
                                break;
                            }
                            job(i);
                        }
                    });
                }
            });
        }
        fn width(&self) -> usize {
            self.0
        }
    }

    #[test]
    fn fill_matches_serial() {
        let (rows, n) = (1000, 3);
        let value = |r: usize, t: usize| (r * 31 + t * 7) as f32 * 0.5;
        let mut serial = alloc::vec![0f32; rows * n];
        // Below the split size: serial, with no pool anyway.
        fill(&mut serial, rows, n, 0, &value);
        static POOL: Scoped = Scoped(4);
        set_pool(&POOL);
        let mut parallel = alloc::vec![-1f32; rows * n];
        fill(&mut parallel, rows, n, MIN_SPLIT_BYTES, &value);
        assert_eq!(serial, parallel);
        assert!(SPLITS.load(Ordering::Relaxed) >= 1, "the pool was used");
    }
}
