//! Silence skipping end to end: voices that aren't sounding cost nothing,
//! lane by lane, and nothing is skipped while it still sounds.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use noodle_core::{Command, Config, Connection, Endpoint, Node, NodeId, Project, Value};
use noodle_engine::{
    Context, Instance, Lane, LaneKernel, NodeError, NodeInfo, NodeType, OUTPUT_ID, PerLane,
    Processor, Registry, Settings, Setup, Skip, engine,
};
use noodle_io::MidiBus;
use noodle_nodes::{MIDI_IN_ID, VOICE_MIX_ID, VOICES_ID, register_library};

struct Guarded;

thread_local! {
    static REALTIME: Cell<bool> = const { Cell::new(false) };
    static VIOLATIONS: Cell<usize> = const { Cell::new(0) };
}

fn note() {
    let _ = REALTIME.try_with(|realtime| {
        if realtime.get() {
            VIOLATIONS.with(|v| v.set(v.get() + 1));
        }
    });
}

unsafe impl GlobalAlloc for Guarded {
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
static ALLOCATOR: Guarded = Guarded;

fn realtime(f: impl FnOnce()) -> usize {
    REALTIME.set(true);
    f();
    REALTIME.set(false);
    VIOLATIONS.replace(0)
}

const SETTINGS: Settings = Settings {
    sample_rate: 48_000.0,
    max_frames: 256,
    channels: 1,
};

/// A pass-through that counts the lanes it actually processes, and opts in to
/// skipping like any effect would.
struct Counter(Arc<AtomicUsize>);

static COUNTER_INFO: NodeInfo = NodeInfo {
    id: "test.counter",
    version: 1,
    name: "Counter",
    category: "Test",
};

impl NodeType for Counter {
    fn info(&self) -> &NodeInfo {
        &COUNTER_INFO
    }

    fn layout(&self, _config: &Config) -> Result<noodle_engine::Layout, NodeError> {
        Ok(noodle_engine::Layout::realtime()
            .input("in", "In")
            .output("out", "Out"))
    }

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        Ok(Instance::realtime(PerLane::new(
            CountKernel(Arc::clone(&self.0)),
            setup,
        )))
    }
}

struct CountKernel(Arc<AtomicUsize>);

impl LaneKernel for CountKernel {
    type State = ();

    fn skip(&self) -> Skip {
        Skip::AnySilent(&[0])
    }

    fn process_lane(&mut self, _: &mut (), _: &Context, mut lane: Lane<'_, '_>) {
        self.0.fetch_add(1, Ordering::Relaxed);
        let input = lane.inputs.get(0);
        lane.outputs.get_mut(0).copy_from_slice(input);
    }
}

/// MIDI In → Voices (4, tail `tail`) → sine → VCA (enveloped by an ADSR with
/// a 10 ms release) → Counter → Voice Mix → Output.
fn synth(tail: f32) -> (Processor, MidiBus, Arc<AtomicUsize>, impl Sized) {
    let mut registry = Registry::with_builtins();
    let library = register_library(&mut registry);
    let counted = Arc::new(AtomicUsize::new(0));
    registry.register(Counter(Arc::clone(&counted)));
    let mut project = Project::new();
    let mut ids = Vec::new();
    for (type_id, params) in [
        (MIDI_IN_ID, vec![]),
        (VOICES_ID, vec![("tail", tail)]),
        ("noodle.osc.sine", vec![]),
        (
            "noodle.mod.adsr",
            vec![
                ("attack", 0.001),
                ("decay", 0.01),
                ("sustain", 1.0),
                ("release", 0.01),
            ],
        ),
        ("noodle.util.vca", vec![]),
        ("test.counter", vec![]),
        (VOICE_MIX_ID, vec![]),
        (OUTPUT_ID, vec![]),
    ] {
        let id = NodeId(ids.len() as u64 + 1);
        let mut node = Node::new(type_id);
        for (key, value) in params {
            node = node.with_param(key, value);
        }
        if type_id == VOICES_ID {
            let mut config = Config::new();
            config.set("voices", Value::Int(4));
            node = node.with_config(config);
        }
        Command::AddNode { id, node }.apply(&mut project).unwrap();
        ids.push(id);
    }
    for (from, port, to, to_port) in [
        (0, "out", 1, "in"),
        (1, "pitch", 2, "frequency"),
        (1, "gate", 3, "gate"),
        (2, "out", 4, "in"),
        (3, "out", 4, "level"),
        (4, "out", 5, "in"),
        (5, "out", 6, "in"),
        (6, "out", 7, "in"),
    ] {
        Command::Connect(Connection {
            from: Endpoint::new(ids[from], port),
            to: Endpoint::new(ids[to], to_port),
        })
        .apply(&mut project)
        .unwrap();
    }
    let (mut controller, processor) = engine(SETTINGS).unwrap();
    let diagnostics = controller.update_project(&project, &registry);
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    (processor, library.midi, counted, controller)
}

