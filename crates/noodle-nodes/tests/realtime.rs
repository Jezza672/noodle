//! The engine's real-time guarantees, checked with the real node library:
//! rendering never allocates or frees memory, even while parameters change and
//! new plans are swapped in, and swapping plans doesn't disturb the sound.
//! Meters and scopes are part of the graph, so reporting telemetry is checked
//! too.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::f32::consts::TAU;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use noodle_core::{Command, Config, Connection, Endpoint, Node, NodeId, Project, Value};
use noodle_engine::{
    Controller, INPUT_ID, OUTPUT_ID, Processor, Registry, ScopeView, Settings, Telemetry, engine,
};
use noodle_io::{DeviceWriter, input_path};

/// Counts allocations and frees made while the current thread is marked as
/// real-time.
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

/// Runs `f` as if on the audio thread, returning how many times it allocated
/// or freed memory.
fn realtime(f: impl FnOnce()) -> usize {
    REALTIME.set(true);
    f();
    REALTIME.set(false);
    VIOLATIONS.replace(0)
}

const SETTINGS: Settings = Settings {
    sample_rate: 48_000.0,
    max_frames: 256,
    channels: 2,
};

struct Session {
    project: Project,
    registry: Registry,
    telemetry: Telemetry,
    controller: Controller,
    processor: Processor,
}

impl Session {
    fn new() -> Self {
        let mut registry = Registry::with_builtins();
        let telemetry = noodle_nodes::register_all(&mut registry);
        let (controller, processor) = engine(SETTINGS).unwrap();
        Self {
            project: Project::new(),
            registry,
            telemetry,
            controller,
            processor,
        }
    }

    fn add(&mut self, node: Node) -> NodeId {
        let id = self.project.new_node_id();
        self.edit(Command::AddNode { id, node });
        id
    }

    fn wire(&mut self, from: NodeId, from_port: &str, to: NodeId, to_port: &str) {
        self.edit(Command::Connect(Connection {
            from: Endpoint::new(from, from_port),
            to: Endpoint::new(to, to_port),
        }));
    }

    fn edit(&mut self, command: Command) {
        command.apply(&mut self.project).unwrap();
    }

    fn update(&mut self) {
        let diagnostics = self
            .controller
            .update_project(&self.project, &self.registry);
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
    }
}

/// sine → filter → gain → mix (with a second sine) → voice mix → output,
/// with an LFO on the filter cutoff, and a meter and scope on the mix.
fn busy_session() -> (Session, NodeId, NodeId, NodeId, NodeId) {
    let mut s = Session::new();
    let sine = s.add(Node::new("noodle.osc.sine").with_param("frequency", 220.0));
    let lfo = s.add(Node::new("noodle.osc.sine").with_param("frequency", 2.0));
    let svf = s.add(Node::new("noodle.filter.svf"));
    let gain = s.add(Node::new("noodle.util.gain"));
    let other = s.add(Node::new("noodle.osc.sine").with_param("frequency", 330.0));
    let mix = s.add(Node::new("noodle.util.mix"));
    let reroute = s.add(Node::new("noodle.util.reroute"));
    let voices = s.add(Node::new("noodle.poly.voice_mix"));
    let output = s.add(Node::new(OUTPUT_ID));
    s.wire(sine, "out", svf, "in");
    s.wire(lfo, "out", svf, "resonance");
    s.wire(svf, "low", gain, "in");
    s.wire(gain, "out", reroute, "in");
    s.wire(reroute, "out", mix, "in1");
    s.wire(other, "out", mix, "in2");
    s.wire(mix, "out", voices, "in");
    s.wire(voices, "out", output, "in");
    let meter = s.add(Node::new("noodle.view.meter"));
    let scope = s.add(Node::new("noodle.view.scope"));
    s.wire(mix, "out", meter, "in");
    s.wire(mix, "out", scope, "in");
    s.update();
    (s, gain, mix, meter, scope)
}

