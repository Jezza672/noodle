//! Offline nodes: processing that needs the whole input before it can produce
//! output.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use crate::{Context, Io};

/// An instance of an offline node (see [`Mode::Offline`](crate::Mode)).
///
/// `render` gets the whole range at once through the same [`Io`] views
/// real-time nodes get; `ctx.frames` is the length of the range. It runs on a
/// worker thread, so it may allocate and take its time, but it should call
/// [`Progress::report`] regularly and stop if that returns `Err`.
pub trait OfflineNode: Send + 'static {
    fn render(
        &mut self,
        ctx: &Context,
        io: Io<'_, '_>,
        progress: &Progress,
    ) -> Result<(), Cancelled>;
}

/// Shared between a render and whatever is showing its progress.
#[derive(Debug, Default)]
pub struct Progress {
    fraction: AtomicU32,
    cancelled: AtomicBool,
}

impl Progress {
    pub fn new() -> Self {
        Self::default()
    }

    /// Records how far the render has got, from 0 to 1. Returns
    /// `Err(Cancelled)` if the render should stop.
    pub fn report(&self, fraction: f32) -> Result<(), Cancelled> {
        self.fraction
            .store(fraction.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
        if self.cancelled.load(Ordering::Relaxed) {
            Err(Cancelled)
        } else {
            Ok(())
        }
    }

    pub fn fraction(&self) -> f32 {
        f32::from_bits(self.fraction.load(Ordering::Relaxed))
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cancelled;
