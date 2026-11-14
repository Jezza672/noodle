//! The library's spectral and convolution effects work as added to a project:
//! each template is a group of primitives that makes the sound it names.

use std::path::Path;

use noodle_core::library::Library;
use noodle_core::{Command, Connection, Endpoint, History, Node, NodeId, Position, Project};
use noodle_engine::{OUTPUT_ID, Registry, Settings};

const RATE: f32 = 48_000.0;
const SETTINGS: Settings = Settings {
    sample_rate: RATE,
    max_frames: 256,
    channels: 1,
};

/// The sources wired to the group's inputs (`in` and so on), then the settings
/// put on the first node of each type inside the group, rendered to the
/// output for `frames` frames.
fn render(
    slug: &str,
    sources: Vec<(Node, &str)>,
    set: &[(&str, &str, f32)],
    frames: usize,
) -> Vec<f32> {
    let library = Library::in_memory();
    let (mut project, mut history) = (Project::new(), History::new());
    let template = library
        .template(&format!("system:template:{slug}"))
        .expect("the template is in the library");
    let (group, command) =
        template.instantiate(slug, None, Position::default(), || project.new_node_id());
    history.apply(&mut project, command).unwrap();
    let add = |project: &mut Project, history: &mut History, node: Node| {
        let id = project.new_node_id();
        history
            .apply(project, Command::AddNode { id, node })
            .unwrap();
        id
    };
    let out = add(&mut project, &mut history, Node::new(OUTPUT_ID));
    for (node, port) in sources {
        let id = add(&mut project, &mut history, node);
        history
            .apply(
                &mut project,
                Command::Connect(Connection {
                    from: Endpoint::new(id, "out"),
                    to: Endpoint::new(group, port),
                }),
            )
            .unwrap();
    }
    history
        .apply(
            &mut project,
            Command::Connect(Connection {
                from: Endpoint::new(group, "out"),
                to: Endpoint::new(out, "in"),
            }),
        )
        .unwrap();
    for &(kind, key, value) in set {
        let inside: NodeId = project
            .graph()
            .children(Some(group))
            .find(|(_, n)| n.type_id == kind)
            .unwrap_or_else(|| panic!("the group has a {kind}"))
            .0;
        history
            .apply(
                &mut project,
                Command::SetParam {
                    node: inside,
                    key: key.into(),
                    value: Some(value),
                },
            )
            .unwrap();
    }
    let mut registry = Registry::with_builtins();
    let rendered = noodle_nodes::render_project_with_clips(
        &project,
        &mut registry,
        Path::new("."),
        SETTINGS,
        frames,
    )
    .unwrap();
    assert!(rendered.problems.is_empty(), "{:?}", rendered.problems);
    rendered.render.samples
}

fn sine(hz: f32) -> Node {
    Node::new("noodle.osc.sine").with_param("frequency", hz)
}

fn rms(x: &[f32]) -> f32 {
    (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt()
}

fn frequency(samples: &[f32]) -> f32 {
    let crossings: Vec<f32> = (1..samples.len())
        .filter(|&i| samples[i - 1] < 0.0 && samples[i] >= 0.0)
        .map(|i| i as f32 - samples[i] / (samples[i] - samples[i - 1]))
        .collect();
    assert!(
        crossings.len() > 8,
        "no tone: {} crossings",
        crossings.len()
    );
    (crossings.len() - 1) as f32 * RATE / (crossings[crossings.len() - 1] - crossings[0])
}

#[test]
fn the_pitch_shifter_moves_a_tone_an_octave() {
    let out = render("pitch-shifter", vec![(sine(300.0), "in")], &[], 40_000);
    let got = frequency(&out[12_000..]);
    assert!((got - 600.0).abs() < 6.0, "{got}");
}

#[test]
fn the_pitch_shifter_goes_down_with_its_semitones_control() {
    let out = render(
        "pitch-shifter",
        vec![(sine(600.0), "in")],
        &[("noodle.spectral.pitch", "shift", -12.0)],
        40_000,
    );
    let got = frequency(&out[12_000..]);
    assert!((got - 300.0).abs() < 3.0, "{got}");
}

#[test]
fn the_time_stretch_keeps_the_pitch() {
    let out = render("time-stretch", vec![(sine(300.0), "in")], &[], 60_000);
    let got = frequency(&out[12_000..]);
    assert!((got - 300.0).abs() < 2.0, "{got}");
    assert!(rms(&out[12_000..]) > 0.4);
}

#[test]
fn the_spectral_compressor_turns_a_loud_tone_down() {
    // A full-scale tone is far over the threshold of -40 dB per bin.
    let loud = render(
        "spectral-compressor",
        vec![(sine(1000.0), "in")],
        &[],
        40_000,
    );
    assert!(rms(&loud[16_000..]) < 0.2, "{}", rms(&loud[16_000..]));
    // With the ratio at 1 it is left alone.
    let flat = render(
        "spectral-compressor",
        vec![(sine(1000.0), "in")],
        &[("noodle.spectral.compress", "ratio", 1.0)],
        40_000,
    );
    assert!((rms(&flat[16_000..]) - 0.707).abs() < 0.05);
}

#[test]
fn the_denoiser_takes_out_noise_it_is_shown() {
    // Two separate noise sources: one is the sound, one is the profile.
    let noise = || Node::new("noodle.noise.white");
    let plain = render("denoiser", vec![(noise(), "in")], &[], 72_000);
    let cleaned = render(
        "denoiser",
        vec![(noise(), "in"), (noise(), "noise")],
        &[],
        72_000,
    );
    // Nothing learned: the sound passes. Shown the noise: it is much quieter.
    assert!(rms(&plain[24_000..]) > 0.1, "{}", rms(&plain[24_000..]));
    assert!(
        rms(&cleaned[24_000..]) < rms(&plain[24_000..]) * 0.3,
        "{} -> {}",
        rms(&plain[24_000..]),
        rms(&cleaned[24_000..])
    );
}

#[test]
fn the_convolution_reverb_rings_on_after_a_click() {
    let click = Node::new("noodle.generator.clock")
        .with_param("rate", 0.5)
        .with_param("width", 0.001);
    let dry_only = render(
        "convolution-reverb",
        vec![(click.clone(), "in")],
        &[("noodle.effect.convolver", "gain", -24.0)],
        48_000,
    );
    let wet = render("convolution-reverb", vec![(click, "in")], &[], 48_000);
    // The click is at the start in both (the dry path is lined up with the
    // convolver's delay); the room is the difference.
    let tail = |x: &[f32]| rms(&x[2_400..24_000]);
    assert!(
        tail(&wet) > 4.0 * tail(&dry_only),
        "{} {}",
        tail(&wet),
        tail(&dry_only)
    );
    // It dies away: the last quarter second is quieter than the first.
    assert!(rms(&wet[40_000..48_000]) < rms(&wet[2_400..10_400]) * 0.5);
}
