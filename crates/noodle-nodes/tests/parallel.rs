//! Running a plan on worker threads: the same sound as one thread, through
//! edits that swap plans, and still no allocation on any thread while a
//! block renders.
//!
//! One test function, because it counts allocations on every thread.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use noodle_core::{Command, Config, Connection, Endpoint, Node, NodeId, Project, Value};
use noodle_engine::{Controller, OUTPUT_ID, Processor, Registry, Settings, engine_with_workers};

static WATCH: AtomicBool = AtomicBool::new(false);
static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

struct Counting;

fn note() {
    if WATCH.load(Ordering::Relaxed) {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
    }
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        note();
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        note();
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        note();
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

const SETTINGS: Settings = Settings {
    sample_rate: 48_000.0,
    max_frames: 128,
    channels: 2,
};

const CHAINS: usize = 8;

struct Rig {
    project: Project,
    registry: Registry,
    controller: Controller,
    processor: Processor,
    mix: NodeId,
}

impl Rig {
    fn new(workers: usize) -> Self {
        let mut registry = Registry::with_builtins();
        let _hub = noodle_nodes::register_all(&mut registry);
        let (controller, processor) = engine_with_workers(SETTINGS, workers, true).unwrap();
        let mut rig = Self {
            project: Project::new(),
            registry,
            controller,
            processor,
            mix: NodeId(0),
        };
        rig.mix = rig.add(
            Node::new("noodle.util.mix")
                .with_config(Config::new().with("inputs", Value::Int(CHAINS as i64))),
        );
        let output = rig.add(Node::new(OUTPUT_ID));
        rig.wire(rig.mix, "out", output, "in");
        for i in 0..CHAINS {
            rig.chain(i, 110.0 * (i + 1) as f32);
        }
        rig.update();
        rig
    }

    fn add(&mut self, node: Node) -> NodeId {
        let id = self.project.new_node_id();
        Command::AddNode { id, node }
            .apply(&mut self.project)
            .unwrap();
        id
    }

    fn wire(&mut self, from: NodeId, from_port: &str, to: NodeId, to_port: &str) {
        Command::Connect(Connection {
            from: Endpoint::new(from, from_port),
            to: Endpoint::new(to, to_port),
        })
        .apply(&mut self.project)
        .unwrap();
    }

    /// sine → filter → gain → mix input `i`.
    fn chain(&mut self, i: usize, hz: f32) {
        let sine = self.add(Node::new("noodle.osc.saw").with_param("frequency", hz));
        let svf = self.add(Node::new("noodle.filter.svf").with_param("cutoff", 800.0));
        let gain = self.add(Node::new("noodle.util.gain").with_param("gain", -12.0));
        self.wire(sine, "out", svf, "in");
        self.wire(svf, "low", gain, "in");
        self.wire(gain, "out", self.mix, &format!("in{}", i + 1));
    }

    fn update(&mut self) {
        let diagnostics = self
            .controller
            .update_project(&self.project, &self.registry);
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
    }

    fn render(&mut self, frames: usize) -> Vec<f32> {
        let mut out = vec![0.0; frames * SETTINGS.channels];
        self.processor.process(&mut out);
        out
    }
}

/// Renders the same edits on a rig with `workers` threads.
fn session(workers: usize) -> Vec<f32> {
    let mut rig = Rig::new(workers);
    let mut all = Vec::new();
    for round in 0..6 {
        all.extend(rig.render(1000));
        match round {
            // A new chain: a different plan, with the rest carried over.
            1 => {
                let extra = rig.add(Node::new("noodle.osc.sine").with_param("frequency", 90.0));
                let gain = rig.add(Node::new("noodle.util.gain"));
                rig.wire(extra, "out", gain, "in");
                rig.update();
            }
            // A knob.
            3 => {
                let mix = rig.mix;
                rig.controller.set_param(mix, "gain3", -20.0);
                rig.update();
            }
            _ => {}
        }
        rig.controller.maintain();
    }
    all
}

#[test]
fn workers_sound_like_one_thread_and_never_allocate() {
    let serial = session(0);
    assert!(
        serial.iter().any(|&x| x.abs() > 0.05),
        "the rig makes sound"
    );
    for workers in [1, 3] {
        let parallel = session(workers);
        assert_eq!(serial.len(), parallel.len());
        // One Output node, so the sums are grouped alike: identical.
        assert!(
            serial == parallel,
            "{workers} workers differ from one thread"
        );
    }

    // Without workers the compiler reuses buffers freely, which makes the
    // branches wait for each other; with them it keeps the branches apart.
    let narrow = Rig::new(0).controller.plan_shape();
    assert!(narrow.width < CHAINS, "{narrow:?}");

    let mut rig = Rig::new(3);
    assert_eq!(rig.processor.workers(), 3);
    // Real-time priority is the system's to give (a CI runner usually
    // refuses); whatever the workers got, they don't claim more than there
    // are.
    let (realtime, raised) = rig.processor.worker_priorities();
    assert!(realtime + raised <= 3);
    let shape = rig.controller.plan_shape();
    assert!(shape.width >= CHAINS, "{shape:?}");
    assert!(shape.critical_path < shape.steps, "{shape:?}");
    rig.render(1000);
    let mut out = vec![0.0; 1000 * SETTINGS.channels];
    WATCH.store(true, Ordering::SeqCst);
    for _ in 0..50 {
        rig.processor.process(&mut out);
    }
    WATCH.store(false, Ordering::SeqCst);
    assert_eq!(
        ALLOCATIONS.load(Ordering::SeqCst),
        0,
        "allocated while rendering on workers"
    );
}
