//! A few threads that run every background job, in place of a thread (and a
//! timer) for each stream, track and device.
//!
//! A [`Task`] is a piece of work that is run when its [`Bell`] is rung, and
//! says what it wants next: to run again at once, to sleep until rung, to
//! sleep until rung or a time, or to finish. Ringing is real-time safe, so the
//! audio thread rings a task when it has made room for it, and an idle task
//! costs nothing: the pool's threads sleep on [`Doorbell`]s until there is
//! something to do.
//!
//! There are two pools. [`io`] runs tasks that never wait for another task,
//! like the ones that decode a file. [`control`] runs tasks that may, like the
//! stretcher, which blocks on its stream while rendering offline. Keeping
//! them apart means the second kind can't use up the threads the first needs.

use std::any::Any;
use std::panic::{self, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use noodle_engine::Doorbell;

/// What a task wants after a run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Next {
    /// Run again as soon as the other tasks have had a turn.
    Again,
    /// Nothing to do until rung.
    Idle,
    /// Nothing to do until rung, or until this long has passed.
    After(Duration),
    /// Finished: drop the task.
    Done,
}

/// A piece of background work.
pub trait Task: Send + 'static {
    fn run(&mut self) -> Next;
}

struct Slot {
    pending: AtomicBool,
    cancelled: AtomicBool,
    runs: AtomicU64,
    pool: Arc<Shared>,
}

/// Rings a task. Cheap to clone and real-time safe.
#[derive(Clone)]
pub struct Bell(Arc<Slot>);

impl Bell {
    /// Has the task run soon. Real-time safe.
    pub fn ring(&self) {
        self.0.pending.store(true, Ordering::SeqCst);
        self.0.pool.wake();
    }

    /// How many times the task has run.
    pub fn runs(&self) -> u64 {
        self.0.runs.load(Ordering::Relaxed)
    }

    /// True once the task's [`Handle`] has been dropped or closed.
    pub fn is_closed(&self) -> bool {
        self.0.cancelled.load(Ordering::Relaxed)
    }
}

/// Owns a task. Dropping it stops the task (on a pool thread, without
/// waiting); [`close`](Self::close) waits until it has stopped.
pub struct Handle {
    bell: Bell,
    id: u64,
}

impl Handle {
    pub fn bell(&self) -> Bell {
        self.bell.clone()
    }

    /// How many times the task has run. Tests use it to check that an idle
    /// task stays idle.
    pub fn runs(&self) -> u64 {
        self.bell.0.runs.load(Ordering::Relaxed)
    }

    /// Stops the task and waits until it has finished running and been
    /// dropped. Don't call it from a task of the same pool.
    pub fn close(self) {
        let slot = &self.bell.0;
        slot.cancelled.store(true, Ordering::SeqCst);
        let shared = &slot.pool;
        let mut tasks = shared.tasks.lock().expect("pool lock");
        loop {
            let Some(at) = tasks.iter().position(|e| e.id == self.id) else {
                return;
            };
            if tasks[at].running {
                tasks = shared.finished.wait(tasks).expect("pool lock");
                continue;
            }
            let entry = tasks.remove(at);
            drop(tasks);
            drop(entry);
            return;
        }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        // After `close` the flag is already set and the entry gone.
        if !self.bell.0.cancelled.swap(true, Ordering::SeqCst) {
            self.bell.0.pool.wake();
        }
    }
}

struct Entry {
    id: u64,
    slot: Arc<Slot>,
    /// None while a thread is running it.
    task: Option<Box<dyn Task>>,
    running: bool,
    due: Option<Instant>,
}

struct Shared {
    tasks: Mutex<Vec<Entry>>,
    /// Notified when a task finishes a run.
    finished: Condvar,
    /// Bumped by every ring, so a thread about to sleep can tell that work
    /// came in after it last looked.
    epoch: AtomicU64,
    bells: Vec<Doorbell>,
    next_id: AtomicU64,
}

impl Shared {
    fn wake(&self) {
        self.epoch.fetch_add(1, Ordering::SeqCst);
        for bell in &self.bells {
            if bell.wake_if_sleeping() {
                return;
            }
        }
    }
}

/// A set of threads running tasks.
pub struct Pool {
    shared: Arc<Shared>,
}

impl Pool {
    /// Starts `threads` threads, named `name`.
    fn new(name: &str, threads: usize) -> Self {
        let shared = Arc::new(Shared {
            tasks: Mutex::new(Vec::new()),
            finished: Condvar::new(),
            epoch: AtomicU64::new(0),
            bells: (0..threads).map(|_| Doorbell::new()).collect(),
            next_id: AtomicU64::new(0),
        });
        for index in 0..threads {
            let shared = Arc::clone(&shared);
            thread::Builder::new()
                .name(name.into())
                .spawn(move || worker(&shared, index))
                .expect("can't start a thread");
        }
        Self { shared }
    }

