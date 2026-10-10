//! Background jobs: work that runs on its own thread with progress and
//! cancellation, polled from the UI. The offline renderer is one; freezes and
//! exports are others.

use std::sync::Arc;
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::thread;

use crate::Progress;

/// The job's thread panicked, so there is no result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JobPanicked;

impl std::fmt::Display for JobPanicked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the background job crashed")
    }
}

impl std::error::Error for JobPanicked {}

/// A running background job producing a `T`.
///
/// Dropping the handle cancels the job (it stops the next time it reports
/// progress) and lets its thread finish on its own, so dropping never blocks.
pub struct Job<T> {
    progress: Arc<Progress>,
    receiver: Receiver<T>,
    finished: Option<Result<T, JobPanicked>>,
    /// The result has been handed out.
    taken: bool,
}

impl<T: Send + 'static> Job<T> {
    /// Runs `work` on a new thread. It should call [`Progress::report`]
    /// regularly and stop once that returns `Err`.
    pub fn spawn(work: impl FnOnce(&Progress) -> T + Send + 'static) -> Self {
        let progress = Arc::new(Progress::new());
        let (sender, receiver) = channel();
        let theirs = Arc::clone(&progress);
        thread::Builder::new()
            .name("noodle-job".into())
            .spawn(move || {
                // The receiver may be gone (job dropped); that's fine.
                let _ = sender.send(work(&theirs));
            })
            .expect("can't start a thread");
        Self {
            progress,
            receiver,
            finished: None,
            taken: false,
        }
    }
}

impl<T> Job<T> {
    /// How far the job has got, from 0 to 1.
    pub fn fraction(&self) -> f32 {
        self.progress.fraction()
    }

    /// Asks the job to stop. It still produces a result (typically an error
    /// saying it was cancelled).
    pub fn cancel(&self) {
        self.progress.cancel();
    }

    /// Whether [`poll`](Self::poll) would return a result.
    pub fn is_finished(&mut self) -> bool {
        self.fill();
        self.finished.is_some() || self.taken
    }

    /// The result if the job has finished, else `None`. Never blocks. Once it
    /// has returned a result, later calls return `None`.
    pub fn poll(&mut self) -> Option<Result<T, JobPanicked>> {
        self.fill();
        let done = self.finished.take();
        self.taken |= done.is_some();
        done
    }

    /// Waits for the result. Don't call it after [`poll`](Self::poll) has
    /// already returned the result: that is gone, and this reports
    /// [`JobPanicked`].
    pub fn wait(mut self) -> Result<T, JobPanicked> {
        if let Some(done) = self.finished.take() {
            return done;
        }
        self.receiver.recv().map_err(|_| JobPanicked)
    }

    fn fill(&mut self) {
        if self.finished.is_none() && !self.taken {
            match self.receiver.try_recv() {
                Ok(value) => self.finished = Some(Ok(value)),
                Err(TryRecvError::Disconnected) => self.finished = Some(Err(JobPanicked)),
                Err(TryRecvError::Empty) => {}
            }
        }
    }
}

impl<T> Drop for Job<T> {
    fn drop(&mut self) {
        self.progress.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Cancelled;
    use std::time::{Duration, Instant};

    fn wait_for<T>(job: &mut Job<T>) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !job.is_finished() {
            assert!(Instant::now() < deadline, "job never finished");
            thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn delivers_the_result() {
        let mut job = Job::spawn(|progress| {
            progress.report(1.0).unwrap();
            7
        });
        wait_for(&mut job);
        assert_eq!(job.fraction(), 1.0);
        assert_eq!(job.poll(), Some(Ok(7)));
        assert_eq!(job.poll(), None);
    }

    #[test]
    fn cancel_stops_the_work() {
        let job = Job::spawn(|progress| -> Result<(), Cancelled> {
            loop {
                if progress.report(0.5).is_err() {
                    return Err(Cancelled);
                }
                thread::sleep(Duration::from_millis(1));
            }
        });
        job.cancel();
        assert_eq!(job.wait(), Ok(Err(Cancelled)));
    }

    #[test]
    fn a_panic_is_reported() {
        let job = Job::spawn(|_| -> u32 { panic!("boom") });
        assert_eq!(job.wait(), Err(JobPanicked));
    }
}
