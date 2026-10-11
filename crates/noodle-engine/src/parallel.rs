//! Running a plan's steps on several threads.
//!
//! A plan's steps are in a valid order for one thread, but many of them are
//! independent: two oscillators, a plugin and a filter chain. [`StepGraph`]
//! records what each step must wait for (the compiler reuses buffers, so that
//! covers a step waiting for the readers of the buffer it overwrites as well
//! as for its writers), and [`Workers`] is a small pool of threads that the
//! audio thread joins to run whatever is ready.
//!
//! Nothing here allocates, locks or blocks in the kernel while a block runs.
//! Workers spin briefly after a block and then park, so an idle engine costs
//! nothing and a busy one wakes them with one `unpark` each.

use std::cell::UnsafeCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::thread::{self, JoinHandle, Thread};
use std::time::{Duration, Instant};

/// How long an idle worker spins before parking. Short: a parked worker is
/// woken in tens of microseconds, and spinning on a core the audio thread
/// might want is worse than that.
const SPIN: Duration = Duration::from_micros(40);

/// A step that a thread has taken.
const CLAIMED: u32 = u32::MAX;

/// A plan's steps and what each waits for.
pub(crate) struct StepGraph {
    /// Steps each step waits for, at the start of a block.
    initial: Box<[u32]>,
    /// Steps still being waited for in this block, or [`CLAIMED`].
    pending: Box<[AtomicU32]>,
    /// Every step before this one has been claimed.
    first: AtomicUsize,
    /// Steps finished in this block.
    done: AtomicUsize,
    /// A node panicked in this block.
    poisoned: AtomicBool,
    /// The steps that wait for step `i` are `dependents[start[i]..start[i + 1]]`.
    start: Box<[u32]>,
    dependents: Box<[u32]>,
    /// The longest chain of steps that must run one after the other.
    critical_path: usize,
    /// The most steps that can run at the same time.
    width: usize,
}

impl StepGraph {
    /// `waits[i]` lists the earlier steps step `i` must wait for.
    pub(crate) fn new(waits: &[Vec<usize>]) -> Self {
        let n = waits.len();
        let mut counts = vec![0u32; n];
        for list in waits {
            for &step in list {
                counts[step] += 1;
            }
        }
        let mut start = Vec::with_capacity(n + 1);
        let mut total = 0;
        for &count in &counts {
            start.push(total);
            total += count;
        }
        start.push(total);
        let mut fill = start.clone();
        let mut dependents = vec![0u32; total as usize];
        for (step, list) in waits.iter().enumerate() {
            for &before in list {
                dependents[fill[before] as usize] = step as u32;
                fill[before] += 1;
            }
        }
        // Levels: the earliest round each step could run in.
        let mut level = vec![0usize; n];
        for (step, list) in waits.iter().enumerate() {
            level[step] = list.iter().map(|&b| level[b] + 1).max().unwrap_or(0);
        }
        let critical_path = level.iter().max().map_or(0, |&l| l + 1);
        let mut per_level = vec![0usize; critical_path];
        for &l in &level {
            per_level[l] += 1;
        }
        let width = per_level.iter().copied().max().unwrap_or(0);
        Self {
            initial: waits.iter().map(|l| l.len() as u32).collect(),
            pending: waits
                .iter()
                .map(|l| AtomicU32::new(l.len() as u32))
                .collect(),
            first: AtomicUsize::new(0),
            done: AtomicUsize::new(0),
            poisoned: AtomicBool::new(false),
            start: start.into_boxed_slice(),
            dependents: dependents.into_boxed_slice(),
            critical_path,
            width,
        }
    }

    pub(crate) fn steps(&self) -> usize {
        self.initial.len()
    }

    /// The longest chain of steps that can't overlap.
    pub(crate) fn critical_path(&self) -> usize {
        self.critical_path
    }

    /// The most steps that could run at once.
    pub(crate) fn width(&self) -> usize {
        self.width
    }

    /// Whether any step can overlap another.
    pub(crate) fn has_parallelism(&self) -> bool {
        self.width > 1
    }

    /// Gets ready for a block. The audio thread calls it before it opens one.
    pub(crate) fn reset(&self) {
        for (cell, &count) in self.pending.iter().zip(&self.initial) {
            cell.store(count, Ordering::Relaxed);
        }
        self.first.store(0, Ordering::Relaxed);
        self.done.store(0, Ordering::Relaxed);
        self.poisoned.store(false, Ordering::Relaxed);
    }

    /// Whether a node panicked in the last block.
    #[cfg(test)]
    pub(crate) fn poisoned(&self) -> bool {
        self.poisoned.load(Ordering::Relaxed)
    }