    /// Adds a task, to run once at first.
    pub fn spawn(&self, task: impl Task) -> Handle {
        self.spawn_with(|_| task)
    }

    /// Adds the task `make` builds, which is given its own bell first (for
    /// the objects it hands out to ring it).
    pub fn spawn_with<T: Task>(&self, make: impl FnOnce(Bell) -> T) -> Handle {
        let shared = &self.shared;
        let id = shared.next_id.fetch_add(1, Ordering::Relaxed);
        let slot = Arc::new(Slot {
            pending: AtomicBool::new(true),
            cancelled: AtomicBool::new(false),
            runs: AtomicU64::new(0),
            pool: Arc::clone(shared),
        });
        let task = make(Bell(Arc::clone(&slot)));
        shared.tasks.lock().expect("pool lock").push(Entry {
            id,
            slot: Arc::clone(&slot),
            task: Some(Box::new(task)),
            running: false,
            due: None,
        });
        shared.wake();
        Handle {
            bell: Bell(slot),
            id,
        }
    }

    /// How many threads the pool has.
    pub fn threads(&self) -> usize {
        self.shared.bells.len()
    }
}

fn thread_count() -> usize {
    let cores = thread::available_parallelism().map_or(1, usize::from);
    (2 + cores / 4).min(8)
}

/// The pool for tasks that never wait for another task.
pub fn io() -> &'static Pool {
    static POOL: OnceLock<Pool> = OnceLock::new();
    POOL.get_or_init(|| Pool::new("noodle-io", thread_count()))
}

/// The pool for tasks that may wait for tasks of [`io`].
pub fn control() -> &'static Pool {
    static POOL: OnceLock<Pool> = OnceLock::new();
    POOL.get_or_init(|| Pool::new("noodle-control", thread_count()))
}

/// The threads both pools have started, for tests.
pub fn thread_counts() -> (usize, usize) {
    (io().threads(), control().threads())
}

fn worker(shared: &Shared, index: usize) {
    loop {
        let seen = shared.epoch.load(Ordering::SeqCst);
        let mut next_due: Option<Instant> = None;
        loop {
            let claimed = claim(shared, &mut next_due);
            let Some((id, slot, mut task)) = claimed else {
                break;
            };
            slot.runs.fetch_add(1, Ordering::Relaxed);
            let result = panic::catch_unwind(AssertUnwindSafe(|| task.run()));
            let next = result.unwrap_or_else(|payload: Box<dyn Any + Send>| {
                drop(payload);
                Next::Done
            });
            let keep = next != Next::Done && !slot.cancelled.load(Ordering::SeqCst);
            if !keep {
                // Drop the task while its entry still says it is running, so
                // `Handle::close` can't see the entry gone before this has.
                drop(task);
                let mut tasks = shared.tasks.lock().expect("pool lock");
                let at = tasks.iter().position(|e| e.id == id).expect("running");
                let entry = tasks.remove(at);
                drop(tasks);
                drop(entry);
            } else {
                let mut tasks = shared.tasks.lock().expect("pool lock");
                let at = tasks.iter().position(|e| e.id == id).expect("running");
                let entry = &mut tasks[at];
                entry.running = false;
                entry.task = Some(task);
                match next {
                    Next::Again => slot.pending.store(true, Ordering::SeqCst),
                    Next::After(d) => entry.due = Some(Instant::now() + d),
                    _ => entry.due = None,
                }
            }
            shared.finished.notify_all();
        }
        let timeout = next_due.map(|d| d.saturating_duration_since(Instant::now()));
        shared.bells[index].wait_unless(timeout, || shared.epoch.load(Ordering::SeqCst) != seen);
    }
}

type Claimed = (u64, Arc<Slot>, Box<dyn Task>);

