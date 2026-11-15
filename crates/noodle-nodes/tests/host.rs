//! The host backend: Noodle run from another program's audio callback. A
//! plugin's `process` is another caller of the engine, so these tests are
//! that caller: odd block sizes against an offline render, host audio, notes
//! and parameters on the frames the host gave, and the host's transport and
//! tempo.

use noodle_core::{Command, Config, Connection, Endpoint, Node, NodeId, Project, Value};
use noodle_engine::{
    Controller, HOST_DEVICE, INPUT_DEVICE_KEY, INPUT_ID, OUTPUT_DEVICE_KEY, OUTPUT_ID, Registry,
    Settings, render_project,
};
use noodle_io::{
    HostBackend, HostConfig, HostEvents, HostLink, HostMidi, HostParamChange, HostTransport,
    host_engine,
};
use noodle_nodes::{HOST_PARAM_ID, HOST_PARAM_SLOT_KEY, MIDI_IN_ID, MIDI_OUT_ID, register_library};

const RATE: f32 = 48_000.0;

struct Rig {
    project: Project,
    registry: Registry,
    library: noodle_nodes::Library,
}

impl Rig {
    fn new() -> Self {
        let mut registry = Registry::with_builtins();
        let library = register_library(&mut registry);
        Self {
            project: Project::new(),
            registry,
            library,
        }
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

    fn text(key: &str, value: &str) -> Config {
        let mut config = Config::new();
        config.set(key, Value::Text(value.to_owned()));
        config
    }

    fn host_out(&mut self) -> NodeId {
        self.add(Node::new(OUTPUT_ID).with_config(Self::text(OUTPUT_DEVICE_KEY, HOST_DEVICE)))
    }

    fn host_in(&mut self) -> NodeId {
        self.add(Node::new(INPUT_ID).with_config(Self::text(INPUT_DEVICE_KEY, HOST_DEVICE)))
    }

    fn host_midi(&mut self, type_id: &str) -> NodeId {
        self.add(Node::new(type_id).with_config(Self::text("port", HOST_DEVICE)))
    }

    /// The engine for the host, with the project compiled into it.
    fn start(&self, in_channels: usize) -> (Controller, HostBackend) {
        let (mut controller, backend) = host_engine(HostConfig {
            sample_rate: RATE,
            out_channels: 2,
            in_channels,
            block: 128,
            midi: self.library.midi.clone(),
            midi_out: self.library.midi_out.clone(),
            link: self.library.host.clone(),
            workers: 0,
        })
        .unwrap();
        let diagnostics = controller.update_project(&self.project, &self.registry);
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        (controller, backend)
    }
}

/// What a host does with one call: planar buffers in, planar buffers out.
struct Call {
    out: [Vec<f32>; 2],
    midi: Vec<HostMidi>,
}

fn playing(beats: Option<f64>, tempo: f64) -> HostTransport {
    HostTransport {
        playing: true,
        tempo: Some(tempo),
        beats,
    }
}

fn run(
    backend: &mut HostBackend,
    frames: usize,
    input: &[&[f32]],
    midi: &[HostMidi],
    params: &[HostParamChange],
    transport: HostTransport,
) -> Call {
    let mut out = [vec![9.0; frames], vec![9.0; frames]];
    let mut sent = Vec::new();
    let [left, right] = &mut out;
    backend.process(
        frames,
        input,
        &mut [left.as_mut_slice(), right.as_mut_slice()],
        HostEvents {
            midi,
            params,
            transport,
        },
        &mut |message| sent.push(message),
    );
    Call { out, midi: sent }
}

fn note(offset: u32, on: bool, key: u8) -> HostMidi {
    HostMidi {
        offset,
        bytes: [if on { 0x90 } else { 0x80 }, key, if on { 100 } else { 0 }],
    }
}

/// Sine → filter → gain → Host Out: a graph with no input, so an offline
/// render of the same project is the reference.
fn tone() -> Rig {
    let mut rig = Rig::new();
    let sine = rig.add(Node::new("noodle.osc.sine").with_param("frequency", 220.0));
    let svf = rig.add(Node::new("noodle.filter.svf").with_param("cutoff", 900.0));
    let gain = rig.add(Node::new("noodle.util.gain").with_param("gain", 0.5));
    let out = rig.host_out();
    rig.wire(sine, "out", svf, "in");
    rig.wire(svf, "low", gain, "in");
    rig.wire(gain, "out", out, "in");
    rig
}

#[test]
fn odd_block_sizes_match_an_offline_render() {
    let rig = tone();
    let total = 9_000;
    let reference = render_project(
        &rig.project,
        &rig.registry,
        Settings {
            sample_rate: RATE,
            max_frames: 128,
            channels: 2,
        },
        total,
    )
    .unwrap();
    assert!(reference.samples.iter().any(|&x| x.abs() > 0.05), "silent");
    for sizes in [
        vec![1],
        vec![37],
        vec![128],
        vec![4096],
        vec![1, 37, 4096, 255],
    ] {
        let (_controller, mut backend) = rig.start(0);
        let mut heard = Vec::new();
        let mut round = 0;
        while heard.len() < total * 2 {
            let frames = sizes[round % sizes.len()].min(total - heard.len() / 2);
            round += 1;
            let call = run(&mut backend, frames, &[], &[], &[], playing(None, 120.0));
            for frame in 0..frames {
                heard.push(call.out[0][frame]);
                heard.push(call.out[1][frame]);
            }
        }
        let worst = heard
            .iter()
            .zip(&reference.samples)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0, f32::max);
        assert!(worst < 1e-4, "block sizes {sizes:?} differ by {worst}");
    }
}