fn block(processor: &mut Processor) -> Vec<f32> {
    let mut out = vec![0.0; SETTINGS.max_frames];
    processor.process(&mut out);
    out
}

/// Runs one block and says how many lanes the counter processed in it.
fn lanes_in_block(processor: &mut Processor, counted: &AtomicUsize) -> (usize, Vec<f32>) {
    let before = counted.load(Ordering::Relaxed);
    let out = block(processor);
    (counted.load(Ordering::Relaxed) - before, out)
}

#[test]
fn an_idle_synth_processes_no_voices() {
    let (mut processor, _midi, counted, _controller) = synth(10.0);
    // The very first block is already skipped: nothing has played.
    for _ in 0..8 {
        let (lanes, out) = lanes_in_block(&mut processor, &counted);
        assert_eq!(lanes, 0);
        assert!(out.iter().all(|&s| s == 0.0));
    }
}

#[test]
fn only_the_sounding_voices_are_processed_and_a_release_rings_out_first() {
    let (mut processor, midi, counted, _controller) = synth(10.0);
    let _ = block(&mut processor);

    midi.send([0x90, 60, 100]);
    let _ = block(&mut processor);
    midi.send([0x90, 64, 100]);
    let _ = block(&mut processor);
    for _ in 0..4 {
        let (lanes, out) = lanes_in_block(&mut processor, &counted);
        assert_eq!(lanes, 2, "two notes held, two voices processed");
        assert!(out.iter().any(|&s| s.abs() > 0.1));
    }

    // One key up: its envelope releases (10 ms is 2 blocks), so it keeps
    // processing until the tail has gone, and the sound is not cut short.
    midi.send([0x80, 60, 0]);
    let (lanes, _) = lanes_in_block(&mut processor, &counted);
    assert_eq!(lanes, 2);
    let mut release = Vec::new();
    for _ in 0..6 {
        release.push(lanes_in_block(&mut processor, &counted).0);
    }
    assert_eq!(release[0], 2, "still releasing: {release:?}");
    assert_eq!(
        *release.last().unwrap(),
        1,
        "released voice skipped: {release:?}"
    );

    midi.send([0x80, 64, 0]);
    let mut last = 2;
    for _ in 0..8 {
        last = lanes_in_block(&mut processor, &counted).0;
    }
    assert_eq!(last, 0, "everything released and skipped");
}

#[test]
fn a_skipped_voice_starts_cleanly_when_a_note_takes_it() {
    // The shortest tail makes released voices inactive soon, so the oscillator
    // is skipped too, not just the envelope's tail.
    let (mut processor, midi, counted, _controller) = synth(0.05);
    for round in 0..3 {
        midi.send([0x90, 69, 100]);
        let _ = block(&mut processor);
        let (lanes, out) = lanes_in_block(&mut processor, &counted);
        assert_eq!(lanes, 1, "round {round}");
        assert!(out.iter().any(|&s| s.abs() > 0.1), "round {round}");
        midi.send([0x80, 69, 0]);
        // The shortest tail is 50 ms: 2400 frames, 10 blocks.
        for _ in 0..14 {
            let _ = block(&mut processor);
        }
        let (lanes, out) = lanes_in_block(&mut processor, &counted);
        assert_eq!(lanes, 0, "round {round}");
        assert!(out.iter().all(|&s| s == 0.0));
    }
}

