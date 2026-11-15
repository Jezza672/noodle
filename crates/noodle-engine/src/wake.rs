//! Waking the UI. Background work (a finished job, a plugin scan) calls
//! [`wake`] when it has news, so a window can sleep until then instead of
//! polling for it.
//!
//! A process can have more than one window: the desktop app has one, and a
//! host can load several Noodle plugins, each with an editor of its own.
//! Work isn't tied to a window, so every window [`register`]s a waker and a
//! wake reaches all of them. A window that had nothing to hear about draws
//! one spare frame, which is cheap and can't miss anything. With none
//! registered (the CLI, most tests) waking does nothing.

use std::sync::{Arc, Mutex};

/// What wakes a UI: in the app, a repaint request.
pub type Waker = Arc<dyn Fn() + Send + Sync>;

static WAKERS: Mutex<Vec<(u64, Waker)>> = Mutex::new(Vec::new());
static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

fn wakers() -> std::sync::MutexGuard<'static, Vec<(u64, Waker)>> {
    WAKERS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Keeps a waker registered until it is dropped.
#[must_use = "the waker is unregistered when this is dropped"]
pub struct Registration(u64);

impl Drop for Registration {
    fn drop(&mut self) {
        wakers().retain(|(id, _)| *id != self.0);
    }
}

/// Adds a waker. It is called, from any thread, by every [`wake`] until the
/// returned [`Registration`] is dropped.
pub fn register(waker: Waker) -> Registration {
    let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    wakers().push((id, waker));
    Registration(id)
}

/// A waker that wakes whichever UIs are registered when it is called, not
/// when it was made, to take along to a thread started now: a window that
/// opens while the work runs still hears of it.
pub fn current() -> Option<Waker> {
    Some(Arc::new(wake))
}

/// Wakes every registered UI. Not for the audio thread: it takes a lock.
pub fn wake() {
    // The wakers run outside the lock, so one may register or drop another.
    let all: Vec<Waker> = wakers().iter().map(|(_, w)| Arc::clone(w)).collect();
    for waker in all {
        waker();
    }
}