#[test]
fn rendering_never_allocates_even_while_editing() {
    let (mut s, gain, mix, meter, scope) = busy_session();
    // Longer than max_frames, so each call renders several blocks.
    let mut out = vec![0.0; 1000 * SETTINGS.channels];
    let mut view = ScopeView::default();
    let reader = s.telemetry.meter_reader();

    for round in 0..12 {
        // UI thread: allowed to allocate.
        let db = -(round as f32);
        s.edit(Command::SetParam {
            node: gain,
            key: "gain".into(),
            value: Some(db),
        });
        s.controller.set_param(gain, "gain", db);
        match round % 3 {
            // A new node: everything else carries over.
            1 => {
                s.add(Node::new("noodle.osc.sine"));
                s.update();
            }
            // A config change: the mix is rebuilt.
            2 => {
                let inputs = 2 + round / 3;
                s.edit(Command::SetConfig {
                    node: mix,
                    key: "inputs".into(),
                    value: Some(Value::Int(inputs as i64)),
                });
                s.update();
            }
            _ => {}
        }
        s.controller.maintain();

        // Audio thread: installs any new plan, then renders.
        let violations = realtime(|| {
            for _ in 0..4 {
                s.processor.process(&mut out);
            }
        });
        assert_eq!(
            violations, 0,
            "allocated on the audio thread in round {round}"
        );

        // UI thread: the meter and scope have reported.
        let levels = reader.meter(meter).unwrap();
        assert!(levels[0].peak > 0.0, "no meter level in round {round}");
        // And the mixer has reported levels per input (the test adds one).
        let inputs = reader.meter(mix).unwrap();
        assert!(
            inputs.len() >= 2 && inputs.iter().any(|l| l.peak > 0.0),
            "no mixer input levels in round {round}: {inputs:?}"
        );
        assert!(s.telemetry.read_scope(scope, &mut view));
        assert!(
            !view.samples().is_empty(),
            "no scope samples in round {round}"
        );
    }

    assert!(out.iter().all(|x| x.is_finite()));
    assert!(out.iter().any(|&x| x != 0.0), "should be making sound");
}

#[test]
fn the_device_writer_never_allocates() {
    let (s, ..) = busy_session();
    let mut writer = DeviceWriter::new(s.processor);
    // Longer than a block, in a format that needs converting.
    let mut out = vec![0i16; 1000 * SETTINGS.channels];
    let violations = realtime(|| {
        for _ in 0..4 {
            writer.write(&mut out);
        }
    });
    assert_eq!(violations, 0, "allocated in the audio callback");
    assert!(out.iter().any(|&x| x != 0), "should be making sound");
}

#[test]
fn the_device_writer_never_allocates_with_input() {
    let mut s = Session::new();
    let input = s.add(Node::new(INPUT_ID));
    let svf = s.add(Node::new("noodle.filter.svf"));
    let output = s.add(Node::new(OUTPUT_ID));
    s.wire(input, "out", svf, "in");
    s.wire(svf, "low", output, "in");
    s.update();
    let glitches = Arc::new(AtomicU64::new(0));
    let (mut capture, feed) = input_path(1, SETTINGS.sample_rate, SETTINGS.max_frames, glitches);
    let mut writer = DeviceWriter::with_input(s.processor, feed);
    let recorded: Vec<i16> = (0..1000).map(|x| (x % 200) * 100).collect();
    let mut out = vec![0i16; 1000 * SETTINGS.channels];
    let violations = realtime(|| {
        for _ in 0..4 {
            capture.capture(&recorded);
            writer.write(&mut out);
        }
        // And running dry.
        writer.write(&mut out);
    });
    assert_eq!(violations, 0, "allocated in the audio callbacks");
    // The last block ran dry, so look at a fresh one.
    capture.capture(&recorded);
    writer.write(&mut out);
    assert!(out.iter().any(|&x| x != 0), "should be playing the input");
}

#[test]
fn swapping_plans_mid_render_is_seamless() {
    let session = || {
        let mut s = Session::new();
        let sine = s.add(Node::new("noodle.osc.sine").with_param("frequency", 440.0));
        let gain = s.add(Node::new("noodle.util.gain").with_param("gain", -6.0));
        let output = s.add(Node::new(OUTPUT_ID));
        s.wire(sine, "out", gain, "in");
        s.wire(gain, "out", output, "in");
        s.update();
        s
    };
    let render = |s: &mut Session| {
        let mut out = vec![0.0; 300 * SETTINGS.channels];
        s.processor.process(&mut out);
        out
    };

    let mut steady = session();
    let expected: Vec<f32> = (0..4).flat_map(|_| render(&mut steady)).collect();

    let mut edited = session();
    let mut actual: Vec<f32> = (0..2).flat_map(|_| render(&mut edited)).collect();
    // An edit elsewhere in the graph: the sine and gain carry on untouched.
    edited.add(Node::new("noodle.util.mix").with_config(Config::new()));
    edited.update();
    actual.extend((0..2).flat_map(|_| render(&mut edited)));

    assert!(expected.iter().any(|&x| x != 0.0));
    assert_eq!(actual, expected);
}