/// MIDI In → Voices (4, a 10 s tail) → Counter on the pitch → sine → VCA
/// (enveloped by an ADSR with a 10 ms release) → Voice Mix → Output. With
/// `busy`, the ADSR's `active` output goes back to the Voices node's `busy`
/// input. The counter sees a lane only while the voice is active.
fn pitch_counted_synth(busy: bool) -> (Processor, MidiBus, Arc<AtomicUsize>, impl Sized) {
    let mut registry = Registry::with_builtins();
    let library = register_library(&mut registry);
    let counted = Arc::new(AtomicUsize::new(0));
    registry.register(Counter(Arc::clone(&counted)));
    let mut project = Project::new();
    let mut ids = Vec::new();
    for type_id in [
        MIDI_IN_ID,
        VOICES_ID,
        "test.counter",
        "noodle.osc.sine",
        "noodle.mod.adsr",
        "noodle.util.vca",
        VOICE_MIX_ID,
        OUTPUT_ID,
    ] {
        let id = NodeId(ids.len() as u64 + 1);
        let mut node = Node::new(type_id);
        if type_id == VOICES_ID {
            let mut config = Config::new();
            config.set("voices", Value::Int(4));
            node = node.with_config(config).with_param("tail", 10.0);
        }
        if type_id == "noodle.mod.adsr" {
            node = node
                .with_param("attack", 0.001)
                .with_param("sustain", 1.0)
                .with_param("release", 0.01);
        }
        Command::AddNode { id, node }.apply(&mut project).unwrap();
        ids.push(id);
    }
    let mut wires = vec![
        (0, "out", 1, "in"),
        (1, "pitch", 2, "in"),
        (2, "out", 3, "frequency"),
        (1, "gate", 4, "gate"),
        (3, "out", 5, "in"),
        (4, "out", 5, "level"),
        (5, "out", 6, "in"),
        (6, "out", 7, "in"),
    ];
    if busy {
        wires.push((4, "active", 1, "busy"));
    }
    for (from, port, to, to_port) in wires {
        Command::Connect(Connection {
            from: Endpoint::new(ids[from], port),
            to: Endpoint::new(ids[to], to_port),
        })
        .apply(&mut project)
        .unwrap();
    }
    let (mut controller, processor) = engine(SETTINGS).unwrap();
    let diagnostics = controller.update_project(&project, &registry);
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    (processor, library.midi, counted, controller)
}

#[test]
fn an_envelope_that_reports_busy_retires_its_voice_when_the_release_ends() {
    // 10 ms of release is two blocks of 256 frames at 48 kHz.
    let after_release = |busy: bool| {
        let (mut processor, midi, counted, _controller) = pitch_counted_synth(busy);
        midi.send([0x90, 60, 100]);
        let _ = block(&mut processor);
        let (held, _) = lanes_in_block(&mut processor, &counted);
        assert_eq!(held, 1, "busy {busy}");
        midi.send([0x80, 60, 0]);
        for _ in 0..8 {
            let _ = block(&mut processor);
        }
        lanes_in_block(&mut processor, &counted).0
    };
    assert_eq!(
        after_release(true),
        0,
        "retired as soon as the release ended"
    );
    assert_eq!(
        after_release(false),
        1,
        "without busy it waits for the tail"
    );
}

#[test]
fn skipping_never_allocates_on_the_audio_thread() {
    let (mut processor, midi, _counted, _controller) = synth(0.01);
    let mut violations = 0;
    let mut out = vec![0.0; SETTINGS.max_frames];
    for round in 0..40u8 {
        midi.send([0x90, 40 + round, 90]);
        midi.send([0x90, 52 + round % 5, 90]);
        violations += realtime(|| processor.process(&mut out));
        midi.send([0x80, 40 + round, 0]);
        for _ in 0..4 {
            violations += realtime(|| processor.process(&mut out));
        }
    }
    assert_eq!(violations, 0, "the audio thread allocated or freed memory");
}

/// MIDI In → Voices (4) → saw → ladder (swept by an ADSR) → VCA (amp
/// ADSR) → Voice Mix → Output: the subtractive synth, played from a keyboard.
fn subtractive() -> (Processor, MidiBus, impl Sized) {
    let mut registry = Registry::with_builtins();
    let library = register_library(&mut registry);
    let mut project = Project::new();
    let mut ids = Vec::new();
    for (type_id, params) in [
        (MIDI_IN_ID, vec![]),
        (VOICES_ID, vec![("tail", 0.05)]),
        ("noodle.osc.saw", vec![]),
        ("noodle.filter.ladder", vec![("resonance", 0.6)]),
        (
            "noodle.mod.adsr",
            vec![("attack", 0.001), ("release", 0.02)],
        ),
        (
            "noodle.mod.adsr",
            vec![("attack", 0.001), ("release", 0.02)],
        ),
        ("noodle.util.vca", vec![]),
        (VOICE_MIX_ID, vec![]),
        (OUTPUT_ID, vec![]),
        // The filter envelope, halved and shaped by a typed expression.
        ("noodle.util.math", vec![]),
    ] {
        let id = NodeId(ids.len() as u64 + 1);
        let mut node = Node::new(type_id);
        for (key, value) in params {
            node = node.with_param(key, value);
        }
        if type_id == "noodle.util.math" {
            let mut config = Config::new();
            config.set(
                "expr",
                Value::Text("clamp(a * 0.5 + sin(b) * 0, 0, 1)".into()),
            );
            node = node.with_config(config);
        }
        if type_id == VOICES_ID {
            let mut config = Config::new();
            config.set("voices", Value::Int(4));
            node = node.with_config(config);
        }
        Command::AddNode { id, node }.apply(&mut project).unwrap();
        ids.push(id);
    }
    for (from, port, to, to_port) in [
        (0, "out", 1, "in"),
        (1, "pitch", 2, "frequency"),
        (2, "out", 3, "in"),
        (1, "gate", 4, "gate"),
        (4, "out", 9, "a"),
        (9, "out", 3, "cutoff"),
        (1, "gate", 5, "gate"),
        (3, "out", 6, "in"),
        (5, "out", 6, "level"),
        (6, "out", 7, "in"),
        (7, "out", 8, "in"),
    ] {
        Command::Connect(Connection {
            from: Endpoint::new(ids[from], port),
            to: Endpoint::new(ids[to], to_port),
        })
        .apply(&mut project)
        .unwrap();
    }
    let (mut controller, processor) = engine(Settings {
        channels: 2,
        ..SETTINGS
    })
    .unwrap();
    let diagnostics = controller.update_project(&project, &registry);
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    (processor, library.midi, controller)
}

