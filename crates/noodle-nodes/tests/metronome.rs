//! The metronome, as the transport bar's button sets it up: a button wired
//! into a metronome wired to the output, rendered through the whole engine.

use noodle_core::{
    Command, Connection, Endpoint, History, Node, NodeId, Project, TempoMap, TimeSignature,
};
use noodle_engine::{OUTPUT_ID, Registry, Settings, render_project};
use noodle_nodes::{BUTTON_ID, BUTTON_STATE, METRONOME_ID, METRONOME_ON, register_library};

const RATE: u32 = 48_000;
const SETTINGS: Settings = Settings {
    sample_rate: RATE as f32,
    max_frames: 512,
    channels: 2,
};

fn project(button_on: bool, tempo: Option<TempoMap>) -> Project {
    let mut project = Project::new();
    let mut history = History::new();
    let (button, metronome, out) = (NodeId(1), NodeId(2), NodeId(3));
    let mut apply = |command| history.apply(&mut project, command).unwrap();
    apply(Command::AddNode {
        id: button,
        node: Node::new(BUTTON_ID).with_param(BUTTON_STATE, f32::from(button_on)),
    });
    apply(Command::AddNode {
        id: metronome,
        node: Node::new(METRONOME_ID),
    });
    apply(Command::AddNode {
        id: out,
        node: Node::new(OUTPUT_ID),
    });
    for (from, from_port, to, to_port) in [
        (button, "out", metronome, METRONOME_ON),
        (metronome, "out", out, "in"),
    ] {
        apply(Command::Connect(Connection {
            from: Endpoint::new(from, from_port),
            to: Endpoint::new(to, to_port),
        }));
    }
    if let Some(map) = tempo {
        apply(Command::SetTempoMap(map));
    }
    project
}

fn render(project: &Project, frames: usize) -> Vec<f32> {
    let mut registry = Registry::with_builtins();
    let _library = register_library(&mut registry);
    let render = render_project(project, &registry, SETTINGS, frames).unwrap();
    assert!(render.diagnostics.is_empty(), "{:?}", render.diagnostics);
    // The left channel.
    render.samples.iter().step_by(2).copied().collect()
}

/// The frames where a click starts.
fn onsets(samples: &[f32]) -> Vec<usize> {
    let mut found = Vec::new();
    let mut last = None::<usize>;
    for (i, x) in samples.iter().enumerate() {
        if x.abs() > 1e-3 {
            if last.is_none_or(|last| i - last > 2_000) {
                found.push(i);
            }
            last = Some(i);
        }
    }
    found
}

#[test]
fn a_pressed_button_gives_a_click_on_every_beat() {
    let samples = render(&project(true, None), 4 * 24_000);
    let clicks = onsets(&samples);
    assert_eq!(clicks.len(), 4, "{clicks:?}");
    for (n, at) in clicks.into_iter().enumerate() {
        assert!(at.abs_diff(n * 24_000) <= 16, "click {n} at {at}");
    }
}

#[test]
fn the_clicks_follow_the_tempo_map() {
    let map = TempoMap::constant(60.0, TimeSignature::COMMON).unwrap();
    let samples = render(&project(true, Some(map)), 3 * 48_000);
    let clicks = onsets(&samples);
    assert_eq!(clicks.len(), 3, "{clicks:?}");
    for (n, at) in clicks.into_iter().enumerate() {
        assert!(at.abs_diff(n * 48_000) <= 16, "click {n} at {at}");
    }
}

#[test]
fn a_button_that_is_off_keeps_the_metronome_silent() {
    let samples = render(&project(false, None), 4 * 24_000);
    assert!(samples.iter().all(|&x| x == 0.0));
}
