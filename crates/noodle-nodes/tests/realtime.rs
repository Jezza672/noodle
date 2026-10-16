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
        let (mut controller, processor) = engine(SETTINGS).unwrap();
        // So wired parameters are probed, as in the app.
        controller.set_telemetry(&telemetry);
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
        // The mixer's first input gets its own gain, and its mute flips.
        for (key, value) in [("gain1", db), ("mute1", (round % 2) as f32)] {
            s.edit(Command::SetParam {
                node: mix,
                key: key.into(),
                value: Some(value),
            });
            s.controller.set_param(mix, key, value);
        }
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
        // The gain node shows a meter of its own.
        assert!(
            reader.meter(gain).is_some(),
            "no gain level in round {round}"
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

/// sine → Analyze → gate → shift → blur → freeze → Cross (with a second,
/// noise analysis as the modulator) → Synthesize → output.
fn spectral_session() -> (Session, [NodeId; 4]) {
    let mut s = Session::new();
    let sine = s.add(Node::new("noodle.osc.sine").with_param("frequency", 220.0));
    let noise = s.add(Node::new("noodle.noise.white"));
    let (a, b) = (
        s.add(Node::new("noodle.spectral.analyze")),
        s.add(Node::new("noodle.spectral.analyze")),
    );
    let gate = s.add(Node::new("noodle.spectral.gate"));
    let shift = s.add(Node::new("noodle.spectral.shift"));
    let blur = s.add(Node::new("noodle.spectral.blur"));
    let freeze = s.add(Node::new("noodle.spectral.freeze"));
    let cross = s.add(Node::new("noodle.spectral.cross"));
    let synth = s.add(Node::new("noodle.spectral.synthesize"));
    let output = s.add(Node::new(OUTPUT_ID));
    s.wire(sine, "out", a, "in");
    s.wire(noise, "out", b, "in");
    s.wire(a, "spectrum", gate, "spectrum");
    s.wire(gate, "out", shift, "spectrum");
    s.wire(shift, "out", blur, "spectrum");
    s.wire(blur, "out", freeze, "spectrum");
    s.wire(freeze, "out", cross, "carrier");
    s.wire(b, "spectrum", cross, "modulator");
    s.wire(cross, "out", synth, "spectrum");
    s.wire(synth, "out", output, "in");
    s.update();
    (s, [shift, freeze, cross, a])
}

#[test]
fn spectral_chains_never_allocate_even_while_editing() {
    let (mut s, [shift, freeze, cross, a]) = spectral_session();
    let mut out = vec![0.0; 1000 * SETTINGS.channels];
    for round in 0..8 {
        for (node, key, value) in [
            (shift, "shift", 40.0 * round as f32),
            (freeze, "freeze", (round % 2) as f32),
            (cross, "mix", round as f32 / 8.0),
        ] {
            s.edit(Command::SetParam {
                node,
                key: key.into(),
                value: Some(value),
            });
            s.controller.set_param(node, key, value);
        }
        // A new node and a rewire swap the plan with spectral state in it.
        if round % 3 == 1 {
            let extra = s.add(Node::new("noodle.spectral.blur"));
            s.wire(a, "spectrum", extra, "spectrum");
            s.update();
        }
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
        set(&mut s, "gain.1", -(round as f32));
        set(&mut s, "mute.1", (round % 2) as f32);
        set(&mut s, "solo.1", ((round / 4) % 2) as f32);
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
            key: "mute.1".into(),
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
            key: "mute.1".into(),
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

#[test]
fn the_metronome_never_allocates_through_seeks_loops_and_button_presses() {
    use noodle_core::{TempoMap, Tick, TimeSignature};

    let mut s = Session::new();
    let button = s.add(Node::new("noodle.input.button").with_param("state", 1.0));
    let metronome = s.add(Node::new("noodle.util.metronome"));
    let output = s.add(Node::new(OUTPUT_ID));
    s.wire(button, "out", metronome, "on");
    s.wire(metronome, "out", output, "in");
    s.update();

    let transport = s.controller.transport();
    let mut out = vec![0.0; 1000 * SETTINGS.channels];
    let mut heard = false;
    for round in 0..12 {
        match round % 4 {
            0 => transport.seek(Tick(480 * round)),
            1 => transport.set_loop(Some((Tick(0), Tick(960 * 2)))),
            2 => {
                let map =
                    TempoMap::constant(90.0 + 11.0 * round as f64, TimeSignature::COMMON).unwrap();
                s.controller.set_tempo_map(&map);
            }
            _ => {
                transport.stop();
                s.controller.set_param(button, "state", round as f32 % 2.0);
                transport.play();
                transport.set_loop(None);
            }
        }
        s.controller.maintain();
        let violations = realtime(|| {
            for _ in 0..8 {
                s.processor.process(&mut out);
                heard |= out.iter().any(|&x| x.abs() > 1e-3);
            }
        });
        assert_eq!(
            violations, 0,
            "allocated on the audio thread in round {round}"
        );
    }
    // A loop that really wraps: 30 blocks of 1000 frames pass the 24 000
    // frames of a one-beat loop at 120 bpm.
    s.controller
        .set_tempo_map(&TempoMap::constant(120.0, TimeSignature::COMMON).unwrap());
    transport.set_loop(Some((Tick(0), Tick(960))));
    transport.seek(Tick(0));
    s.controller.maintain();
    let violations = realtime(|| {
        for _ in 0..30 {
            s.processor.process(&mut out);
            heard |= out.iter().any(|&x| x.abs() > 1e-3);
        }
    });
    assert_eq!(violations, 0, "allocated while the loop wrapped");
    assert!(transport.position() < 30_000, "the loop never wrapped");
    assert!(heard, "the metronome never sounded");
    assert!(out.iter().all(|x| x.is_finite()));
}

#[test]
fn events_envelopes_and_modulated_parameters_never_allocate() {
    // button -> key -> mono note -> saw -> filter (cutoff offset by an LFO)
    // -> vca (level from an ADSR) -> output, with every wired parameter probed.
    let mut s = Session::new();
    let button = s.add(Node::new("noodle.input.button").with_param("state", 1.0));
    let key = s.add(Node::new("noodle.event.key"));
    let mono = s.add(Node::new("noodle.event.mono"));
    let saw = s.add(Node::new("noodle.osc.saw"));
    let lfo = s.add(
        Node::new("noodle.mod.lfo")
            .with_param("rate", 8.0)
            .with_param("depth", 0.1),
    );
    let svf = s.add(Node::new("noodle.filter.svf"));
    let adsr = s.add(
        Node::new("noodle.mod.adsr")
            .with_param("attack", 0.002)
            .with_param("release", 0.01),
    );
    let vca = s.add(Node::new("noodle.util.vca"));
    let output = s.add(Node::new(OUTPUT_ID));
    s.wire(button, "out", key, "gate");
    s.wire(key, "out", mono, "in");
    s.wire(mono, "pitch", saw, "frequency");
    s.wire(saw, "out", svf, "in");
    s.wire(lfo, "out", svf, "cutoff");
    s.wire(mono, "gate", adsr, "gate");
    s.wire(svf, "low", vca, "in");
    s.wire(adsr, "out", vca, "level");
    s.wire(vca, "out", output, "in");
    s.update();

    let mut out = vec![0.0; 1000 * SETTINGS.channels];
    let mut heard = false;
    for round in 0..16 {
        // Notes on and off, and a key change while held, from the button and
        // the key's own parameters.
        s.controller
            .set_param(button, "state", if round % 4 == 3 { 0.0 } else { 1.0 });
        s.controller.set_param(key, "note", 48.0 + round as f32);
        s.controller.maintain();
        let violations = realtime(|| {
            for _ in 0..4 {
                s.processor.process(&mut out);
                heard |= out.iter().any(|&x| x.abs() > 1e-3);
            }
        });
        assert_eq!(violations, 0, "allocated in round {round}");
        let _ = s.telemetry.read_param(svf, "cutoff");
        let _ = s.telemetry.read_param(vca, "level");
    }
    s.processor.process(&mut out);
    let cutoff = s.telemetry.read_param(svf, "cutoff").expect("it reports");
    assert!(
        cutoff.max > cutoff.min,
        "the LFO should move the cutoff: {cutoff:?}"
    );
    assert!(heard, "the patch never sounded");
    assert!(out.iter().all(|x| x.is_finite()));
}

#[test]
fn a_remap_never_allocates_while_its_curve_is_edited() {
    // lfo -> remap (pitch) -> sine -> output, and the curve is rewritten
    // between blocks: the table is built off the audio thread each time.
    let mut s = Session::new();
    let lfo = s.add(Node::new("noodle.mod.lfo").with_param("rate", 5.0));
    let remap = s.add(
        Node::new("noodle.util.remap")
            .with_param("in_min", -1.0)
            .with_param("out_min", 200.0)
            .with_param("out_max", 800.0),
    );
    let sine = s.add(Node::new("noodle.osc.sine"));
    let output = s.add(Node::new(OUTPUT_ID));
    s.wire(lfo, "out", remap, "in");
    s.wire(remap, "out", sine, "frequency");
    s.wire(sine, "out", output, "in");
    s.update();

    let mut out = vec![0.0; 1000 * SETTINGS.channels];
    let mut heard = false;
    for round in 0..8 {
        let curve = format!("0 0 0 0 {} 0;1 1 -0.3 0 0 0", 0.1 * round as f32);
        s.edit(Command::SetConfig {
            node: remap,
            key: "curve".to_owned(),
            value: Some(Value::Text(curve)),
        });
        s.update();
        s.controller.maintain();
        let violations = realtime(|| {
            for _ in 0..4 {
                s.processor.process(&mut out);
                heard |= out.iter().any(|&x| x.abs() > 1e-3);
            }
        });
        assert_eq!(violations, 0, "allocated in round {round}");
    }
    assert!(heard && out.iter().all(|x| x.is_finite()));
}

#[test]
fn editing_a_remap_curve_does_not_fade_the_output() {
    // A constant 0.5 comes out of the Remap whichever of these point-symmetric
    // curves it holds, so any dip in the output is a fade, not the new curve.
    let mut s = Session::new();
    let remap = s.add(
        Node::new("noodle.util.remap")
            .with_param("in_min", -1.0)
            .with_param("in_max", 1.0),
    );
    let output = s.add(Node::new(OUTPUT_ID));
    s.wire(remap, "out", output, "in");
    s.update();
    let mut block = vec![0.0; 64 * SETTINGS.channels];
    for _ in 0..20 {
        s.processor.process(&mut block);
    }
    let mut lowest = f32::MAX;
    for round in 0..10 {
        let reach = 0.1 + 0.05 * round as f32;
        let curve = format!("0 0 0 0 {reach} 0;1 1 -{reach} 0 0 0");
        s.edit(Command::SetConfig {
            node: remap,
            key: "curve".to_owned(),
            value: Some(Value::Text(curve)),
        });
        s.update();
        for _ in 0..4 {
            s.processor.process(&mut block);
            lowest = block
                .chunks(SETTINGS.channels)
                .map(|frame| frame[0])
                .fold(lowest, f32::min);
        }
    }
    assert!(lowest > 0.45, "the output dipped to {lowest} during edits");
}

#[test]
fn outputs_on_two_devices_never_allocate_and_each_has_a_scope() {
    let settings = Settings {
        channels: 4,
        ..SETTINGS
    };
    let mut registry = Registry::with_builtins();
    let telemetry = noodle_nodes::register_all(&mut registry);
    let (mut controller, processor) = engine(settings).unwrap();
    controller
        .set_buses(vec![
            noodle_engine::Bus {
                device: "main".into(),
                channels: 2,
            },
            noodle_engine::Bus {
                device: "second".into(),
                channels: 2,
            },
        ])
        .unwrap();
    let mut s = Session {
        project: Project::new(),
        registry,
        telemetry,
        controller,
        processor,
    };
    let mut outputs = Vec::new();
    for (frequency, device) in [(220.0, ""), (330.0, "second")] {
        let sine = s.add(Node::new("noodle.osc.sine").with_param("frequency", frequency));
        let mut config = Config::new();
        config.set("device", Value::Text(device.into()));
        let output = s.add(Node::new(OUTPUT_ID).with_config(config));
        s.wire(sine, "out", output, "in");
        outputs.push(output);
    }
    s.update();

    let glitches = Arc::new(AtomicU64::new(0));
    let (mut writer, mut extras) = DeviceWriter::new(s.processor).with_outputs(2, &[2], &glitches);
    let mut extra = extras.remove(0);
    let mut main_out = vec![0i16; 1000 * 2];
    let mut second_out = vec![0i16; 500 * 2];
    let violations = realtime(|| {
        for _ in 0..6 {
            writer.write(&mut main_out);
            extra.fill(&mut second_out);
        }
    });
    assert_eq!(violations, 0, "allocated in the audio callbacks");
    assert!(
        main_out.iter().any(|&x| x != 0),
        "the main device is silent"
    );
    assert!(second_out.iter().any(|&x| x != 0), "the second is silent");

    let mut view = ScopeView::default();
    for output in outputs {
        assert!(s.telemetry.read_scope(output, &mut view));
        assert!(
            view.samples().iter().any(|&x| x != 0.0),
            "no scope samples for {output:?}"
        );
    }
}

#[test]
fn a_feedback_loop_through_a_delay_never_allocates_even_while_the_loop_is_made_and_broken() {
    // saw -> mix -> delay -> gain -> back into the mix, and out. The loop is
    // closed and opened again while rendering, which swaps plans between
    // split and whole.
    let mut s = Session::new();
    let saw = s.add(Node::new("noodle.osc.saw").with_param("frequency", 110.0));
    let mix = s.add(Node::new("noodle.util.mix"));
    let delay = s.add(Node::new("noodle.util.delay").with_param("time", 0.02));
    let gain = s.add(Node::new("noodle.util.gain").with_param("gain", -6.0));
    let output = s.add(Node::new(OUTPUT_ID));
    s.wire(saw, "out", mix, "in1");
    s.wire(mix, "out", delay, "in");
    s.wire(delay, "out", gain, "in");
    s.wire(mix, "out", output, "in");
    s.update();

    let mut out = vec![0.0; 1000 * SETTINGS.channels];
    for round in 0..10 {
        let closed = round % 2 == 1;
        if closed {
            s.wire(gain, "out", mix, "in2");
        } else if round > 0 {
            s.edit(Command::Disconnect {
                input: Endpoint::new(mix, "in2"),
            });
        }
        s.update();
        s.controller
            .set_param(delay, "time", 0.01 + 0.005 * round as f32);
        s.controller.maintain();
        let violations = realtime(|| {
            for _ in 0..4 {
                s.processor.process(&mut out);
            }
        });
        assert_eq!(violations, 0, "allocated in round {round}");
        assert!(
            out.iter().all(|x| x.is_finite() && x.abs() < 10.0),
            "round {round}"
        );
        assert!(
            out.iter().any(|&x| x.abs() > 1e-3),
            "silent in round {round}"
        );
    }
}

#[test]
fn a_cached_player_never_allocates_through_seeks_and_loops() {
    use noodle_core::{Tick, group::group_nodes};
    use noodle_nodes::{Freezer, RenderRequest, freeze};

    const FRAMES: usize = 60_000;
    let mut s = Session::new();
    let osc = s.add(Node::new("noodle.osc.sine"));
    let output = s.add(Node::new(OUTPUT_ID));
    s.wire(osc, "out", output, "in");
    let project = s.project.clone();
    let mut next = project.next_node_id().0;
    let (group, command) = group_nodes(&project, &[osc], || {
        next += 1;
        NodeId(next - 1)
    })
    .unwrap();
    s.edit(command);
    s.edit(Command::SetFrozen {
        node: group,
        frozen: true,
    });

    let dir = std::env::temp_dir().join(format!("noodle-cached-rt-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let freezer = Freezer::new(noodle_io::CacheStore::open(&dir).unwrap());
    let request = RenderRequest {
        project: s.project.clone(),
        base: dir.clone(),
        settings: SETTINGS,
        frames: FRAMES,
        extend_registry: None,
    };
    freeze(&freezer, &request, &noodle_engine::Progress::new()).unwrap();
    let analysis = freezer.analyze(&s.project, &s.registry, SETTINGS, FRAMES, &dir);
    let plan = freezer.plan(&analysis, SETTINGS, FRAMES, false);
    assert!(!plan.replacements.is_empty());
    let diagnostics =
        s.controller
            .update_project_replacing(&s.project, &s.registry, &plan.replacements);
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    // Let the player's worker read ahead.
    std::thread::sleep(std::time::Duration::from_millis(100));

    let transport = s.controller.transport();
    let mut out = vec![0.0; 1000 * SETTINGS.channels];
    let mut heard = false;
    for round in 0..12 {
        match round % 4 {
            0 => transport.seek(Tick(480 * round)),
            1 => transport.set_loop(Some((Tick(0), Tick(960)))),
            2 => transport.set_loop(None),
            _ => {
                transport.stop();
                transport.play();
            }
        }
        s.controller.maintain();
        let violations = realtime(|| {
            for _ in 0..8 {
                s.processor.process(&mut out);
                heard |= out.iter().any(|&x| x.abs() > 1e-3);
            }
        });
        assert_eq!(violations, 0, "allocated in round {round}");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    // A loop that really wraps, with the worker keeping up.
    transport.set_loop(Some((Tick(0), Tick(960))));
    transport.seek(Tick(0));
    s.controller.maintain();
    let violations = realtime(|| {
        for i in 0..30 {
            s.processor.process(&mut out);
            heard |= out.iter().any(|&x| x.abs() > 1e-3);
            if i % 4 == 3 {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }
    });
    assert_eq!(violations, 0, "allocated while the loop wrapped");
    assert!(transport.position() < 30_000, "the loop never wrapped");
    assert!(heard, "the cached player never sounded");
    assert!(out.iter().all(|x| x.is_finite()));
}

#[test]
fn live_monitoring_never_allocates_while_notes_come_and_go() {
    let mut registry = Registry::with_builtins();
    let library = noodle_nodes::register_library(&mut registry);
    let (mut controller, mut processor) = engine(SETTINGS).unwrap();
    let mut project = Project::new();
    let track = project.new_node_id();
    let output = project.new_node_id();
    for (id, node) in [
        (track, Node::new(noodle_nodes::TRACK_INPUT_ID)),
        (output, Node::new(OUTPUT_ID)),
    ] {
        Command::AddNode { id, node }.apply(&mut project).unwrap();
    }
    Command::Connect(Connection {
        from: Endpoint::new(track, "audio"),
        to: Endpoint::new(output, "in"),
    })
    .apply(&mut project)
    .unwrap();
    let diagnostics = controller.update_project(&project, &registry);
    assert!(diagnostics.is_empty(), "{diagnostics:?}");

    let transport = controller.transport();
    let mut out = vec![0.0; 128 * SETTINGS.channels];
    for round in 0..16u8 {
        // Notes, a controller, then the track is disarmed with some held and
        // armed again, while the transport starts and stops.
        library.midi.send([0x90 | (round & 3), 48 + round, 100]);
        library.midi.send([0xB0, 1, 64]);
        library.midi.send([0x80, 48 + round.saturating_sub(2), 0]);
        library.clips.set_monitoring(track, round % 5 != 4);
        if round % 3 == 0 {
            library.midi.all_notes_off();
        }
        if round % 2 == 0 {
            transport.stop();
        } else {
            transport.play();
        }
        controller.maintain();
        let violations = realtime(|| {
            for _ in 0..4 {
                processor.process(&mut out);
            }
        });
        assert_eq!(violations, 0, "allocated in round {round}");
    }
}

/// Passes its input through 100 samples late and says so, so the engine has
/// something to compensate for.
struct Latent;

static LATENT_INFO: noodle_engine::NodeInfo = noodle_engine::NodeInfo {
    id: "test.latent",
    version: 1,
    name: "Latent",
    category: "Test",
};

impl noodle_engine::NodeType for Latent {
    fn info(&self) -> &noodle_engine::NodeInfo {
        &LATENT_INFO
    }

    fn layout(&self, _config: &Config) -> Result<noodle_engine::Layout, noodle_engine::NodeError> {
        Ok(noodle_engine::Layout::realtime()
            .input("in", "In")
            .output("out", "Out"))
    }

    fn instantiate(
        &self,
        _setup: &noodle_engine::Setup<'_>,
    ) -> Result<noodle_engine::Instance, noodle_engine::NodeError> {
        Ok(noodle_engine::Instance::realtime(LatentNode {
            ring: vec![0.0; 100],
            pos: 0,
        }))
    }
}

struct LatentNode {
    ring: Vec<f32>,
    pos: usize,
}

impl noodle_engine::Node for LatentNode {
    fn process(&mut self, _ctx: &noodle_engine::Context, io: noodle_engine::Io<'_, '_>) {
        let input = io.inputs[0].lane(0, 0);
        let out = io.outputs[0].lane_mut(0, 0);
        for (o, &x) in out.iter_mut().zip(input) {
            *o = std::mem::replace(&mut self.ring[self.pos], x);
            self.pos = (self.pos + 1) % self.ring.len();
        }
    }

    fn latency(&self) -> u32 {
        self.ring.len() as u32
    }
}

#[test]
fn delay_compensation_never_allocates_through_plan_swaps_and_seeks() {
    // saw -> latent -> mix, and a sine straight into the mix, which is
    // delayed to match. The plan is swapped (a node is added and removed)
    // and the transport jumps while rendering.
    let mut s = Session::new();
    s.registry.register(Latent);
    let saw = s.add(Node::new("noodle.osc.saw").with_param("frequency", 110.0));
    let latent = s.add(Node::new("test.latent"));
    let sine = s.add(Node::new("noodle.osc.sine").with_param("frequency", 220.0));
    let mix = s.add(Node::new("noodle.util.mix"));
    let output = s.add(Node::new(OUTPUT_ID));
    s.wire(saw, "out", latent, "in");
    s.wire(latent, "out", mix, "in1");
    s.wire(sine, "out", mix, "in2");
    s.wire(mix, "out", output, "in");
    s.update();
    assert_eq!(s.controller.output_latency(), 100);

    let transport = s.controller.transport();
    let mut out = vec![0.0; 300 * SETTINGS.channels];
    for round in 0..8 {
        // The latent path is cut and restored, so the latency to compensate
        // comes and goes with each plan.
        let cut = round % 2 == 1;
        if cut {
            s.edit(Command::Disconnect {
                input: Endpoint::new(mix, "in1"),
            });
        } else if round > 0 {
            s.wire(latent, "out", mix, "in1");
        }
        s.update();
        assert_eq!(s.controller.output_latency(), if cut { 0 } else { 100 });
        if round % 3 == 2 {
            transport.seek(noodle_core::Tick(1_000));
        }
        s.controller.maintain();
        let violations = realtime(|| {
            for _ in 0..4 {
                s.processor.process(&mut out);
            }
        });
        assert_eq!(violations, 0, "allocated in round {round}");
        assert!(out.iter().all(|x| x.is_finite()), "round {round}");
        assert!(
            out.iter().any(|&x| x.abs() > 1e-3),
            "silent in round {round}"
        );
    }
}

#[test]
fn the_m9_nodes_never_allocate_through_notes_edits_and_config_changes() {
    use noodle_io::Audio;
    use noodle_nodes::{SAMPLE_KEY, SampleStore, Sampler};

    // key -> arpeggiator -> note split (two ranges and the rest) -> two
    // samplers (the rest unplayed) -> mix -> dynamics (lookahead, sidechained
    // from the first sampler) -> EQ -> reverb -> output.
    let mut s = Session::new();
    let store = SampleStore::new();
    s.registry.replace(Sampler::new(&store));
    let hash = store.insert(&Audio {
        samples: (0..4800).map(|i| (i as f32 * 0.07).sin() * 0.5).collect(),
        channels: 1,
        sample_rate: 48_000,
    });
    let sampler = |s: &mut Session| {
        s.add(
            Node::new("noodle.instrument.sampler")
                .with_config(Config::new().with(SAMPLE_KEY, Value::Text(hash.clone())))
                .with_param("mode", 1.0)
                .with_param("loop", 1.0)
                .with_param("loop_start", 0.2)
                .with_param("loop_end", 0.8),
        )
    };
    let button = s.add(Node::new("noodle.input.button").with_param("state", 1.0));
    let key = s.add(Node::new("noodle.event.key"));
    let arp = s.add(
        Node::new("noodle.event.arpeggiator")
            .with_param("rate", 5.0)
            .with_param("pattern", 5.0),
    );
    let split = s.add(
        Node::new("noodle.event.note_split").with_config(
            Config::new()
                .with("outputs", Value::Int(2))
                .with("rest", Value::Int(1)),
        ),
    );
    let (low, high) = (sampler(&mut s), sampler(&mut s));
    let mix = s.add(Node::new("noodle.util.mix"));
    let dynamics = s.add(
        Node::new("noodle.effect.dynamics")
            .with_config(Config::new().with("lookahead", Value::Int(2)))
            .with_param("detect", 1.0)
            .with_param("threshold", -30.0),
    );
    let eq = s.add(Node::new("noodle.effect.eq"));
    let reverb = s.add(Node::new("noodle.effect.reverb"));
    let output = s.add(Node::new(OUTPUT_ID));
    s.wire(button, "out", key, "gate");
    s.wire(key, "out", arp, "in");
    s.wire(arp, "out", split, "in");
    s.wire(split, "out1", low, "in");
    s.wire(split, "out2", high, "in");
    s.wire(low, "out", mix, "in1");
    s.wire(high, "out", mix, "in2");
    s.wire(mix, "out", dynamics, "in");
    s.wire(low, "out", dynamics, "sidechain");
    s.wire(dynamics, "out", eq, "in");
    s.wire(eq, "out", reverb, "in");
    s.wire(reverb, "out", output, "in");
    s.edit(Command::SetParam {
        node: split,
        key: "high1".into(),
        value: Some(59.0),
    });
    s.edit(Command::SetParam {
        node: split,
        key: "low2".into(),
        value: Some(60.0),
    });
    s.update();

    let mut out = vec![0.0; 1000 * SETTINGS.channels];
    let mut heard = false;
    for round in 0..16 {
        s.controller
            .set_param(button, "state", if round % 4 == 3 { 0.0 } else { 1.0 });
        s.controller
            .set_param(key, "note", 48.0 + 2.0 * round as f32);
        for (node, key, value) in [
            (dynamics, "ratio", 2.0 + round as f32),
            (eq, "gain1", round as f32 - 8.0),
            (reverb, "decay", 0.5 + round as f32 * 0.2),
            (low, "tune", 10.0 * round as f32),
        ] {
            s.controller.set_param(node, key, value);
        }
        // A config change rebuilds the node, and the plan is swapped.
        if round == 8 {
            s.edit(Command::SetConfig {
                node: eq,
                key: "bands".into(),
                value: Some(Value::Int(6)),
            });
            s.update();
        }
        s.controller.maintain();
        let violations = realtime(|| {
            for _ in 0..4 {
                s.processor.process(&mut out);
                heard |= out.iter().any(|&x| x.abs() > 1e-3);
            }
        });
        assert_eq!(violations, 0, "allocated in round {round}");
        assert!(out.iter().all(|x| x.is_finite()), "round {round}");
    }
    assert!(heard, "the patch never sounded");
}

#[test]
fn the_batch_4_nodes_never_allocate_through_triggers_seeks_and_config_changes() {
    use noodle_core::{AutomationLane, AutomationPoint, Curve, Tick};

    // A square LFO (4 Hz) and a clock both trigger envelopes (one per voice,
    // one shared); the LFO also runs through a comparator into a Sample &
    // Hold on noise, and a lag smooths the held value. A sine goes through a
    // distortion, bitcrusher, chorus and phaser and a VCA the envelope
    // opens; a follower listens to the sine, and an Automation node with a
    // lane drives the distortion's mix.
    let mut s = Session::new();
    let lfo = s.add(
        Node::new("noodle.mod.lfo")
            .with_param("rate", 4.0)
            .with_param("shape", 3.0),
    );
    let compare = s.add(Node::new("noodle.mod.comparator"));
    let clock = s.add(Node::new("noodle.generator.clock").with_param("rate", 4.0));
    let multi = s.add(Node::new("noodle.mod.envelope").with_param("length", 0.1));
    let single = s.add(
        Node::new("noodle.mod.envelope")
            .with_config(Config::new().with("single", Value::Int(1)))
            .with_param("length", 0.2)
            .with_param("loop", 1.0),
    );
    let noise = s.add(Node::new("noodle.noise.white"));
    let hold = s.add(Node::new("noodle.mod.samplehold"));
    let lag = s.add(Node::new("noodle.mod.lag").with_param("rise", 0.05));
    let sine = s.add(Node::new("noodle.osc.sine").with_param("frequency", 220.0));
    let distortion = s.add(Node::new("noodle.effect.distortion"));
    let crusher = s.add(Node::new("noodle.effect.bitcrusher"));
    let chorus = s.add(Node::new("noodle.effect.chorus"));
    let phaser = s.add(Node::new("noodle.effect.phaser").with_param("feedback", 0.5));
    let vca = s.add(Node::new("noodle.util.vca"));
    let follower = s.add(Node::new("noodle.mod.follower"));
    let automation = s.add(Node::new("noodle.mod.automation"));
    let output = s.add(Node::new(OUTPUT_ID));
    s.wire(lfo, "out", compare, "in");
    s.wire(compare, "out", multi, "trigger");
    s.wire(clock, "out", single, "trigger");
    s.wire(compare, "out", hold, "trigger");
    s.wire(noise, "out", hold, "in");
    s.wire(hold, "out", lag, "in");
    s.wire(lag, "out", chorus, "rate");
    s.wire(sine, "out", follower, "in");
    s.wire(sine, "out", distortion, "in");
    s.wire(automation, "out", distortion, "mix");
    s.wire(distortion, "out", crusher, "in");
    s.wire(crusher, "out", chorus, "in");
    s.wire(chorus, "out", phaser, "in");
    s.wire(phaser, "out", vca, "in");
    s.wire(multi, "out", vca, "level");
    s.wire(follower, "out", phaser, "depth");
    s.wire(vca, "out", output, "in");
    let points = |n: i64| {
        (0..n)
            .map(|i| AutomationPoint {
                tick: Tick(i * 480),
                value: (i % 2) as f32,
                curve: Curve::Linear,
            })
            .collect()
    };
    let lane = s.project.new_lane_id();
    s.edit(Command::AddLane {
        id: lane,
        lane: AutomationLane::new(Endpoint::new(automation, "value"), points(6)),
    });
    s.update();

    let transport = s.controller.transport();
    transport.play();
    let mut out = vec![0.0; 1000 * SETTINGS.channels];
    let mut heard = false;
    for round in 0..16 {
        for (node, key, value) in [
            (distortion, "drive", 6.0 + round as f32),
            (crusher, "bits", 2.0 + round as f32 * 0.5),
            (phaser, "stages", (round % 5) as f32),
            (multi, "length", 0.05 + 0.01 * round as f32),
            (hold, "mode", (round % 2) as f32),
        ] {
            s.controller.set_param(node, key, value);
        }
        transport.seek(Tick(300 * round));
        match round {
            // Editing the curve rebuilds the envelope off the audio thread.
            4 => {
                s.edit(Command::SetConfig {
                    node: multi,
                    key: "curve".into(),
                    value: Some(Value::Text("0 0.2 0;0.5 1 -0.5;1 0 0".into())),
                });
                s.update();
            }
            // Single voice changes the output's shape.
            8 => {
                s.edit(Command::SetConfig {
                    node: multi,
                    key: "single".into(),
                    value: Some(Value::Int(1)),
                });
                s.update();
            }
            12 => {
                s.edit(Command::SetLane {
                    id: lane,
                    lane: AutomationLane::new(Endpoint::new(automation, "value"), points(9)),
                });
                s.update();
            }
            _ => {}
        }
        s.controller.maintain();
        let violations = realtime(|| {
            for _ in 0..4 {
                s.processor.process(&mut out);
                heard |= out.iter().any(|&x| x.abs() > 1e-3);
            }
        });
        assert_eq!(violations, 0, "allocated in round {round}");
        assert!(out.iter().all(|x| x.is_finite()), "round {round}");
    }
    assert!(heard, "the patch never sounded");
}
