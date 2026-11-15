//! The Wavetable oscillator, rendered through the engine.

use std::f32::consts::TAU;

use noodle_core::{Command, Config, Connection, Endpoint, Node, NodeId, Project, Value};
use noodle_engine::{OUTPUT_ID, Registry, Settings, render_project};
use noodle_io::Audio;
use noodle_nodes::{CYCLE_KEY, SampleStore, TABLE_KEY, WAVETABLE_ID, register_library};
use realfft::RealFftPlanner;

const RATE: f32 = 48_000.0;
const SETTINGS: Settings = Settings {
    sample_rate: RATE,
    channels: 1,
    max_frames: 128,
};

struct Rig {
    registry: Registry,
    store: SampleStore,
}

impl Rig {
    fn new() -> Self {
        let mut registry = Registry::with_builtins();
        let store = register_library(&mut registry).samples;
        Self { registry, store }
    }

    fn table(&self, samples: Vec<f32>) -> String {
        self.store.insert(&Audio {
            samples,
            channels: 1,
            sample_rate: 48_000,
        })
    }

    /// Plays a wavetable node (with `position` set) and renders it.
    fn render(
        &self,
        table: &str,
        cycle: i64,
        frequency: f32,
        position: f32,
        frames: usize,
    ) -> Vec<f32> {
        let mut project = Project::new();
        let node = Node::new(WAVETABLE_ID)
            .with_config(
                Config::new()
                    .with(TABLE_KEY, Value::Text(table.into()))
                    .with(CYCLE_KEY, Value::Int(cycle)),
            )
            .with_param("frequency", frequency)
            .with_param("position", position);
        self.play(&mut project, node, frames)
    }

    fn play(&self, project: &mut Project, node: Node, frames: usize) -> Vec<f32> {
        let osc = add(project, node);
        let out = add(project, Node::new(OUTPUT_ID));
        wire(project, osc, out);
        let render = render_project(project, &self.registry, SETTINGS, frames).unwrap();
        assert!(render.diagnostics.is_empty(), "{:?}", render.diagnostics);
        render.samples
    }
}

fn add(project: &mut Project, node: Node) -> NodeId {
    let id = project.new_node_id();
    Command::AddNode { id, node }.apply(project).unwrap();
    id
}

fn wire(project: &mut Project, from: NodeId, to: NodeId) {
    Command::Connect(Connection {
        from: Endpoint::new(from, "out"),
        to: Endpoint::new(to, "in"),
    })
    .apply(project)
    .unwrap();
}

fn sine_cycle(n: usize) -> Vec<f32> {
    (0..n).map(|i| (i as f32 / n as f32 * TAU).sin()).collect()
}

#[test]
fn a_sine_table_plays_like_the_sine_oscillator() {
    let rig = Rig::new();
    let table = rig.table(sine_cycle(2048));
    let got = rig.render(&table, 2048, 440.0, 0.0, 4800);
    let mut project = Project::new();
    let want = rig.play(
        &mut project,
        Node::new("noodle.osc.sine").with_param("frequency", 440.0),
        4800,
    );
    let worst = got
        .iter()
        .zip(&want)
        .map(|(a, b)| (a - b).abs())
        .fold(0.0, f32::max);
    assert!(worst < 2e-3, "{worst}");
    assert!(got.iter().any(|x| x.abs() > 0.9));
}

/// The share of a block's power that isn't at one of `harmonics` (in Hz) or
/// next to it.
fn off_harmonic_share(signal: &[f32], harmonics: &[f32]) -> f32 {
    let n = signal.len();
    let mut windowed: Vec<f32> = signal
        .iter()
        .enumerate()
        .map(|(i, x)| x * (0.5 - 0.5 * (TAU * i as f32 / n as f32).cos()))
        .collect();
    let fft = RealFftPlanner::<f32>::new().plan_fft_forward(n);
    let mut spectrum = fft.make_output_vec();
    fft.process(&mut windowed, &mut spectrum).unwrap();
    let bin_hz = RATE / n as f32;
    let (mut total, mut off) = (0.0f32, 0.0f32);
    for (k, bin) in spectrum.iter().enumerate().skip(2) {
        let power = bin.norm_sqr();
        total += power;
        let near = harmonics
            .iter()
            .any(|h| (k as f32 * bin_hz - h).abs() < 5.0 * bin_hz);
        if !near {
            off += power;
        }
    }
    off / total
}

#[test]
fn a_saw_table_at_5_khz_does_not_alias() {
    let rig = Rig::new();
    let n = 2048;
    // Every harmonic of a saw, as a table.
    let saw: Vec<f32> = (0..n).map(|i| 2.0 * i as f32 / n as f32 - 1.0).collect();
    let table = rig.table(saw);
    let out = rig.render(&table, n as i64, 5000.0, 0.0, 8192 + 4096);
    let share = off_harmonic_share(&out[4096..], &[5000.0, 10_000.0, 15_000.0, 20_000.0]);
    // -40 dB. The fifth harmonic folded back to 23 kHz alone is about -15 dB.
    assert!(share < 1e-4, "{share}");
}

#[test]
fn position_moves_through_the_stack_and_crossfades() {
    let rig = Rig::new();
    let n = 512;
    let mut data = sine_cycle(n);
    data.extend(sine_cycle(n).iter().map(|x| x * 0.5));
    data.extend(vec![0.0; n]);
    let table = rig.table(data);
    let peak = |position: f32| {
        rig.render(&table, n as i64, 440.0, position, 2400)
            .iter()
            .fold(0.0f32, |m, x| m.max(x.abs()))
    };
    assert!((peak(0.0) - 1.0).abs() < 0.01);
    assert!((peak(0.5) - 0.5).abs() < 0.01);
    assert!(peak(1.0) < 0.01);
    assert!((peak(0.25) - 0.75).abs() < 0.01);
}

#[test]
fn a_missing_table_is_silent() {
    let rig = Rig::new();
    let out = rig.render("0123abcd", 2048, 440.0, 0.0, 1000);
    assert!(out.iter().all(|&x| x == 0.0));
}
