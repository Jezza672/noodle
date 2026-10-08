//! Group nodes cost nothing: a project with parts folded into groups renders
//! exactly like the same project wired flat.

use std::fs;
use std::path::Path;

use noodle_core::group::group_nodes;
use noodle_core::{History, NodeId, Project};
use noodle_engine::{Registry, Settings, render};

const SETTINGS: Settings = Settings {
    sample_rate: 48_000.0,
    max_frames: 512,
    channels: 1,
};
const FRAMES: usize = 4_800;

fn render_project(project: &Project) -> Vec<f32> {
    let mut registry = Registry::with_builtins();
    let _telemetry = noodle_nodes::register_all(&mut registry);
    let rendered = render(project.graph(), &registry, SETTINGS, FRAMES).unwrap();
    assert!(
        rendered.diagnostics.is_empty(),
        "{:?}",
        rendered.diagnostics
    );
    assert!(rendered.samples.iter().any(|&x| x != 0.0), "silent");
    rendered.samples
}

#[test]
fn a_grouped_vibrato_sounds_the_same_as_the_flat_one() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/vibrato.ron");
    let mut project = Project::from_ron(&fs::read_to_string(path).unwrap()).unwrap();
    let flat = render_project(&project);

    let mut history = History::new();
    // Fold the LFO (1-3) up, then the oscillator (4) on its own, then that
    // oscillator with the output gain (5), so signals cross one group on the
    // way out and two on the way in.
    let ids = |ns: &[u64]| ns.iter().map(|&n| NodeId(n)).collect::<Vec<_>>();
    let (_, command) =
        group_nodes(&project.clone(), &ids(&[1, 2, 3]), || project.new_node_id()).unwrap();
    history.apply(&mut project, command).unwrap();
    let (osc, command) =
        group_nodes(&project.clone(), &ids(&[4]), || project.new_node_id()).unwrap();
    history.apply(&mut project, command).unwrap();
    let (_, command) = group_nodes(&project.clone(), &[osc, NodeId(5)], || {
        project.new_node_id()
    })
    .unwrap();
    history.apply(&mut project, command).unwrap();
    assert!(project.graph().ancestors(NodeId(4)).len() == 2);

    assert_eq!(render_project(&project), flat);

    // The same after saving and loading, and after undo.
    let reloaded = Project::from_ron(&project.to_ron()).unwrap();
    assert_eq!(render_project(&reloaded), flat);
    for _ in 0..3 {
        history.undo(&mut project).unwrap();
    }
    assert_eq!(render_project(&project), flat);
}

/// The vibrato with its oscillator and output gain folded into one group,
/// and that group's output node.
fn grouped_output() -> (Project, History, Vec<f32>, NodeId) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/vibrato.ron");
    let mut project = Project::from_ron(&fs::read_to_string(path).unwrap()).unwrap();
    let flat = render_project(&project);
    let mut history = History::new();
    let (group, command) = group_nodes(&project.clone(), &[NodeId(4), NodeId(5)], || {
        project.new_node_id()
    })
    .unwrap();
    history.apply(&mut project, command).unwrap();
    let output = project.graph().group_ports(group).outputs[0].node;
    (project, history, flat, output)
}

fn set(project: &mut Project, history: &mut History, node: NodeId, key: &str, value: f32) {
    let command = noodle_core::Command::SetParam {
        node,
        key: key.into(),
        value: Some(value),
    };
    history.apply(project, command).unwrap();
}

#[test]
fn a_groups_gain_scales_what_comes_out_of_it() {
    let (mut project, mut history, flat, output) = grouped_output();
    set(&mut project, &mut history, output, "gain", -6.0206);
    let quieter = render_project(&project);
    assert_eq!(quieter.len(), flat.len());
    for (a, b) in quieter.iter().zip(&flat) {
        assert!((a - b * 0.5).abs() < 1e-3, "{a} vs half of {b}");
    }
}

#[test]
fn a_muted_group_is_silent_and_unmuting_restores_it() {
    let (mut project, mut history, flat, output) = grouped_output();
    set(&mut project, &mut history, output, "mute", 1.0);
    let mut registry = Registry::with_builtins();
    let _telemetry = noodle_nodes::register_all(&mut registry);
    let silent = render(project.graph(), &registry, SETTINGS, FRAMES).unwrap();
    assert!(silent.diagnostics.is_empty(), "{:?}", silent.diagnostics);
    assert!(silent.samples.iter().all(|&x| x == 0.0));
    history.undo(&mut project).unwrap();
    assert_eq!(render_project(&project), flat);
}

#[test]
fn soloing_another_group_silences_this_one() {
    let (mut project, mut history, _, _) = grouped_output();
    // A second, empty group beside it, soloed.
    let group = project.new_node_id();
    let command = noodle_core::Command::AddNode {
        id: group,
        node: noodle_core::Node::new(noodle_core::group::GROUP),
    };
    history.apply(&mut project, command).unwrap();
    let solo_out = project.new_node_id();
    let boundary = noodle_core::Node::new(noodle_core::group::GROUP_OUTPUT)
        .with_config(noodle_core::Config::new().with(
            noodle_core::group::PORT_NAME,
            noodle_core::Value::Text("out".into()),
        ))
        .in_group(group);
    let command = noodle_core::Command::AddNode {
        id: solo_out,
        node: boundary,
    };
    history.apply(&mut project, command).unwrap();
    set(&mut project, &mut history, solo_out, "solo", 1.0);
    let mut registry = Registry::with_builtins();
    let _telemetry = noodle_nodes::register_all(&mut registry);
    let rendered = render(project.graph(), &registry, SETTINGS, FRAMES).unwrap();
    assert!(rendered.samples.iter().all(|&x| x == 0.0));
}
