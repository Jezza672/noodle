//! The timeline's key reaches the Song Key node, and through its wires moves
//! Scale Quantise and Transpose, at each marker.

use noodle_core::{
    Command, Connection, Endpoint, History, Key, Marker, Mode, Node, NodeId, Project, Tick,
};
use noodle_engine::{OUTPUT_ID, Registry, Settings, render_project};
use noodle_nodes::register_library_blocking;

const SETTINGS: Settings = Settings {
    sample_rate: 48_000.0,
    max_frames: 256,
    channels: 1,
};
/// One bar of 4/4 at 120 bpm (two seconds), in samples.
const MARKER_SAMPLE: usize = 96_000;

/// A Song Key node (written in `from`) with `output` wired to the Output,
/// rendered for four seconds with a marker at bar 2 that sets `later`.
fn render(from: f32, output: &str, key: Key, later: Option<Key>) -> Vec<f32> {
    let (mut project, mut history) = (Project::new(), History::new());
    let mut apply = |project: &mut Project, command| history.apply(project, command).unwrap();
    apply(&mut project, Command::SetKey(key));
    if later.is_some() {
        apply(
            &mut project,
            Command::SetMarkers(vec![Marker {
                name: "Chorus".into(),
                at: Tick(4 * 960),
                key: later,
            }]),
        );
    }
    let song_key: NodeId = project.new_node_id();
    apply(
        &mut project,
        Command::AddNode {
            id: song_key,
            node: Node::new("noodle.mod.key").with_param("from", from),
        },
    );
    let out = project.new_node_id();
    apply(
        &mut project,
        Command::AddNode {
            id: out,
            node: Node::new(OUTPUT_ID),
        },
    );
    apply(
        &mut project,
        Command::Connect(Connection {
            from: Endpoint::new(song_key, output),
            to: Endpoint::new(out, "in"),
        }),
    );
    let mut registry = Registry::with_builtins();
    let _library = register_library_blocking(&mut registry);
    let render = render_project(&project, &registry, SETTINGS, 4 * 48_000).unwrap();
    assert!(render.diagnostics.is_empty(), "{:?}", render.diagnostics);
    render.samples
}

#[test]
fn the_song_keys_outputs_follow_the_project_key() {
    let a_minor = Key::new(9, Mode::Minor);
    // Written in C major; the project is in A minor: tonic 9, minor, no
    // shift (relative keys).
    let root = render(0.0, "root", a_minor, None);
    assert!(root.iter().all(|&s| s == 9.0));
    let scale = render(0.0, "scale", a_minor, None);
    assert!(scale.iter().all(|&s| s == 1.0));
    let shift = render(0.0, "shift", a_minor, None);
    assert!(shift.iter().all(|&s| s == 0.0));
    // Written in D major (choice 2); the project is in C major: -2.
    let shift = render(2.0, "shift", Key::default(), None);
    assert!(shift.iter().all(|&s| s == -2.0));
}

#[test]
fn a_marker_with_another_key_moves_the_outputs_from_the_next_block() {
    let g_major = Key::new(7, Mode::Major);
    let shift = render(0.0, "shift", Key::default(), Some(g_major));
    // C to G the short way is -5.
    assert!(
        shift[..MARKER_SAMPLE].iter().all(|&s| s == 0.0),
        "before the marker"
    );
    // The change lands at the next block, at most one block late.
    assert!(
        shift[MARKER_SAMPLE + SETTINGS.max_frames..]
            .iter()
            .all(|&s| s == -5.0),
        "after the marker"
    );
    assert_eq!(shift[4 * 48_000 - 1], -5.0);
    let root = render(0.0, "root", Key::default(), Some(g_major));
    assert_eq!(root[0], 0.0);
    assert_eq!(root[4 * 48_000 - 1], 7.0);
}
