//! Waking the UI. Background work (a finished job, a plugin scan) calls
//! [`wake`] when it has news, so the window can sleep until then instead of
//! polling for it. The app registers the waker once at start; with none
//! registered (the CLI, most tests) waking does nothing.

use std::sync::{Arc, Mutex};

/// What wakes the UI: in the app, a repaint request.
pub type Waker = Arc<dyn Fn() + Send + Sync>;

static WAKER: Mutex<Option<Waker>> = Mutex::new(None);

/// Registers the waker, replacing any earlier one.
pub fn set_waker(waker: Option<Waker>) {
    *WAKER.lock().unwrap_or_else(|e| e.into_inner()) = waker;
}

/// The registered waker, to take along to a thread started now.
pub fn current() -> Option<Waker> {
    WAKER.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Wakes the UI, if there is one. Not for the audio thread: it takes a lock.
pub fn wake() {
    if let Some(waker) = current() {
        waker();
    }
}