    /// Runs steps through `run` until all of them are done. Any number of
    /// threads can do this at once; each step runs on exactly one.
    pub(crate) fn work(&self, mut run: impl FnMut(usize)) {
        let n = self.pending.len();
        loop {
            let first = self.first.load(Ordering::Relaxed);
            let mut advance = first;
            let mut claimed = None;
            let mut prefix = true;
            for step in first..n {
                let state = self.pending[step].load(Ordering::Acquire);
                if state == CLAIMED {
                    if prefix {
                        advance = step + 1;
                    }
                    continue;
                }
                prefix = false;
                if state == 0
                    && self.pending[step]
                        .compare_exchange(0, CLAIMED, Ordering::AcqRel, Ordering::Relaxed)
                        .is_ok()
                {
                    claimed = Some(step);
                    break;
                }
            }
            self.first.fetch_max(advance, Ordering::Relaxed);
            match claimed {
                Some(step) => {
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        run(step);
                    }));
                    if result.is_err() {
                        self.poisoned.store(true, Ordering::Relaxed);
                    }
                    let (from, to) = (self.start[step] as usize, self.start[step + 1] as usize);
                    for &next in &self.dependents[from..to] {
                        self.pending[next as usize].fetch_sub(1, Ordering::AcqRel);
                    }
                    self.done.fetch_add(1, Ordering::Release);
                }
                None => {
                    if self.done.load(Ordering::Acquire) == n {
                        return;
                    }
                    std::hint::spin_loop();
                }
            }
        }
    }
}

/// A block's work: a function and what to call it with.
#[derive(Clone, Copy)]
struct Job {
    run: unsafe fn(*const ()),
    data: *const (),
}

struct Shared {
    /// Bumped to start a block.
    epoch: AtomicU64,
    /// A block is running, so `job` is valid.
    open: AtomicBool,
    /// Workers that may be reading `job`.
    inflight: AtomicUsize,
    shutdown: AtomicBool,
    /// Whether each worker is (about to be) parked.
    parked: Box<[AtomicBool]>,
    job: UnsafeCell<Option<Job>>,
}

// SAFETY: `job` is written only by the audio thread, while no worker is
// inside a block (`inflight` is zero and `open` is false), and read only by
// workers that have counted themselves in and seen `open`.
unsafe impl Sync for Shared {}
unsafe impl Send for Shared {}

/// Threads that help the audio thread run a plan.
pub(crate) struct Workers {
    shared: Arc<Shared>,
    threads: Vec<JoinHandle<()>>,
    handles: Vec<Thread>,
    always: bool,
}

/// Plans with fewer steps than this aren't worth sharing out.
const MIN_STEPS: usize = 4;

impl Workers {
    /// Starts `count` threads. With `always`, every plan with independent
    /// steps runs in parallel; otherwise only those wide enough to pay for
    /// the hand-off.
    pub(crate) fn start(count: usize, always: bool) -> Self {
        let shared = Arc::new(Shared {
            epoch: AtomicU64::new(0),
            open: AtomicBool::new(false),
            inflight: AtomicUsize::new(0),
            shutdown: AtomicBool::new(false),
            parked: (0..count).map(|_| AtomicBool::new(false)).collect(),
            job: UnsafeCell::new(None),
        });
        let threads: Vec<_> = (0..count)
            .map(|index| {
                let shared = shared.clone();
                thread::Builder::new()
                    .name(format!("noodle-worker-{index}"))
                    .spawn(move || worker(&shared, index))
                    .expect("spawning a worker thread")
            })
            .collect();
        let handles = threads.iter().map(|t| t.thread().clone()).collect();
        Self {
            shared,
            threads,
            handles,
            always,
        }
    }

    /// How many threads help the audio thread.
    pub(crate) fn count(&self) -> usize {
        self.threads.len()
    }

    /// Whether sharing this plan out beats running it on one thread.
    pub(crate) fn worth(&self, graph: &StepGraph) -> bool {
        graph.has_parallelism()
            && (self.always
                || (graph.steps() >= MIN_STEPS
                    && graph.steps() as f32 >= 1.5 * graph.critical_path() as f32))
    }

    /// Calls `run(data)` on this thread and every worker, until it returns
    /// on all of them. `run` must return only when the block's work is done
    /// (see [`StepGraph::work`]).
    ///
    /// SAFETY: `run` must be safe to call concurrently with `data`.
    pub(crate) unsafe fn run(&self, run: unsafe fn(*const ()), data: *const ()) {
        let shared = &*self.shared;
        // SAFETY: no worker is inside a block, so none reads the job.
        unsafe { *shared.job.get() = Some(Job { run, data }) };
        shared.open.store(true, Ordering::SeqCst);
        shared.epoch.fetch_add(1, Ordering::SeqCst);
        for (parked, handle) in shared.parked.iter().zip(&self.handles) {
            if parked.load(Ordering::SeqCst) {
                handle.unpark();
            }
        }
        // SAFETY: as promised by the caller.
        unsafe { run(data) };
        shared.open.store(false, Ordering::SeqCst);
        // `data` lives on the caller's stack: wait for stragglers to leave.
        while shared.inflight.load(Ordering::SeqCst) != 0 {
            std::hint::spin_loop();
        }
    }
}

