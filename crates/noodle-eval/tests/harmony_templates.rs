//! The Harmoniser and Euclid Hat templates are working groups: the Harmoniser
//! plays a chord on the note it is given and, when it follows the key, moves
//! and snaps it to the song's key; the Euclid Hat plays a hat on its rhythm
//! and goes quiet when its pulses are none.

use std::path::Path;

use noodle_core::library::Library;
use noodle_core::{
    Command, Connection, Endpoint, History, Key, Mode, Node, NodeId, Position, Project, TempoMap,
    TimeSignature,
};
use noodle_engine::{OUTPUT_ID, Registry, Settings};

const SETTINGS: Settings = Settings {
    sample_rate: 48_000.0,
    max_frames: 256,
    channels: 1,
};

struct Rig {
    project: Project,
    history: History,
}

impl Rig {
    fn new(key: Key) -> Self {
        let mut rig = Self {
            project: Project::new(),
            history: History::new(),
        };
        rig.apply(Command::SetTempoMap(
            TempoMap::constant(120.0, TimeSignature::COMMON).unwrap(),
        ));
        rig.apply(Command::SetKey(key));
        rig
    }

    fn apply(&mut self, command: Command) {
        self.history.apply(&mut self.project, command).unwrap();
    }

    fn add(&mut self, node: Node) -> NodeId {
        let id = self.project.new_node_id();
        self.apply(Command::AddNode { id, node });
        id
    }

    fn wire(&mut self, from: NodeId, from_port: &str, to: NodeId, to_port: &str) {
        self.apply(Command::Connect(Connection {
            from: Endpoint::new(from, from_port),
            to: Endpoint::new(to, to_port),
        }));
    }

    fn template(&mut self, slug: &str) -> NodeId {
        let library = Library::in_memory();
        let template = library
            .template(&format!("system:template:{slug}"))
            .expect("the template is in the library");
        let (group, command) = template.instantiate(slug, None, Position::default(), || {
            self.project.new_node_id()
        });
        self.apply(command);
        group
    }

    /// Sets `key` on every node of `type_id` inside `group`.
    fn set_inside(&mut self, group: NodeId, type_id: &str, key: &str, value: f32) {
        let nodes: Vec<NodeId> = self
            .project
            .graph()
            .children(Some(group))
            .filter(|(_, n)| n.type_id == type_id)
            .map(|(id, _)| id)
            .collect();
        assert!(!nodes.is_empty(), "no {type_id} in the group");
        for node in nodes {
            self.apply(Command::SetParam {
                node,
                key: key.into(),
                value: Some(value),
            });
        }
    }

    fn render(&self, frames: usize) -> Vec<f32> {
        let mut registry = Registry::with_builtins();
        let out = noodle_nodes::render_project_with_clips(
            &self.project,
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
}

fn peak(samples: &[f32]) -> f32 {
    samples.iter().fold(0.0, |m, x| m.max(x.abs()))
}

/// The pitch of a steady tone, from its rising zero crossings.
fn hertz(samples: &[f32]) -> f32 {
    let crossings = samples
        .windows(2)
        .filter(|w| w[0] < 0.0 && w[1] >= 0.0)
        .count();
    crossings as f32 / (samples.len() as f32 / 48_000.0)
}

/// The Harmoniser given C4, into a Mono Note (which holds the highest note of
/// the chord, as it sounds last) and a saw.
fn harmoniser_pitch(key: Key, follow: bool) -> f32 {
    let mut rig = Rig::new(key);
    let group = rig.template("harmoniser");
    if follow {
        rig.set_inside(group, "noodle.util.math", "c", 1.0);
    }
    let note = rig.add(
        Node::new("noodle.event.key")
            .with_param("gate", 1.0)
            .with_param("note", 60.0),
    );
    let mono = rig.add(Node::new("noodle.event.mono"));
    let saw = rig.add(Node::new("noodle.osc.saw"));
    let out = rig.add(Node::new(OUTPUT_ID));
    rig.wire(note, "out", group, "in");
    rig.wire(group, "out", mono, "in");
    rig.wire(mono, "pitch", saw, "frequency");
    rig.wire(saw, "out", out, "in");
    let samples = rig.render(24_000);
    hertz(&samples[4_800..])
}

#[test]
fn the_harmoniser_plays_a_chord_on_the_note() {
    // C major triad on C4: the top note is G4 (392 Hz).
    let pitch = harmoniser_pitch(Key::new(2, Mode::Minor), false);
    assert!((pitch - 392.0).abs() < 6.0, "{pitch}");
}

#[test]
fn following_the_key_moves_and_snaps_the_chord() {
    // The part is written in C major; the song is in D minor (relative F
    // major), so C4 goes up five to F4 and the triad's top is C5 (523 Hz).
    let pitch = harmoniser_pitch(Key::new(2, Mode::Minor), true);
    assert!((pitch - 523.25).abs() < 8.0, "{pitch}");
    // With the song in C major there is no shift: back to G4.
    let pitch = harmoniser_pitch(Key::new(0, Mode::Major), true);
    assert!((pitch - 392.0).abs() < 6.0, "{pitch}");
}

fn euclid_hat(pulses: f32) -> Vec<f32> {
    let mut rig = Rig::new(Key::default());
    let group = rig.template("euclid-hat");
    rig.set_inside(group, "noodle.event.euclidean", "pulses", pulses);
    let out = rig.add(Node::new(OUTPUT_ID));
    rig.wire(group, "out", out, "in");
    rig.render(48_000)
}

#[test]
fn the_euclid_hat_plays_its_rhythm_and_is_quiet_without_pulses() {
    let samples = euclid_hat(5.0);
    // Five hits in sixteen sixteenths at 120 bpm: steps 0, 3, 6, 9 and 12 of
    // a 12000-sample... a step is 6000 samples, so the second 6000 holds a
    // hit at step 3 (18000) too.
    assert!(peak(&samples[0..3000]) > 0.01, "the first hit");
    assert!(peak(&samples[18_000..21_000]) > 0.01, "the next hit");
    assert!(peak(&samples[9_000..15_000]) < 1e-4, "silence between");
    assert!(peak(&euclid_hat(0.0)) < 1e-4, "no pulses, no hat");
}
