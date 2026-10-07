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
    let (_, command) = group_nodes(&mut project, &ids(&[1, 2, 3])).unwrap();
    history.apply(&mut project, command).unwrap();
    let (osc, command) = group_nodes(&mut project, &ids(&[4])).unwrap();
    history.apply(&mut project, command).unwrap();
    let (_, command) = group_nodes(&mut project, &[osc, NodeId(5)]).unwrap();
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