impl Drop for Workers {
    fn drop(&mut self) {
        self.shared.shutdown.store(true, Ordering::SeqCst);
        self.shared.epoch.fetch_add(1, Ordering::SeqCst);
        for handle in &self.handles {
            handle.unpark();
        }
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

fn worker(shared: &Shared, index: usize) {
    let mut seen = 0;
    loop {
        // Wait for the next block: spin a little, then park.
        let since = Instant::now();
        loop {
            if shared.shutdown.load(Ordering::SeqCst) {
                return;
            }
            let epoch = shared.epoch.load(Ordering::SeqCst);
            if epoch != seen {
                seen = epoch;
                break;
            }
            if since.elapsed() < SPIN {
                std::hint::spin_loop();
                continue;
            }
            shared.parked[index].store(true, Ordering::SeqCst);
            if shared.epoch.load(Ordering::SeqCst) == seen
                && !shared.shutdown.load(Ordering::SeqCst)
            {
                thread::park();
            }
            shared.parked[index].store(false, Ordering::SeqCst);
        }
        shared.inflight.fetch_add(1, Ordering::SeqCst);
        if shared.open.load(Ordering::SeqCst) {
            // SAFETY: counted in and `open`, so the job is valid until we
            // count out (see `Shared`).
            if let Some(job) = unsafe { *shared.job.get() } {
                unsafe { (job.run)(job.data) };
            }
        }
        shared.inflight.fetch_sub(1, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    /// A diamond: 0 → {1, 2} → 3.
    fn diamond() -> StepGraph {
        StepGraph::new(&[vec![], vec![0], vec![0], vec![1, 2]])
    }

    #[test]
    fn measures_the_shape_of_a_graph() {
        let graph = diamond();
        assert_eq!(graph.steps(), 4);
        assert_eq!(graph.critical_path(), 3);
        assert_eq!(graph.width(), 2);
        assert!(graph.has_parallelism());
        let chain = StepGraph::new(&[vec![], vec![0], vec![1]]);
        assert!(!chain.has_parallelism());
    }

    #[test]
    fn one_thread_runs_every_step_in_a_valid_order() {
        let graph = diamond();
        graph.reset();
        let order = std::cell::RefCell::new(Vec::new());
        graph.work(|step| order.borrow_mut().push(step));
        let order = order.into_inner();
        assert_eq!(order.len(), 4);
        assert_eq!(order[0], 0);
        assert_eq!(order[3], 3);
    }

    #[test]
    fn many_threads_run_each_step_once_after_its_waits() {
        // A wide layer between a source and a sink, repeated, with each step
        // checking that everything it waits for is done.
        let layers = 6;
        let width = 8;
        let mut waits = vec![vec![]];
        let mut previous = vec![0];
        for _ in 0..layers {
            let mut layer = Vec::new();
            for _ in 0..width {
                layer.push(waits.len());
                waits.push(previous.clone());
            }
            let join = waits.len();
            waits.push(layer.clone());
            previous = vec![join];
        }
        let graph = StepGraph::new(&waits);
        let workers = Workers::start(3, true);
        let ran: Vec<AtomicUsize> = waits.iter().map(|_| AtomicUsize::new(0)).collect();
        struct Ctx<'a> {
            graph: &'a StepGraph,
            waits: &'a [Vec<usize>],
            ran: &'a [AtomicUsize],
        }
        unsafe fn work(data: *const ()) {
            let ctx = unsafe { &*data.cast::<Ctx>() };
            ctx.graph.work(|step| {
                for &before in &ctx.waits[step] {
                    assert_eq!(ctx.ran[before].load(Ordering::Acquire), 1);
                }
                ctx.ran[step].fetch_add(1, Ordering::Release);
            });
        }
        for _ in 0..200 {
            for cell in &ran {
                cell.store(0, Ordering::Relaxed);
            }
            graph.reset();
            let ctx = Ctx {
                graph: &graph,
                waits: &waits,
                ran: &ran,
            };
            unsafe { workers.run(work, (&ctx as *const Ctx).cast()) };
            assert!(ran.iter().all(|c| c.load(Ordering::Relaxed) == 1));
            assert!(!graph.poisoned());
        }
    }

    #[test]
    fn a_panicking_step_does_not_hang_the_block() {
        let graph = diamond();
        graph.reset();
        graph.work(|step| {
            if step == 1 {
                panic!("node bug");
            }
        });
        assert!(graph.poisoned());
    }
}