#[test]
fn the_subtractive_synth_sounds_and_never_allocates() {
    let (mut processor, midi, _controller) = subtractive();
    let mut out = vec![0.0; SETTINGS.max_frames * 2];
    let mut violations = 0;
    let mut heard = false;
    for round in 0..30u8 {
        midi.send([0x90, 48 + round % 12, 100]);
        midi.send([0x90, 60 + round % 7, 100]);
        violations += realtime(|| processor.process(&mut out));
        heard |= out.iter().any(|&s| s.abs() > 0.05);
        assert!(out.iter().all(|s| s.is_finite()));
        midi.send([0x80, 48 + round % 12, 0]);
        midi.send([0x80, 60 + round % 7, 0]);
        for _ in 0..6 {
            violations += realtime(|| processor.process(&mut out));
        }
    }
    assert!(heard);
    assert_eq!(violations, 0, "the audio thread allocated or freed memory");
    // Everything released and past its tail: silence.
    for _ in 0..40 {
        processor.process(&mut out);
    }
    assert!(out.iter().all(|&s| s == 0.0));
}

/// Silence flags cross a group's boundaries and its stage: a counter inside a
/// group (whose output has a gain set, so it keeps a stage) only processes the
/// voices that sound.
#[test]
fn skipping_works_through_a_group_and_its_stage() {
    let mut registry = Registry::with_builtins();
    let library = register_library(&mut registry);
    let counted = Arc::new(AtomicUsize::new(0));
    registry.register(Counter(Arc::clone(&counted)));
    let project = Project::from_ron(
        r#"(
        format: 1,
        graph: (
            nodes: {
                1: (type: "noodle.event.midi_in"),
                2: (type: "noodle.poly.voices", config: {"voices": 4}, params: {"tail": 0.05}),
                3: (type: "noodle.osc.sine"),
                4: (type: "noodle.group"),
                5: (type: "noodle.group.input", config: {"name": "in"}, parent: Some(4)),
                6: (type: "test.counter", parent: Some(4)),
                7: (type: "noodle.group.output", config: {"name": "out"}, params: {"gain": 0.0, "mute": 0.0}, parent: Some(4)),
                8: (type: "noodle.poly.voice_mix"),
                9: (type: "noodle.io.output"),
            },
            connections: [
                (from: (node: 1, port: "out"), to: (node: 2, port: "in")),
                (from: (node: 2, port: "pitch"), to: (node: 3, port: "frequency")),
                (from: (node: 3, port: "out"), to: (node: 4, port: "in")),
                (from: (node: 5, port: "out"), to: (node: 6, port: "in")),
                (from: (node: 6, port: "out"), to: (node: 7, port: "in")),
                (from: (node: 4, port: "out"), to: (node: 8, port: "in")),
                (from: (node: 8, port: "out"), to: (node: 9, port: "in")),
            ],
        ),
    )"#,
    )
    .unwrap();
    let (mut controller, mut processor) = engine(SETTINGS).unwrap();
    let diagnostics = controller.update_project(&project, &registry);
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    let midi = library.midi;

    for _ in 0..4 {
        assert_eq!(lanes_in_block(&mut processor, &counted).0, 0, "idle");
    }
    midi.send([0x90, 60, 100]);
    midi.send([0x90, 64, 100]);
    let _ = block(&mut processor);
    let (lanes, out) = lanes_in_block(&mut processor, &counted);
    assert_eq!(lanes, 2, "two notes, two voices through the group");
    assert!(out.iter().any(|&s| s.abs() > 0.1), "and they sound");

    midi.send([0x80, 60, 0]);
    midi.send([0x80, 64, 0]);
    for _ in 0..14 {
        let _ = block(&mut processor);
    }
    assert_eq!(lanes_in_block(&mut processor, &counted).0, 0, "released");
}
