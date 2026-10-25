//! A way to tell a sleeping worker thread that there is work, from a thread
//! that must not block, allocate or take a lock (the audio thread).
//!
//! A worker that polls on a timer wakes thousands of times a second to find
//! nothing. With a [`Doorbell`] it sleeps until it is rung, and a ring costs
//! two atomic operations, plus one `unpark` only when the worker really is
//! asleep. This is the same idea as the `parked` flags in the parallel
//! executor, made general.
//!
//! A bell has one waiter. The first thread to [`wait`](Doorbell::wait) is it.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::{self, Thread};
use std::time::{Duration, Instant};

#[derive(Debug, Default)]
pub struct Doorbell {
    /// Set by a ring and cleared by the wait that answers it, so a ring that
    /// arrives while the worker is busy is not lost.
    rung: AtomicBool,
    /// The waiter is parked or about to park.
    sleeping: AtomicBool,
    waiter: OnceLock<Thread>,
    wakes: AtomicU64,
}

impl Doorbell {
    pub const fn new() -> Self {
        Self {
            rung: AtomicBool::new(false),
            sleeping: AtomicBool::new(false),
            waiter: OnceLock::new(),
            wakes: AtomicU64::new(0),
        }
    }

    /// Tells the waiter there is work. Real-time safe: no allocation, no lock,
    /// and a system call only if the waiter is asleep. Returns whether it had
    /// to wake the waiter.
    pub fn ring(&self) -> bool {
        self.rung.store(true, Ordering::SeqCst);
        self.wake_if_sleeping()
    }

    /// Wakes the waiter if it is asleep, without leaving a ring behind for it
    /// to find later. For a caller that has published the work some other way
    /// (see [`recheck`](Self::wait_unless)). Returns whether it woke it.
    pub fn wake_if_sleeping(&self) -> bool {
        if !self.sleeping.load(Ordering::SeqCst) {
            return false;
        }
        match self.waiter.get() {
            Some(thread) => {
                thread.unpark();
                true
            }
            None => false,
        }
    }

    /// Sleeps until the bell is rung, or for at most `timeout`. Returns true
    /// if it was rung. A ring made since the last wait returns at once.
    pub fn wait(&self, timeout: Option<Duration>) -> bool {
        self.wait_unless(timeout, || false)
    }

    /// Like [`wait`](Self::wait), but looks at `recheck` after announcing that
    /// it is about to sleep, and doesn't sleep if that says the work has come.
    /// A ringer that publishes work and then calls
    /// [`wake_if_sleeping`](Self::wake_if_sleeping) can't be missed.
    pub fn wait_unless(&self, timeout: Option<Duration>, recheck: impl Fn() -> bool) -> bool {
        self.waiter.get_or_init(thread::current);
        let deadline = timeout.map(|t| Instant::now() + t);
        self.sleeping.store(true, Ordering::SeqCst);
        while !self.rung.load(Ordering::SeqCst) && !recheck() {
            match deadline {
                None => thread::park(),
                Some(deadline) => {
                    let now = Instant::now();
                    if now >= deadline {
                        break;
                    }
                    thread::park_timeout(deadline - now);
                }
            }
        }
        self.sleeping.store(false, Ordering::SeqCst);
        self.wakes.fetch_add(1, Ordering::Relaxed);
        self.rung.swap(false, Ordering::SeqCst)
    }

    /// Whether the waiter is asleep (or about to be).
    pub fn is_sleeping(&self) -> bool {
        self.sleeping.load(Ordering::SeqCst)
    }

    /// How many times [`wait`](Self::wait) has returned, for tests that check
    /// an idle worker stays idle.
    pub fn wakes(&self) -> u64 {
        self.wakes.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn a_ring_before_the_wait_is_not_lost() {
        let bell = Doorbell::new();
        assert!(!bell.ring());
        assert!(bell.wait(Some(Duration::from_secs(5))));
    }

    #[test]
    fn a_wait_times_out_unrung() {
        let bell = Doorbell::new();
        assert!(!bell.wait(Some(Duration::from_millis(10))));
        assert_eq!(bell.wakes(), 1);
    }

    #[test]
    fn a_ring_wakes_a_sleeper() {
        let bell = Arc::new(Doorbell::new());
        let waiter = {
            let bell = bell.clone();
            thread::spawn(move || bell.wait(None))
        };
        while !bell.is_sleeping() {
            thread::yield_now();
        }
        assert!(bell.ring());
        assert!(waiter.join().unwrap());
    }

    #[test]
    fn a_sleeper_wakes_once_however_many_rings() {
        let bell = Arc::new(Doorbell::new());
        let waiter = {
            let bell = bell.clone();
            thread::spawn(move || bell.wait(None))
        };
        while !bell.is_sleeping() {
            thread::yield_now();
        }
        for _ in 0..100 {
            bell.ring();
        }
        waiter.join().unwrap();
        assert_eq!(bell.wakes(), 1);
    }

    #[test]
    fn the_recheck_stops_a_sleep() {
        let bell = Doorbell::new();
        assert!(!bell.wait_unless(None, || true));
    }
}
