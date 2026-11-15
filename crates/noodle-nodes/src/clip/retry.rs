//! One retry policy for files that fail to open, shared by the hub (streams)
//! and the stretch worker (lanes), so that a file that turns up later
//! (restored, or a drive mounted) starts playing, without either of them
//! hammering one that stays away.
//!
//! The first retry comes after [`FIRST`]; each failure in a row doubles the
//! wait, up to [`LONGEST`]. A success, or a new schedule (the clips changed),
//! starts it over.

use std::collections::HashMap;
use std::hash::Hash;
use std::time::{Duration, Instant};

/// The wait after a first failure.
pub(super) const FIRST: Duration = Duration::from_secs(2);
/// The longest wait, however many times in a row a file has failed.
pub(super) const LONGEST: Duration = Duration::from_secs(30);

/// The files that failed to open, and when each may be tried again.
pub(super) struct Retry<K> {
    failed: HashMap<K, Failure>,
}

struct Failure {
    /// When it may be tried again.
    until: Instant,
    /// Failures in a row.
    count: u32,
}

/// The wait after `count` failures in a row.
fn wait(count: u32) -> Duration {
    FIRST
        .checked_mul(1u32 << count.saturating_sub(1).min(8))
        .map_or(LONGEST, |d| d.min(LONGEST))
}

impl<K: Hash + Eq> Retry<K> {
    pub fn new() -> Self {
        Self {
            failed: HashMap::new(),
        }
    }

    /// `key` failed to open at `now`.
    pub fn failed(&mut self, key: K, now: Instant) {
        let count = self.failed.get(&key).map_or(1, |f| f.count + 1);
        self.failed.insert(
            key,
            Failure {
                until: now + wait(count),
                count,
            },
        );
    }

    /// Whether `key` failed recently enough that it should be left alone.
    pub fn backing_off(&self, key: &K, now: Instant) -> bool {
        self.failed.get(key).is_some_and(|f| now < f.until)
    }

    /// `key` opened: forgets it. Returns whether it had failed.
    pub fn opened(&mut self, key: &K) -> bool {
        self.failed.remove(key).is_some()
    }

    /// Forgets every failure, so each file is tried at once.
    pub fn clear(&mut self) {
        self.failed.clear();
    }

    /// How long until the first file that failed may be tried again (zero if
    /// one may be now), or `None` when none failed.
    pub fn next_wait(&self, now: Instant) -> Option<Duration> {
        self.failed
            .values()
            .map(|f| f.until.saturating_duration_since(now))
            .min()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_wait_doubles_to_a_ceiling() {
        let secs: Vec<u64> = (1..=6).map(|n| wait(n).as_secs()).collect();
        assert_eq!(secs, [2, 4, 8, 16, 30, 30]);
        assert_eq!(wait(1000), LONGEST);
    }

    #[test]
    fn a_failed_file_waits_then_is_tried_again_and_success_forgets_it() {
        let mut retry = Retry::new();
        let t0 = Instant::now();
        retry.failed("a", t0);
        assert!(retry.backing_off(&"a", t0 + Duration::from_secs(1)));
        assert!(!retry.backing_off(&"a", t0 + Duration::from_secs(2)));
        assert!(!retry.backing_off(&"b", t0));
        // Failing again waits twice as long.
        let t1 = t0 + Duration::from_secs(2);
        retry.failed("a", t1);
        assert!(retry.backing_off(&"a", t1 + Duration::from_secs(3)));
        assert!(!retry.backing_off(&"a", t1 + Duration::from_secs(4)));
        assert_eq!(retry.next_wait(t1), Some(Duration::from_secs(4)));
        assert!(retry.opened(&"a"));
        assert!(!retry.opened(&"a"));
        assert_eq!(retry.next_wait(t1), None);
    }

    #[test]
    fn clearing_starts_every_file_over() {
        let mut retry = Retry::new();
        let t0 = Instant::now();
        retry.failed(1, t0);
        retry.failed(1, t0);
        retry.clear();
        retry.failed(1, t0);
        assert_eq!(retry.next_wait(t0), Some(FIRST));
    }
}
