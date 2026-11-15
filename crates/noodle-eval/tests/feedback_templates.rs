//! The Comb and Flanger templates are loops of Delay as short as the pitch or
//! sweep asks: the comb rings at its frequency, and the flanger sweeps a
//! delay under a millisecond.

use std::path::Path;

use noodle_core::library::Library;
use noodle_core::{Command, Connection, Endpoint, History, Node, NodeId, Position, Project};
use noodle_engine::{OUTPUT_ID, Registry, Settings};

const RATE: f32 = 48_000.0;
const SETTINGS: Settings = Settings {
    sample_rate: RATE,
    max_frames: 128,
    channels: 1,
};

/// `source` through the template `slug`, with `set` put on the node of type
/// `kind` inside it that already holds the first key, to the output. (An
/// unwired group input leaves the value inside as it is.)
fn render(slug: &str, source: Node, set: &[(&str, f32)], kind: &str, frames: usize) -> Vec<f32> {
    let library = Library::in_memory();
    let (mut project, mut history) = (Project::new(), History::new());
    let template = library
        .template(&format!("system:template:{slug}"))
        .expect("the template is in the library");
    let (group, command) =
        template.instantiate(slug, None, Position::default(), || project.new_node_id());
    history.apply(&mut project, command).unwrap();
    let add = |project: &mut Project, history: &mut History, node: Node| -> NodeId {
        let id = project.new_node_id();
        history
            .apply(project, Command::AddNode { id, node })
            .unwrap();
        id
    };
    let src = add(&mut project, &mut history, source);
    let out = add(&mut project, &mut history, Node::new(OUTPUT_ID));
    for (from, from_port, to, to_port) in [(src, "out", group, "in"), (group, "out", out, "in")] {
        history
            .apply(
                &mut project,
                Command::Connect(Connection {
                    from: Endpoint::new(from, from_port),
                    to: Endpoint::new(to, to_port),
                }),
            )
            .unwrap();
    }
    if let Some(&(first, _)) = set.first() {
        let inner = project
            .graph()
            .children(Some(group))
            .find(|(_, n)| n.type_id == kind && n.params.contains_key(first))
            .expect("the group has the node")
            .0;
        for &(key, value) in set {
            history
                .apply(
                    &mut project,
                    Command::SetParam {
                        node: inner,
                        key: key.into(),
                        value: Some(value),
                    },
                )
                .unwrap();
        }
    }
    let mut registry = Registry::with_builtins();
    let out = noodle_nodes::render_project_with_clips(
        &project,
        &mut registry,
        Path::new("."),
        SETTINGS,
        frames,
    )
    .unwrap();
    assert!(out.problems.is_empty(), "{:?}", out.problems);
    assert!(
        out.render.diagnostics.is_empty(),
        "{:?}",
        out.render.diagnostics
    );
    out.render.samples
}

/// Noise: through a comb it comes out with a period of the comb's delay.
fn click() -> Node {
    Node::new("noodle.noise.white")
}

/// The lag, in samples, of the strongest autocorrelation peak in `lo..hi`.
fn period(samples: &[f32], lo: usize, hi: usize) -> usize {
    let n = samples.len() - hi;
    (lo..=hi)
        .max_by(|&a, &b| {
            let sum = |lag: usize| -> f32 { (0..n).map(|i| samples[i] * samples[i + lag]).sum() };
            sum(a).total_cmp(&sum(b))
        })
        .unwrap()
}

#[test]
fn a_comb_rings_at_its_frequency() {
    // The template's own frequency is 220 Hz: 218.2 samples.
    let samples = render("comb", click(), &[], "", 12_100);
    let heard = period(&samples[100..12_000], 150, 300);
    assert!(
        (heard as f32 - 218.2).abs() < 1.5,
        "period {heard}, wanted 218"
    );
}

#[test]
fn a_comb_follows_its_frequency_input() {
    for frequency in [110.0f32, 440.0, 1000.0, 1760.0] {
        let samples = render(
            "comb",
            click(),
            &[("a", frequency)],
            "noodle.util.math",
            12_100,
        );
        let expected = RATE / frequency;
        let heard = period(
            &samples[100..12_000],
            (expected * 0.6) as usize,
            (expected * 1.6) as usize,
        );
        assert!(
            (heard as f32 - expected).abs() < 2.0,
            "{frequency} Hz rang at a period of {heard}, wanted {expected}"
        );
    }
}

#[test]
fn a_flanger_sounds_and_stays_finite() {
    let noise = Node::new("noodle.noise.white");
    let dry = render(
        "flanger",
        noise.clone(),
        &[("gain2", -60.0)],
        "noodle.util.mix",
        24_000,
    );
    let wet = render("flanger", noise, &[], "", 24_000);
    assert!(wet.iter().all(|x| x.is_finite()));
    assert!(wet.iter().fold(0.0f32, |m, x| m.max(x.abs())) > 0.05);
    let differ = dry
        .iter()
        .zip(&wet)
        .filter(|(a, b)| (*a - *b).abs() > 0.01)
        .count();
    assert!(differ > 1_000, "the flange changes the sound");
}
