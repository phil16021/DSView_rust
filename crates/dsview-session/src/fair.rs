//! A mutex that a busy loop hands over to waiting callers (F42, F43).
//!
//! The collect thread and a live decode lock a shared value, work a while
//! (a USB wait, a chunk of decoding) and lock it again at once. The std
//! mutex is not fair: the thread that just unlocked takes it again before a
//! parked waiter has woken up, again and again, and the GUI waited hundreds
//! of milliseconds for a frame. [`Fair::lock`] counts its waiters, and
//! [`Fair::lock_after_waiters`], for such a loop, lets them go first.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{LockResult, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// A [`Mutex`] that knows how many callers of [`lock`](Self::lock) wait.
pub struct Fair<T> {
    m: Mutex<T>,
    waiting: AtomicUsize,
    stat: lock_stats::Stat,
}

impl<T> Fair<T> {
    pub const fn new(value: T, stat: lock_stats::Stat) -> Self {
        Fair {
            m: Mutex::new(value),
            waiting: AtomicUsize::new(0),
            stat,
        }
    }

    /// [`Mutex::lock`], counted as a waiter until it has the lock.
    pub fn lock(&self) -> LockResult<MutexGuard<'_, T>> {
        let since = lock_stats::start();
        self.waiting.fetch_add(1, Ordering::AcqRel);
        let g = self.m.lock();
        self.waiting.fetch_sub(1, Ordering::AcqRel);
        lock_stats::record(self.stat, since);
        g
    }

    /// The lock for a loop that takes it again right after letting it go:
    /// waits up to `max` for the callers waiting in [`lock`](Self::lock) to
    /// get it first.
    pub fn lock_after_waiters(&self, max: Duration) -> LockResult<MutexGuard<'_, T>> {
        let since = Instant::now();
        while self.waiting.load(Ordering::Acquire) > 0 && since.elapsed() < max {
            std::thread::yield_now();
        }
        self.m.lock()
    }
}

/// Counts of [`Fair::lock`] calls and the time they waited, for the GUI's
/// `DSVIEW_FRAME_LOG`. Off unless [`enable`]d.
pub mod lock_stats {
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::time::Instant;

    /// Which counters a lock adds to.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Stat {
        /// The devices
        Device = 0,
        /// The sample stores
        Store = 1,
    }

    struct Counters {
        calls: AtomicU64,
        wait_ns: AtomicU64,
        max_ns: AtomicU64,
    }

    static ON: AtomicBool = AtomicBool::new(false);
    static C: [Counters; 2] = [const {
        Counters {
            calls: AtomicU64::new(0),
            wait_ns: AtomicU64::new(0),
            max_ns: AtomicU64::new(0),
        }
    }; 2];

    pub fn enable() {
        ON.store(true, Ordering::Relaxed);
    }

    pub(crate) fn start() -> Option<Instant> {
        ON.load(Ordering::Relaxed).then(Instant::now)
    }

    pub(crate) fn record(stat: Stat, since: Option<Instant>) {
        if let Some(t) = since {
            let ns = u64::try_from(t.elapsed().as_nanos()).unwrap_or(u64::MAX);
            let c = &C[stat as usize];
            c.calls.fetch_add(1, Ordering::Relaxed);
            c.wait_ns.fetch_add(ns, Ordering::Relaxed);
            c.max_ns.fetch_max(ns, Ordering::Relaxed);
        }
    }

    /// (calls, total wait, longest wait) since the last take, in ns.
    pub fn take(stat: Stat) -> (u64, u64, u64) {
        let c = &C[stat as usize];
        (
            c.calls.swap(0, Ordering::Relaxed),
            c.wait_ns.swap(0, Ordering::Relaxed),
            c.max_ns.swap(0, Ordering::Relaxed),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    /// A loop that holds the lock 2 ms and takes it again at once (the
    /// collect thread, a live decode) must not starve a caller of `lock`.
    #[test]
    fn a_busy_loop_lets_waiters_go_first() {
        let f = Arc::new(Fair::new(0u64, lock_stats::Stat::Store));
        let stop = Arc::new(AtomicBool::new(false));
        let busy = {
            let (f, stop) = (f.clone(), stop.clone());
            std::thread::spawn(move || {
                // bounded, so a starved caller fails the test instead of hanging it
                let start = Instant::now();
                while !stop.load(Ordering::Relaxed) && start.elapsed() < Duration::from_secs(3) {
                    let mut g = f.lock_after_waiters(Duration::from_millis(5)).unwrap();
                    let t = Instant::now();
                    while t.elapsed() < Duration::from_millis(2) {
                        std::hint::spin_loop();
                    }
                    *g += 1;
                }
            })
        };
        std::thread::sleep(Duration::from_millis(20));
        let mut worst = Duration::ZERO;
        for _ in 0..50 {
            let t = Instant::now();
            drop(f.lock().unwrap());
            worst = worst.max(t.elapsed());
            std::thread::sleep(Duration::from_millis(1));
        }
        stop.store(true, Ordering::Relaxed);
        busy.join().unwrap();
        // one hold of the loop, plus scheduling slack on a loaded machine
        assert!(worst < Duration::from_millis(50), "waited {worst:?}");
    }
}