#[test]
fn host_input_reaches_host_out_and_channels_are_matched() {
    let mut rig = Rig::new();
    let input = rig.host_in();
    let out = rig.host_out();
    rig.wire(input, "out", out, "in");
    let (_controller, mut backend) = rig.start(2);
    let left: Vec<f32> = (0..300).map(|i| (i as f32 * 0.01).sin() * 0.5).collect();
    let right: Vec<f32> = left.iter().map(|x| -x * 0.5).collect();
    let call = run(
        &mut backend,
        300,
        &[&left, &right],
        &[],
        &[],
        playing(None, 120.0),
    );
    assert_eq!(call.out[0], left);
    assert_eq!(call.out[1], right);
    // Not-a-number from the host is silence, not poison.
    let mut bad = left.clone();
    bad[10] = f32::NAN;
    let call = run(
        &mut backend,
        300,
        &[&bad, &right],
        &[],
        &[],
        playing(None, 120.0),
    );
    assert_eq!(call.out[0][10], 0.0);
    assert_eq!(call.out[0][11], left[11]);
}

#[test]
fn host_in_is_silent_when_a_device_drives_the_engine() {
    // The same project, on an engine whose main bus is a sound card's.
    let mut rig = Rig::new();
    let input = rig.host_in();
    let out = rig.host_out();
    rig.wire(input, "out", out, "in");
    let (mut controller, mut processor) = noodle_engine::engine(Settings {
        sample_rate: RATE,
        max_frames: 128,
        channels: 2,
    })
    .unwrap();
    controller
        .set_buses(vec![noodle_engine::Bus {
            device: "card".into(),
            channels: 2,
        }])
        .unwrap();
    controller.update_project(&rig.project, &rig.registry);
    let input = vec![0.5f32; 128 * 2];
    let mut out = vec![0.0f32; 128 * 2];
    for _ in 0..4 {
        processor.process_with_input(&input, 2, &mut out);
    }
    assert!(out.iter().all(|&x| x == 0.0), "Host In heard a sound card");
}

/// Host MIDI In → Host MIDI Out: what goes in comes out, on its frame.
#[test]
fn notes_land_on_the_frame_the_host_gave_and_leave_on_it() {
    let mut rig = Rig::new();
    let midi_in = rig.host_midi(MIDI_IN_ID);
    let midi_out = rig.host_midi(MIDI_OUT_ID);
    rig.wire(midi_in, "out", midi_out, "in");
    // A MIDI Out with nothing on the audio side still needs an output to run
    // the graph; an empty Host Out does.
    rig.host_out();
    for sizes in [vec![512], vec![37], vec![1, 200, 3]] {
        let (_controller, mut backend) = rig.start(0);
        // A note at frame 37 and its end at frame 300 of a 512 frame stretch.
        let notes = [note(37, true, 60), note(300, false, 60)];
        let mut sent = Vec::new();
        let (mut at, mut round) = (0usize, 0);
        while at < 512 {
            let frames = sizes[round % sizes.len()].min(512 - at);
            round += 1;
            let local: Vec<HostMidi> = notes
                .iter()
                .filter(|m| (at..at + frames).contains(&(m.offset as usize)))
                .map(|m| HostMidi {
                    offset: m.offset - at as u32,
                    bytes: m.bytes,
                })
                .collect();
            let call = run(&mut backend, frames, &[], &local, &[], playing(None, 120.0));
            sent.extend(
                call.midi
                    .iter()
                    .map(|m| (at as u32 + m.offset, m.bytes[..2].to_vec())),
            );
            at += frames;
        }
        assert_eq!(
            sent,
            vec![(37, vec![0x90, 60]), (300, vec![0x80, 60])],
            "block sizes {sizes:?}"
        );
    }
}

