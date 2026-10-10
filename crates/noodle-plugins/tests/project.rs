//! A plugin node in a real project: saved and loaded as RON, and rendered
//! through the same hook that freeze and export use.

mod common;

use base64::Engine as _;
use common::*;
use noodle_core::Project;
use noodle_engine::{Progress, Settings};
use noodle_nodes::{RenderRequest, render_streaming};

const SETTINGS: Settings = Settings {
    sample_rate: 48_000.0,
    max_frames: 512,
    channels: 2,
};

fn project(plugin_params: &str, config: &str) -> Project {
    Project::from_ron(&format!(
        r#"(
    format: 1,
    graph: (
        nodes: {{
            1: (type: "noodle.osc.sine", params: {{"frequency": 440.0}}, position: (x: 0.0, y: 0.0)),
            2: (type: "clap.noodle.test.gain-sine", params: {{{plugin_params}}}, config: {{{config}}}, position: (x: 200.0, y: 0.0)),
            3: (type: "noodle.io.output", position: (x: 400.0, y: 0.0)),
        }},
        connections: [
            (from: (node: 1, port: "out"), to: (node: 2, port: "in")),
            (from: (node: 2, port: "out"), to: (node: 3, port: "in")),
        ],
    ),
)"#
    ))
    .unwrap()
}

fn render(project: &Project, with_plugins: bool) -> (Vec<f32>, usize) {
    let request = RenderRequest {
        project: project.clone(),
        base: std::env::temp_dir(),
        settings: SETTINGS,
        frames: 4_800,
        extend_registry: with_plugins.then(|| catalog().extender()),
    };
    let mut samples = Vec::new();
    let report = render_streaming(&request, &Progress::new(), |chunk| {
        samples.extend_from_slice(chunk);
        Ok::<_, ()>(())
    })
    .unwrap();
    (samples, report.diagnostics.len())
}

#[test]
fn a_project_with_a_plugin_node_renders_through_the_extend_registry_hook() {
    let loud = project("", "");
    let quiet = project("\"p1\": 0.5", "");
    let (loud, loud_problems) = render(&loud, true);
    let (quiet, quiet_problems) = render(&quiet, true);
    assert_eq!((loud_problems, quiet_problems), (0, 0));
    assert!(loud.iter().any(|&s| s.abs() > 0.1), "silent render");
    for (l, q) in loud.iter().zip(&quiet) {
        assert!((l * 0.5 - q).abs() < 1e-6, "{l} {q}");
    }
}

#[test]
fn without_the_hook_the_plugin_node_is_unknown_and_reported() {
    let (samples, problems) = render(&project("", ""), false);
    assert!(problems > 0, "an unknown node type is a diagnostic");
    assert!(samples.iter().all(|&s| s == 0.0));
}

#[test]
fn state_in_the_project_file_survives_saving_and_loading() {
    let bias = base64::engine::general_purpose::STANDARD.encode(0.5f32.to_le_bytes());
    let original = project("", &format!("\"state\": \"{bias}\""));
    let reloaded = Project::from_ron(&original.to_ron()).unwrap();
    assert_eq!(reloaded, original);

    let (plain, _) = render(&project("", ""), true);
    let (with_state, problems) = render(&reloaded, true);
    assert_eq!(problems, 0);
    // The saved state adds 0.5 of DC to every sample.
    for (p, s) in plain.iter().zip(&with_state) {
        assert!((p + 0.5 - s).abs() < 1e-6, "{p} {s}");
    }
}
