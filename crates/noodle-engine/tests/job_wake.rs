//! A finished job wakes the UI. Its own test binary, because the waker is
//! process-wide.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use noodle_engine::{Job, wake};

fn wait_for(count: &AtomicUsize, at_least: usize) -> bool {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(5) {
        if count.load(Ordering::SeqCst) >= at_least {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    false
}

#[test]
fn a_finished_job_wakes_the_ui() {
    let wakes = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&wakes);
    wake::set_waker(Some(Arc::new(move || {
        counter.fetch_add(1, Ordering::SeqCst);
    })));

    let mut job = Job::spawn(|_| 7);
    assert!(wait_for(&wakes, 1), "no wake after the job finished");
    assert_eq!(job.poll().unwrap().unwrap(), 7);

    let _ = Job::spawn(|_| -> u32 { panic!("boom") });
    assert!(wait_for(&wakes, 2), "no wake after the job crashed");
}
