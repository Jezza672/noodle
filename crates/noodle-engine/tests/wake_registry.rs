//! More than one window can be woken, and each stops when it is dropped. Its
//! own test binary, because wakers are process-wide.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use noodle_engine::wake;

fn counting() -> (Arc<AtomicUsize>, wake::Registration) {
    let count = Arc::new(AtomicUsize::new(0));
    let theirs = Arc::clone(&count);
    let registration = wake::register(Arc::new(move || {
        theirs.fetch_add(1, Ordering::SeqCst);
    }));
    (count, registration)
}

#[test]
fn every_registered_window_hears_a_wake_until_it_is_dropped() {
    let (first, first_reg) = counting();
    let (second, second_reg) = counting();
    wake::wake();
    assert_eq!(first.load(Ordering::SeqCst), 1);
    assert_eq!(second.load(Ordering::SeqCst), 1);

    drop(first_reg);
    wake::wake();
    assert_eq!(
        first.load(Ordering::SeqCst),
        1,
        "a dropped waker was called"
    );
    assert_eq!(second.load(Ordering::SeqCst), 2);

    // A waker taken before a window opened still reaches it.
    let early = wake::current().unwrap();
    let (late, _late_reg) = counting();
    early();
    assert_eq!(late.load(Ordering::SeqCst), 1);
    drop(second_reg);
}