#[test]
fn deleting_a_view_node_closes_its_channel() {
    let (mut s, _, _, meter, scope) = busy_session();
    let mut out = vec![0.0; 256 * SETTINGS.channels];
    s.processor.process(&mut out);
    let reader = s.telemetry.meter_reader();
    assert!(reader.meter(meter).is_some());

    s.edit(Command::RemoveNode { id: meter });
    s.update();
    // The audio thread installs the new plan and hands back the old one,
    // which the controller frees, dropping the meter's writer.
    s.processor.process(&mut out);
    s.controller.maintain();

    assert!(reader.meter(meter).is_none());
    assert!(s.telemetry.read_scope(scope, &mut ScopeView::default()));
}

#[test]
fn changing_the_audible_path_mid_render_does_not_click() {
    let mut s = Session::new();
    let sine = s.add(Node::new("noodle.osc.sine").with_param("frequency", 440.0));
    let gain = s.add(Node::new("noodle.util.gain"));
    let output = s.add(Node::new(OUTPUT_ID));
    s.wire(sine, "out", gain, "in");
    s.wire(gain, "out", output, "in");
    s.update();

    let render = |s: &mut Session, frames: usize| {
        let mut out = vec![0.0; frames * SETTINGS.channels];
        s.processor.process(&mut out);
        out
    };
    // An odd length, so the swap lands mid-block and mid-cycle.
    let mut out = render(&mut s, 1007);
    // The steepest a full-scale 440 Hz sine gets between samples.
    let steady = TAU * 440.0 / SETTINGS.sample_rate;

    // Removing the gain rewires the output straight to the sine.
    s.edit(Command::RemoveNode { id: gain });
    s.wire(sine, "out", output, "in");
    s.update();
    out.extend(render(&mut s, 500));

    // A new sine in its place starts at phase 0, a jump without a fade.
    s.edit(Command::RemoveNode { id: sine });
    let fresh = s.add(Node::new("noodle.osc.sine").with_param("frequency", 440.0));
    s.wire(fresh, "out", output, "in");
    s.update();
    out.extend(render(&mut s, 500));

    let jump = out
        .chunks(SETTINGS.channels)
        .zip(out.chunks(SETTINGS.channels).skip(1))
        .map(|(a, b)| (a[0] - b[0]).abs())
        .fold(0.0, f32::max);
    assert!(jump < steady * 1.5, "jump of {jump}, steady {steady}");
}

#[test]
fn the_transport_never_allocates_on_the_audio_thread() {
    use noodle_core::{TempoMap, Tick, TimeSignature};

    let (mut s, ..) = busy_session();
    let transport = s.controller.transport();
    let mut out = vec![0.0; 1000 * SETTINGS.channels];

    for round in 0..12 {
        // UI thread: allowed to allocate.
        match round % 4 {
            0 => transport.seek(Tick(100 * round)),
            1 => transport.set_loop(Some((Tick(0), Tick(960 * (1 + round))))),
            2 => {
                let bpm = 80.0 + 7.0 * round as f64;
                let map = TempoMap::constant(bpm, TimeSignature::COMMON).unwrap();
                s.controller.set_tempo_map(&map);
            }
            _ => {
                transport.stop();
                transport.seek(Tick(480));
                transport.play();
                transport.set_loop(None);
            }
        }
        s.controller.maintain();

        // Audio thread: seeks, loop wraps and tempo swaps all happen here.
        let violations = realtime(|| {
            for _ in 0..4 {
                s.processor.process(&mut out);
            }
        });
        assert_eq!(
            violations, 0,
            "allocated on the audio thread in round {round}"
        );
    }
    assert!(out.iter().all(|x| x.is_finite()));
}

#[test]
fn group_stages_never_allocate_while_controls_change() {
    use noodle_core::group::group_nodes;

    let (mut s, gain, ..) = busy_session();
    let (group, command) =
        group_nodes(&s.project.clone(), &[gain], || s.project.new_node_id()).unwrap();
    command.apply(&mut s.project).unwrap();
    s.update();
    let output = s.project.graph().group_ports(group).outputs[0].node;
    let mut out = vec![0.0; 1000 * SETTINGS.channels];

    for round in 0..12 {
        // UI thread: a stage appears, and mute and solo recompile.
        let set = |s: &mut Session, key: &str, value: f32| {
            s.edit(Command::SetParam {
                node: output,
                key: key.into(),
                value: Some(value),
            });
        };
        set(&mut s, "gain", -(round as f32));
        set(&mut s, "mute", (round % 2) as f32);
        set(&mut s, "solo", ((round / 4) % 2) as f32);
        s.update();
        s.controller.maintain();

        let violations = realtime(|| {
            for _ in 0..4 {
                s.processor.process(&mut out);
            }
        });
        assert_eq!(
            violations, 0,
            "allocated on the audio thread in round {round}"
        );
        assert!(out.iter().all(|x| x.is_finite()));
    }
}