#[test]
fn a_parameter_change_lands_on_its_frame_and_a_slot_reads_only_its_own() {
    let mut rig = Rig::new();
    let param = rig.add(Node::new(HOST_PARAM_ID).with_config({
        let mut config = Config::new();
        config.set(HOST_PARAM_SLOT_KEY, Value::Int(3));
        config
    }));
    let out = rig.host_out();
    rig.wire(param, "value", out, "in");
    let (_controller, mut backend) = rig.start(0);
    let change = |offset, slot, value| HostParamChange {
        offset,
        slot,
        value,
    };
    let call = run(
        &mut backend,
        300,
        &[],
        &[],
        &[
            change(50, 2, 0.5),
            change(200, 3, 0.9),
            change(250, 2, 0.25),
        ],
        playing(None, 120.0),
    );
    // Slot 3 is the node's (index 2); slot 4 moved at frame 200 and doesn't
    // reach it.
    let heard = &call.out[0];
    assert!(
        heard[..50].iter().all(|&x| x == 0.0),
        "{:?}",
        &heard[45..55]
    );
    assert!(
        heard[50..250].iter().all(|&x| x == 0.5),
        "{:?}",
        &heard[48..52]
    );
    assert!(heard[250..].iter().all(|&x| x == 0.25));
    assert_eq!(backend_link(&rig).value(3), 0.9);
}

fn backend_link(rig: &Rig) -> &HostLink {
    &rig.library.host
}

#[test]
fn the_hosts_tempo_moves_a_tempo_node_at_the_next_block() {
    let mut rig = Rig::new();
    // 1/4 by default: the rate is beats per second.
    let tempo = rig.add(Node::new("noodle.mod.tempo"));
    let out = rig.host_out();
    rig.wire(tempo, "rate", out, "in");
    let (_controller, mut backend) = rig.start(0);
    let mut at = |bpm| {
        let call = run(&mut backend, 128, &[], &[], &[], playing(None, bpm));
        call.out[0][127]
    };
    assert!((at(120.0) - 2.0).abs() < 1e-5);
    assert!((at(90.0) - 1.5).abs() < 1e-5);
    assert!((at(180.0) - 3.0).abs() < 1e-5);
}

#[test]
fn the_playhead_follows_the_hosts_play_stop_and_seeks_to_the_sample() {
    let rig = tone();
    let (_controller, mut backend) = rig.start(0);
    let position = |backend: &HostBackend| backend.transport().position();
    let tempo = 120.0;
    // 120 bpm, 48 kHz: a beat is 24000 samples.
    run(&mut backend, 480, &[], &[], &[], playing(Some(1.0), tempo));
    assert_eq!(position(&backend), 24_000 + 480);
    // Carrying on from where the last block ended is not a seek.
    let next = 1.0 + 480.0 * 2.0 / RATE as f64;
    run(&mut backend, 100, &[], &[], &[], playing(Some(next), tempo));
    assert_eq!(position(&backend), 24_000 + 580);
    // A locate to another bar is.
    run(&mut backend, 100, &[], &[], &[], playing(Some(8.0), tempo));
    assert_eq!(position(&backend), 8 * 24_000 + 100);
    // A loop's wrap is one too.
    run(&mut backend, 64, &[], &[], &[], playing(Some(2.0), tempo));
    assert_eq!(position(&backend), 2 * 24_000 + 64);
    // Stopped, the playhead stays where the host put it.
    let stopped = |beats| HostTransport {
        playing: false,
        tempo: Some(tempo),
        beats: Some(beats),
    };
    run(&mut backend, 200, &[], &[], &[], stopped(3.0));
    run(&mut backend, 200, &[], &[], &[], stopped(3.0));
    assert_eq!(position(&backend), 3 * 24_000);
    // And plays on from there.
    run(&mut backend, 200, &[], &[], &[], playing(Some(3.0), tempo));
    assert_eq!(position(&backend), 3 * 24_000 + 200);
    assert!(backend_link(&rig).playing());
}
