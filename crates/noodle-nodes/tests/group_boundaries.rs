//! Group boundaries are invisible: whatever kind of signal crosses one (audio,
//! events, spectra, modulation, voices), a project folded into groups renders
//! exactly like the same project wired flat, at any depth, however the groups
//! were made, with the boundary controls touched or not, and after a save,
//! a load or an undo.
//!
//! Every test here compares sample for sample against the flat project, so a
//! wire that a boundary drops, or a node it puts in the way of a wire it
//! can't carry, shows up as a difference or a diagnostic.

use std::fs;
use std::path::Path;

use noodle_core::group::{
    GAIN, GROUP, GROUP_INPUT, GROUP_OUTPUT, MUTE, control_key, group_nodes, port_key, ports_config,
};
use noodle_core::{Command, Connection, Endpoint, History, Node, NodeId, Project};
use noodle_engine::{Registry, Settings, render};

const SETTINGS: Settings = Settings {
    sample_rate: 48_000.0,
    max_frames: 512,
    channels: 1,
};
const FRAMES: usize = 4_800;

/// Examples that between them put every kind of wire on the line: audio and
/// modulation (`vibrato`), events into voices (`euclid-chords`),
/// spectra (`spectral-shift`) and a plain chain (`saw-chord`).
const EXAMPLES: [&str; 4] = ["vibrato", "euclid-chords", "spectral-shift", "saw-chord"];

fn registry() -> Registry {
    let mut registry = Registry::with_builtins();
    let _telemetry = noodle_nodes::register_all(&mut registry);
    registry
}

fn example(name: &str) -> Project {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples")
        .join(format!("{name}.ron"));
    Project::from_ron(&fs::read_to_string(path).unwrap()).unwrap()
}

/// Renders the project's graph, insisting it compiles cleanly.
fn sound(project: &Project, what: &str) -> Vec<f32> {
    let rendered = render(project.graph(), &registry(), SETTINGS, FRAMES).unwrap();
    assert!(
        rendered.diagnostics.is_empty(),
        "{what}: {:?}",
        rendered.diagnostics
    );
    rendered.samples
}

fn assert_same(project: &Project, flat: &[f32], what: &str) {
    let grouped = sound(project, what);
    assert_eq!(grouped.len(), flat.len(), "{what}: length");
    if let Some(at) = grouped.iter().zip(flat).position(|(a, b)| a != b) {
        panic!(
            "{what}: differs from the flat project at sample {at}: {} against {}",
            grouped[at], flat[at]
        );
    }
}

fn flat_sound(name: &str) -> (Project, Vec<f32>) {
    let project = example(name);
    let samples = sound(&project, name);
    assert!(
        samples.iter().any(|&x| x.abs() > 1e-4),
        "{name} is silent flat, so it proves nothing"
    );
    (project, samples)
}

/// A small, seeded random number generator, so failures reproduce.
struct XorShift(u64);

impl XorShift {
    fn below(&mut self, n: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % n as u64) as usize
    }
}

fn add(project: &mut Project, history: &mut History, node: Node) -> NodeId {
    let id = project.new_node_id();
    history
        .apply(project, Command::AddNode { id, node })
        .unwrap();
    id
}

fn connect(project: &mut Project, history: &mut History, from: Endpoint, to: Endpoint) {
    history
        .apply(project, Command::Connect(Connection { from, to }))
        .unwrap();
}

/// How a pass-through's boundary controls are left.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Controls {
    /// Never touched, so flatten drops the boundary entirely.
    Untouched,
    /// Gain and mute written at their defaults on every boundary port, which
    /// keeps a stage wherever one can stand.
    SetToDefaults,
}

/// Puts `depth` nested groups on the wire into `to`, each passing it from an
/// input straight to an output, and returns the nodes of the boundaries.
fn route_through_groups(
    project: &mut Project,
    history: &mut History,
    to: &Endpoint,
    depth: usize,
    controls: Controls,
) {
    let from = project.graph().source(to).unwrap().clone();
    history
        .apply(project, Command::Disconnect { input: to.clone() })
        .unwrap();
    let parent = project.graph().node(to.node).unwrap().parent;
    let ports = |name: &str| ports_config(&[(1, name.to_owned())]);
    let mut groups: Vec<NodeId> = Vec::new();
    for level in 0..depth {
        let mut group = Node::new(GROUP);
        group.parent = if level == 0 {
            parent
        } else {
            Some(groups[level - 1])
        };
        groups.push(add(project, history, group));
    }
    // Outermost first: the wire enters the outer group's `a`, and each level
    // hands it to the next one's `a` and takes it back from its `b`.
    let mut boundaries = Vec::new();
    for &group in &groups {
        let input = add(
            project,
            history,
            Node::new(GROUP_INPUT)
                .with_config(ports("a"))
                .in_group(group),
        );
        let output = add(
            project,
            history,
            Node::new(GROUP_OUTPUT)
                .with_config(ports("b"))
                .in_group(group),
        );
        boundaries.push((input, output));
    }
    connect(project, history, from, Endpoint::new(groups[0], "a"));
    for (level, &(input, output)) in boundaries.iter().enumerate() {
        match groups.get(level + 1) {
            Some(&inner) => {
                connect(
                    project,
                    history,
                    Endpoint::new(input, port_key(1)),
                    Endpoint::new(inner, "a"),
                );
                connect(
                    project,
                    history,
                    Endpoint::new(inner, "b"),
                    Endpoint::new(output, port_key(1)),
                );
            }
            None => connect(
                project,
                history,
                Endpoint::new(input, port_key(1)),
                Endpoint::new(output, port_key(1)),
            ),
        }
    }
    connect(project, history, Endpoint::new(groups[0], "b"), to.clone());
    if controls == Controls::SetToDefaults {
        for &(input, output) in &boundaries {
            for node in [input, output] {
                for (key, value) in [(GAIN, 0.0), (MUTE, 0.0)] {
                    history
                        .apply(
                            project,
                            Command::SetParam {
                                node,
                                key: control_key(key, 1),
                                value: Some(value),
                            },
                        )
                        .unwrap();
                }
            }
        }
    }
}