/// Two tracks into a mix: a silent one and a 440 Hz one, each a group with
/// the track's gain on its output node. Returns the session and the silent
/// track's output node.
fn two_tracks_session(touch_first: bool) -> (Session, NodeId) {
    use noodle_core::group::group_nodes;

    let mut s = Session::new();
    let mix = s.add(Node::new("noodle.util.mix"));
    let output = s.add(Node::new(OUTPUT_ID));
    s.wire(mix, "out", output, "in");
    let mut outputs = Vec::new();
    for (i, hz) in [0.0, 440.0].into_iter().enumerate() {
        let sine = s.add(Node::new("noodle.osc.sine").with_param("frequency", hz));
        let gain = s.add(Node::new("noodle.util.gain"));
        s.wire(sine, "out", gain, "in");
        s.wire(gain, "out", mix, ["in1", "in2"][i]);
        let (group, command) =
            group_nodes(&s.project.clone(), &[gain], || s.project.new_node_id()).unwrap();
        command.apply(&mut s.project).unwrap();
        outputs.push(s.project.graph().group_ports(group).outputs[0].node);
    }
    if touch_first {
        // Set to what they already are: the stage appears now, in the plan
        // the engine starts with.
        s.edit(Command::SetParam {
            node: outputs[0],
            key: "mute".into(),
            value: Some(0.0),
        });
    }
    s.update();
    (s, outputs[0])
}

/// The lowest per-block peak of the left channel while `edit` is applied to a
/// running session, over blocks of 64 frames.
fn lowest_peak_across(touch_first: bool, edit: impl FnOnce(&mut Session, NodeId)) -> f32 {
    let (mut s, muted_track) = two_tracks_session(touch_first);
    let mut block = vec![0.0; 64 * SETTINGS.channels];
    for _ in 0..20 {
        s.processor.process(&mut block);
    }
    edit(&mut s, muted_track);
    s.update();
    let mut lowest = f32::MAX;
    for _ in 0..40 {
        s.processor.process(&mut block);
        let peak = block
            .chunks(SETTINGS.channels)
            .map(|frame| frame[0].abs())
            .fold(0.0, f32::max);
        lowest = lowest.min(peak);
    }
    lowest
}

#[test]
fn muting_a_track_whose_stage_is_in_place_does_not_dip_the_others() {
    let mute = |s: &mut Session, node| {
        s.edit(Command::SetParam {
            node,
            key: "mute".into(),
            value: Some(1.0),
        });
    };
    // The stage already exists, so muting is a parameter change.
    let lowest = lowest_peak_across(true, mute);
    assert!(lowest > 0.3, "the playing track dipped to {lowest}");
    // The first touch adds the stage, which fades the whole output once. This
    // is what the test above guards against, and shows it can fail.
    let first = lowest_peak_across(false, mute);
    assert!(first < 0.2, "expected the structural fade, lowest {first}");
}

#[test]
fn automation_lanes_never_allocate_on_the_audio_thread() {
    use noodle_core::{AutomationLane, AutomationPoint, Curve, Endpoint, Tick};

    let (mut s, gain, ..) = busy_session();
    let points = |n: i64| {
        (0..n)
            .map(|i| AutomationPoint {
                tick: Tick(i * 480),
                value: -(i as f32),
                curve: if i % 2 == 0 {
                    Curve::Linear
                } else {
                    Curve::Hold
                },
            })
            .collect()
    };
    let id = s.project.new_lane_id();
    s.edit(Command::AddLane {
        id,
        lane: AutomationLane::new(Endpoint::new(gain, "gain"), points(8)),
    });
    s.update();
    let transport = s.controller.transport();
    let mut out = vec![0.0; 1000 * SETTINGS.channels];
    for round in 0..8 {
        if round == 4 {
            // Editing the lane rebuilds its source off the audio thread.
            s.edit(Command::SetLane {
                id,
                lane: AutomationLane::new(Endpoint::new(gain, "gain"), points(12)),
            });
            s.update();
        }
        transport.seek(Tick(300 * round));
        s.controller.maintain();
        let violations = realtime(|| {
            for _ in 0..4 {
                s.processor.process(&mut out);
            }
        });
        assert_eq!(violations, 0, "allocated in round {round}");
    }
    assert!(out.iter().all(|x| x.is_finite()));
}