/// Takes a task that is ready to run, and drops any that were cancelled.
/// `next_due` collects the soonest time a sleeping task wants to run.
fn claim(shared: &Shared, next_due: &mut Option<Instant>) -> Option<Claimed> {
    let mut cancelled = Vec::new();
    let mut tasks = shared.tasks.lock().expect("pool lock");
    let now = Instant::now();
    let mut claimed = None;
    for entry in tasks.iter_mut() {
        if entry.running {
            continue;
        }
        if entry.slot.cancelled.load(Ordering::SeqCst) {
            // Dropped below, outside the lock, with the entry marked running
            // until then: `Handle::close` waits for it.
            entry.running = true;
            cancelled.push((entry.id, entry.task.take().expect("not running")));
            continue;
        }
        let due = entry.due.is_some_and(|d| d <= now);
        if claimed.is_none() && (entry.slot.pending.swap(false, Ordering::SeqCst) || due) {
            entry.running = true;
            entry.due = None;
            let task = entry.task.take().expect("not running");
            claimed = Some((entry.id, Arc::clone(&entry.slot), task));
        } else if let Some(d) = entry.due {
            *next_due = Some(next_due.map_or(d, |n| n.min(d)));
        }
    }
    drop(tasks);
    if !cancelled.is_empty() {
        let ids: Vec<u64> = cancelled.iter().map(|(id, _)| *id).collect();
        drop(cancelled);
        shared
            .tasks
            .lock()
            .expect("pool lock")
            .retain(|e| !ids.contains(&e.id));
        shared.finished.notify_all();
    }
    claimed
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    struct Count(Arc<AtomicUsize>, Next);

    impl Task for Count {
        fn run(&mut self) -> Next {
            self.0.fetch_add(1, Ordering::SeqCst);
            self.1
        }
    }

    fn wait_for(what: impl Fn() -> bool) {
        let started = Instant::now();
        while !what() {
            assert!(started.elapsed() < Duration::from_secs(5), "timed out");
            thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn an_idle_task_runs_only_when_rung() {
        let count = Arc::new(AtomicUsize::new(0));
        let handle = io().spawn(Count(count.clone(), Next::Idle));
        wait_for(|| count.load(Ordering::SeqCst) == 1);
        thread::sleep(Duration::from_millis(100));
        assert_eq!(count.load(Ordering::SeqCst), 1);
        handle.bell().ring();
        wait_for(|| count.load(Ordering::SeqCst) == 2);
        assert_eq!(handle.runs(), 2);
    }

    #[test]
    fn a_task_can_ask_for_a_later_run() {
        let count = Arc::new(AtomicUsize::new(0));
        let _handle = io().spawn(Count(count.clone(), Next::After(Duration::from_millis(5))));
        wait_for(|| count.load(Ordering::SeqCst) >= 3);
    }

    #[test]
    fn a_closed_task_is_dropped_before_close_returns() {
        struct Flag(Arc<AtomicBool>);
        impl Task for Flag {
            fn run(&mut self) -> Next {
                Next::Idle
            }
        }
        impl Drop for Flag {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        let dropped = Arc::new(AtomicBool::new(false));
        let handle = io().spawn(Flag(dropped.clone()));
        handle.close();
        assert!(dropped.load(Ordering::SeqCst));
    }

    #[test]
    fn a_task_closed_while_the_pool_is_busy_is_dropped_before_close_returns() {
        // A worker that finds a cancelled task drops it itself; `close` must
        // wait for that rather than see the entry gone and return.
        struct SlowDrop(Arc<AtomicBool>);
        impl Task for SlowDrop {
            fn run(&mut self) -> Next {
                Next::Idle
            }
        }
        impl Drop for SlowDrop {
            fn drop(&mut self) {
                thread::sleep(Duration::from_millis(2));
                self.0.store(true, Ordering::SeqCst);
            }
        }
        let busy = Arc::new(AtomicBool::new(true));
        let count = Arc::new(AtomicUsize::new(0));
        let ringer = io().spawn(Count(count.clone(), Next::Idle));
        let bell = ringer.bell();
        let keep_ringing = {
            let busy = busy.clone();
            thread::spawn(move || {
                while busy.load(Ordering::SeqCst) {
                    bell.ring();
                    thread::yield_now();
                }
            })
        };
        for _ in 0..200 {
            let dropped = Arc::new(AtomicBool::new(false));
            let handle = io().spawn(SlowDrop(dropped.clone()));
            handle.close();
            assert!(dropped.load(Ordering::SeqCst));
        }
        busy.store(false, Ordering::SeqCst);
        keep_ringing.join().unwrap();
    }

    #[test]
    fn a_panicking_task_doesnt_take_a_thread_with_it() {
        struct Boom;
        impl Task for Boom {
            fn run(&mut self) -> Next {
                panic!("expected by the test");
            }
        }
        let _boom = io().spawn(Boom);
        let count = Arc::new(AtomicUsize::new(0));
        let _handle = io().spawn(Count(count.clone(), Next::Idle));
        wait_for(|| count.load(Ordering::SeqCst) == 1);
    }
}
