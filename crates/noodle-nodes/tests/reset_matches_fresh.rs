//! `Node::reset` must return a node to the state a newly built instance has
//! (ADR 0010: the playback cache renders a pass from a fresh state at its
//! start, and a seek on a running plan must sound the same).
//!
//! For every registered node type: run it for a while with input and notes,
//! seek to S (move the playhead and reset), and compare what it plays next
//! with a new instance built at S.

use noodle_core::Config;
use noodle_engine::testing::Harness;
use noodle_engine::{Event, EventKind, InputKind, NodeType, NoteId, Registry, Shape};
use noodle_nodes::register_library;

const BLOCK: usize = 128;
/// A second and a half: past the first pass through every delay line.
const BLOCKS: usize = 560;
const SEEK_TO: u64 = 96_000;
const RATE: f32 = 48_000.0;

fn default_config(node_type: &dyn NodeType) -> Config {
    let mut config = Config::new();
    for info in node_type.config() {
        config.set(info.key, info.default.clone());
    }
    config
}

/// Deterministic noise, so every run feeds the same input.
fn fill(rng: &mut u32, out: &mut [f32]) {
    for sample in out {
        *rng = rng.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        *sample = (*rng >> 8) as f32 / (1u32 << 23) as f32 - 1.0;
    }
}

fn notes() -> [Event; 2] {
    [
        Event {
            time: 3,
            kind: EventKind::NoteOn {
                note: NoteId(1),
                channel: 0,
                key: 60,
                velocity: 0.8,
            },
        },
        Event {
            time: 40,
            kind: EventKind::NoteOn {
                note: NoteId(2),
                channel: 0,
                key: 67,
                velocity: 0.6,
            },
        },
    ]
}

/// Runs `BLOCKS` blocks and returns everything every output wrote.
fn play(
    harness: &mut Harness,
    node_type: &dyn NodeType,
    config: &Config,
    rng: &mut u32,
) -> (Vec<f32>, String) {
    let layout = node_type.layout(config).unwrap();
    let mut heard = Vec::new();
    let mut events = String::new();
    for block in 0..BLOCKS {
        for port in 0..layout.inputs.len() {
            let mut buffer = vec![0.0; BLOCK];
            fill(rng, &mut buffer);
            if harness.is_connected(port) {
                harness
                    .input(port, BLOCK)
                    .lane_mut(0, 0)
                    .copy_from_slice(&buffer);
            }
        }
        for port in 0..layout.event_inputs.len() {
            let sent = if block == 0 {
                notes().to_vec()
            } else {
                Vec::new()
            };
            harness.send_events(port, &sent);
        }
        harness.run(BLOCK).unwrap();
        for port in 0..layout.event_outputs.len() {
            events.push_str(&format!("{:?}", harness.events(port)));
        }
        for port in 0..layout.outputs.len() {
            let out = harness.output(port);
            for voice in 0..out.shape().voices {
                for channel in 0..out.shape().channels {
                    heard.extend_from_slice(out.lane(voice, channel));
                }
            }
        }
    }
    (heard, events)
}

/// Where the unconnected parameters sit.
#[derive(Clone, Copy, Debug)]
enum Params {
    Default,
    Middle,
    Max,
}

fn build(
    node_type: &dyn NodeType,
    config: &Config,
    params: Params,
    position: u64,
) -> Option<Harness> {
    let layout = node_type.layout(config).ok()?;
    if !layout.spectral_inputs.is_empty() || !layout.sample_inputs.is_empty() {
        return None;
    }
    let connected: Vec<(usize, Shape)> = (0..layout.inputs.len())
        .filter(|&p| layout.inputs[p].kind == InputKind::Audio)
        .map(|p| (p, Shape::MONO))
        .collect();
    let mut harness =
        Harness::starting_at(node_type, config, &connected, RATE, BLOCK, position).ok()?;
    for (port, input) in layout.inputs.iter().enumerate() {
        if let InputKind::Param(info) = &input.kind {
            match params {
                Params::Default => {}
                Params::Middle => harness.set(port, 0.5 * (info.min + info.max)),
                Params::Max => harness.set(port, info.max),
            }
        }
    }
    Some(harness)
}

#[test]
fn a_reset_node_sounds_like_a_new_one() {
    let mut registry = Registry::with_builtins();
    let _library = register_library(&mut registry);
    let mut failures = Vec::new();
    let mut checked = 0;
    for node_type in registry.iter() {
        let id = node_type.info().id;
        let config = default_config(node_type);
        for params in [Params::Default, Params::Middle, Params::Max] {
            let Some(mut used) = build(node_type, &config, params, 0) else {
                eprintln!("skipped {id}");
                continue;
            };
            let Some(mut fresh) = build(node_type, &config, params, SEEK_TO) else {
                continue;
            };
            let mut rng = 1;
            play(&mut used, node_type, &config, &mut rng);
            used.seek(SEEK_TO);
            let mut rng = 7;
            let after_reset = play(&mut used, node_type, &config, &mut rng);
            let mut rng = 7;
            let from_new = play(&mut fresh, node_type, &config, &mut rng);
            checked += 1;
            let same = after_reset.1 == from_new.1
                && after_reset.0.len() == from_new.0.len()
                && after_reset
                    .0
                    .iter()
                    .zip(&from_new.0)
                    .all(|(a, b)| a.to_bits() == b.to_bits() || (a.is_nan() && b.is_nan()));
            if !same {
                failures.push(format!("{id} (parameters: {params:?})"));
            }
        }
    }
    assert!(checked > 40, "only {checked} node runs were checked");
    assert!(
        failures.is_empty(),
        "reset left state behind in: {failures:#?}"
    );
}