/// Every wire of the example, one at a time, goes through `depth` groups.
fn every_wire_through(name: &str, depth: usize, controls: Controls) {
    let (flat_project, flat) = flat_sound(name);
    let wires: Vec<Connection> = flat_project.graph().connections().collect();
    assert!(!wires.is_empty());
    for wire in wires {
        let mut project = flat_project.clone();
        let mut history = History::new();
        route_through_groups(&mut project, &mut history, &wire.to, depth, controls);
        let what = format!(
            "{name}: wire {}.{} -> {}.{} through {depth} group(s), {controls:?}",
            wire.from.node.0, wire.from.port, wire.to.node.0, wire.to.port
        );
        assert_same(&project, &flat, &what);
    }
}

#[test]
fn every_wire_of_every_kind_crosses_one_group_untouched() {
    for name in EXAMPLES {
        every_wire_through(name, 1, Controls::Untouched);
    }
}

#[test]
fn every_wire_of_every_kind_crosses_nested_groups_untouched() {
    for name in EXAMPLES {
        every_wire_through(name, 3, Controls::Untouched);
    }
}

/// Once a boundary port's gain or mute has been set, flatten keeps a signal
/// node there so the control has something to move. Wires that node can't
/// carry (events, spectra) must still get through.
#[test]
fn every_wire_of_every_kind_crosses_one_group_with_its_controls_set() {
    for name in EXAMPLES {
        every_wire_through(name, 1, Controls::SetToDefaults);
    }
}

#[test]
fn every_wire_of_every_kind_crosses_nested_groups_with_their_controls_set() {
    for name in EXAMPLES {
        every_wire_through(name, 3, Controls::SetToDefaults);
    }
}

/// Groups made the way a user makes them: pick some nodes, fold them up.
/// Picks are random, nest inside earlier groups, and are seeded so a failure
/// can be replayed.
#[test]
fn randomly_folded_groups_render_like_the_flat_project() {
    for (n, name) in EXAMPLES.into_iter().enumerate() {
        let (flat_project, flat) = flat_sound(name);
        let mut rng = XorShift(0x9e37_79b9 + n as u64);
        for trial in 0..6 {
            let mut project = flat_project.clone();
            let mut history = History::new();
            for _ in 0..1 + rng.below(4) {
                // Siblings at one level: the top level, or inside one group.
                let level = {
                    let parents: Vec<Option<NodeId>> = std::iter::once(None)
                        .chain(
                            project
                                .graph()
                                .nodes()
                                .filter(|(_, n)| n.type_id == GROUP)
                                .map(|(id, _)| Some(id)),
                        )
                        .collect();
                    parents[rng.below(parents.len())]
                };
                let candidates: Vec<NodeId> = project
                    .graph()
                    .children(level)
                    .filter(|(_, n)| !matches!(n.type_id.as_str(), GROUP_INPUT | GROUP_OUTPUT))
                    .map(|(id, _)| id)
                    .collect();
                if candidates.is_empty() {
                    continue;
                }
                let picked: Vec<NodeId> = candidates
                    .iter()
                    .copied()
                    .filter(|_| rng.below(3) == 0)
                    .collect();
                if picked.is_empty() {
                    continue;
                }
                let Ok((_, command)) =
                    group_nodes(&project.clone(), &picked, || project.new_node_id())
                else {
                    continue;
                };
                history.apply(&mut project, command).unwrap();
            }
            let what = format!("{name}, trial {trial}");
            assert_same(&project, &flat, &what);

            // A save and a load change nothing.
            let reloaded = Project::from_ron(&project.to_ron()).unwrap();
            assert_eq!(reloaded.graph(), project.graph(), "{what}: round trip");
            assert_same(&reloaded, &flat, &format!("{what}, reloaded"));

            // Undoing every grouping gets the flat project back, exactly.
            while history.undo(&mut project).unwrap() {}
            assert_eq!(project.graph(), flat_project.graph(), "{what}: undo");
        }
    }
}
