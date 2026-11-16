//! Convert to sample node and Convert to sampler: a stretch of audio becomes
//! a Sample node, or the stretches between transients become the keys of a
//! Sampler. Each is one undo step.
//!
//! - A region of a clip (the clip editor) becomes a Sample node on the
//!   file's own audio, with the selection as its start and end.
//! - A region of the arrangement is bounced first (see
//!   [`Session::bounce_to_sample`]), then becomes a Sample node on the file
//!   the bounce wrote.
//! - The transients picked in the clip editor become a Sampler track. The
//!   Sampler is a group of primitives (a Sample Set into a Sampler) with its
//!   controls as group inputs, so it can be edited like any other
//!   instrument; each region between two picked transients is one entry of
//!   the set, on its own key.

use super::*;
use crate::timeline::add_track;
use noodle_core::group::{GROUP, GROUP_INPUT, GROUP_OUTPUT, PORT_NAME, port_key, ports_config};
use noodle_core::{Config, Endpoint, Node, Position, Value};
use noodle_nodes::{SAMPLE_INPUT_ID, SAMPLE_KEY, SAMPLE_SET_ID, SAMPLER_ID, SampleRef, SetEntry};

/// The key of the first slice. C2, as Slice to Sampler starts at.
pub const FIRST_KEY: u8 = 36;
/// The most slices one conversion makes: one for each key from the first to
/// 127.
pub const MAX_SLICES: usize = 128 - FIRST_KEY as usize;
/// How much shorter a region than this, in seconds, is not worth a sample.
const MIN_REGION: f64 = 0.001;

/// The controls the Sampler group exposes, in order, with the Sampler
/// parameter each drives.
const CONTROLS: [(&str, &str); 4] = [
    ("tune", "tune"),
    ("bend range", "bend_range"),
    ("release", "release"),
    ("gain", "gain"),
];

impl Session {
    /// A Sample node on the part of the audio `clip` plays from `from` to
    /// `to` seconds of its file.
    pub fn region_to_sample_node(&mut self, clip: ClipId, from: f64, to: f64) {
        match self.region_command(clip, from, to) {
            Ok(command) => {
                self.edit([Edit::Apply(command)]);
                self.message = Some("Made a Sample node from the selection".into());
            }
            Err(why) => self.message = Some(why),
        }
    }

    /// A Sampler track whose keys play the stretches between consecutive
    /// `cuts` (seconds of the clip's file, in order) of the audio of `clip`.
    pub fn cuts_to_sampler(&mut self, clip: ClipId, cuts: &[f64]) {
        match self.sampler_command(clip, cuts) {
            Ok((command, slices)) => {
                self.edit([Edit::Apply(command)]);
                self.message = Some(format!("Made a Sampler of {slices} slices on a new track"));
            }
            Err(why) => self.message = Some(why),
        }
    }

    /// The reference to the window `from` to `to` seconds of the file whose
    /// hash is `hash`.
    fn window_of(&self, hash: &str, from: f64, to: f64) -> Result<SampleRef, String> {
        let sample = self.samples().get(hash).ok_or("The sample can't be read")?;
        let seconds = sample.frames() as f64 / f64::from(sample.sample_rate.max(1));
        if seconds <= 0.0 {
            return Err("The sample is empty".into());
        }
        let (start, end) = (
            (from / seconds).clamp(0.0, 1.0),
            (to / seconds).clamp(0.0, 1.0),
        );
        if (end - start) * seconds < MIN_REGION {
            return Err("The selection is too short to be a sample".into());
        }
        Ok(SampleRef {
            hash: hash.to_owned(),
            start: start as f32,
            end: end as f32,
        })
    }

    /// The command that adds a Sample node playing `reference`, at the top
    /// level below everything else.
    pub(super) fn sample_node_command(&self, reference: &SampleRef) -> Command {
        let below = self
            .project
            .graph()
            .nodes()
            .map(|(_, n)| n.position.y)
            .fold(0.0, f32::max);
        let node = Node::new(SAMPLE_INPUT_ID)
            .with_config(Config::new().with(SAMPLE_KEY, Value::Text(reference.to_text())))
            .at(0.0, below + 200.0);
        Command::AddNode {
            id: self.new_node_id(),
            node,
        }
    }

    fn region_command(&self, clip: ClipId, from: f64, to: f64) -> Result<Command, String> {
        let dropped = crate::sample_drop::from_clip(self, clip)?;
        let reference = self.window_of(&dropped.reference.hash, from.min(to), from.max(to))?;
        Ok(self.sample_node_command(&reference))
    }

    fn sampler_command(&self, clip: ClipId, cuts: &[f64]) -> Result<(Command, usize), String> {
        let dropped = crate::sample_drop::from_clip(self, clip)?;
        let regions: Vec<(f64, f64)> = cuts
            .windows(2)
            .filter(|w| w[1] - w[0] >= MIN_REGION)
            .map(|w| (w[0], w[1]))
            .collect();
        if regions.is_empty() {
            return Err("Pick at least two transients to cut between".into());
        }
        if regions.len() > MAX_SLICES {
            return Err(format!("A Sampler holds {MAX_SLICES} slices at most"));
        }
        let entries = regions
            .iter()
            .enumerate()
            .map(|(i, &(from, to))| {
                Ok(SetEntry {
                    reference: self.window_of(&dropped.reference.hash, from, to)?,
                    root: Some(FIRST_KEY + i as u8),
                    layer: 1,
                    rr: 1,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        let slices = entries.len();
        let source = self.project.clip(clip).ok_or("That clip is gone")?;
        let audio = source.as_audio().ok_or("Only audio clips can be sliced")?;
        let name = std::path::Path::new(&audio.source)
            .file_stem()
            .map_or_else(|| "clip".to_owned(), |s| s.to_string_lossy().into_owned());

        let mut scratch = self.project.clone();
        let (track, input) = add_track::with_input(scratch.graph(), || self.new_node_id());
        track
            .clone()
            .apply(&mut scratch)
            .map_err(|e| e.to_string())?;
        let track_group = scratch
            .graph()
            .node(input)
            .and_then(|n| n.parent)
            .ok_or("the new track has no group")?;
        let track_output = scratch
            .graph()
            .children(Some(track_group))
            .find(|(_, n)| n.type_id == GROUP_OUTPUT)
            .map(|(id, _)| id)
            .ok_or("the new track has no output")?;
        let mut commands = vec![
            track,
            Command::SetConfig {
                node: track_group,
                key: PORT_NAME.into(),
                value: Some(Value::Text(format!("{name} (sampler)"))),
            },
        ];

        // The group, and inside it: the controls, a Sample Set, a Sampler.
        let group = self.new_node_id();
        let mut group_node = Node::new(GROUP)
            .with_config(Config::new().with(PORT_NAME, Value::Text("Sliced Sampler".into())))
            .at(300.0, 0.0);
        group_node.parent = Some(track_group);
        commands.push(Command::AddNode {
            id: group,
            node: group_node,
        });
        let mut inside = |type_id: &str, x: f32, y: f32, config: Config, params: &[(&str, f32)]| {
            let id = self.new_node_id();
            let mut node = Node::new(type_id).with_config(config);
            node.position = Position { x, y };
            for (key, value) in params {
                node = node.with_param(*key, *value);
            }
            node.parent = Some(group);
            commands.push(Command::AddNode { id, node });
            id
        };
        let mut inputs = vec![(1, "notes".to_owned())];
        inputs.extend(
            CONTROLS
                .iter()
                .enumerate()
                .map(|(i, (name, _))| (i as u32 + 2, (*name).to_owned())),
        );
        let controls = inside(GROUP_INPUT, -250.0, 0.0, ports_config(&inputs), &[]);
        let outputs = inside(
            GROUP_OUTPUT,
            750.0,
            0.0,
            ports_config(&[(1, "out".to_owned())]),
            &[],
        );
        let set = inside(
            SAMPLE_SET_ID,
            0.0,
            200.0,
            Config::new().with(SAMPLE_KEY, Value::Text(noodle_nodes::set_to_text(&entries))),
            &[],
        );
        // Each key plays its own slice at the pitch it was recorded.
        let sampler = inside(
            SAMPLER_ID,
            350.0,
            0.0,
            Config::new(),
            &[("keytrack", 0.0), ("mode", 0.0)],
        );
        let wire = |from: Endpoint, to: Endpoint| noodle_core::spare::wire(from, to);
        commands.push(wire(
            Endpoint::new(set, "sample"),
            Endpoint::new(sampler, "sample"),
        ));
        commands.push(wire(
            Endpoint::new(controls, port_key(1)),
            Endpoint::new(sampler, "in"),
        ));
        for (i, (_, param)) in CONTROLS.iter().enumerate() {
            commands.push(wire(
                Endpoint::new(controls, port_key(i as u32 + 2)),
                Endpoint::new(sampler, *param),
            ));
        }
        commands.push(wire(
            Endpoint::new(sampler, "out"),
            Endpoint::new(outputs, port_key(1)),
        ));
        // And around it: the track's own MIDI in, the sound out.
        commands.push(wire(
            Endpoint::new(input, "midi"),
            Endpoint::new(group, "notes"),
        ));
        commands.push(wire(
            Endpoint::new(group, "out"),
            Endpoint::new(track_output, port_key(1)),
        ));
        Ok((Command::Batch(commands), slices))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_core::{Clip, Tick};

    const RATE: u32 = 44_100;

    /// A saved project with a clip of a four second file on a track.
    fn rig(dir: &std::path::Path) -> (Session, ClipId) {
        let mut session = Session::new(crate::session::Nodes::all());
        let (track, input) =
            add_track::with_input(session.project().graph(), || session.new_node_id());
        session.edit([Edit::Apply(track)]);
        let samples: Vec<f32> = (0..4 * RATE as usize)
            .map(|i| 0.3 * (std::f32::consts::TAU * 220.0 * i as f32 / RATE as f32).sin())
            .collect();
        noodle_io::write_wav(&dir.join("hum.wav"), &samples, 1, RATE).unwrap();
        let id = session.project().next_clip_id();
        let clip = Clip::audio(input, Tick(0), "hum.wav", samples.len() as u64);
        session.edit([Edit::Apply(Command::AddClip { id, clip })]);
        assert!(session.save_as(&dir.join("project.ron")));
        (session, id)
    }

    fn sample_nodes(session: &Session) -> Vec<(NodeId, String)> {
        session
            .project()
            .graph()
            .nodes()
            .filter(|(_, n)| n.type_id == SAMPLE_INPUT_ID)
            .filter_map(|(id, n)| match n.config.get(SAMPLE_KEY) {
                Some(Value::Text(text)) => Some((id, text.clone())),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_region_becomes_a_sample_node_on_the_files_own_audio_with_start_and_end() {
        let dir = tempfile::tempdir().unwrap();
        let (mut session, clip) = rig(dir.path());
        session.region_to_sample_node(clip, 1.0, 3.0);
        assert_eq!(
            session.message(),
            Some("Made a Sample node from the selection")
        );
        let nodes = sample_nodes(&session);
        assert_eq!(nodes.len(), 1);
        let reference = SampleRef::parse(&nodes[0].1);
        assert!((reference.start - 0.25).abs() < 1e-4, "{reference:?}");
        assert!((reference.end - 0.75).abs() < 1e-4, "{reference:?}");
        // The selection can be made either way round, and a sliver is refused.
        session.region_to_sample_node(clip, 0.5, 0.25);
        assert_eq!(sample_nodes(&session).len(), 2);
        session.region_to_sample_node(clip, 1.0, 1.0001);
        assert_eq!(sample_nodes(&session).len(), 2);
        assert!(session.message().unwrap().contains("too short"));
        session.undo();
        session.undo();
        assert!(sample_nodes(&session).is_empty());
    }

    #[test]
    fn picked_cuts_become_a_sampler_group_of_primitives_with_a_key_for_each_region() {
        let dir = tempfile::tempdir().unwrap();
        let (mut session, clip) = rig(dir.path());
        let tracks = crate::timeline::tracks(session.project()).len();
        session.cuts_to_sampler(clip, &[0.5, 1.0, 2.0, 3.0]);
        assert_eq!(
            session.message(),
            Some("Made a Sampler of 3 slices on a new track")
        );
        assert_eq!(crate::timeline::tracks(session.project()).len(), tracks + 1);
        let graph = session.project().graph();
        let of = |ty: &str| graph.nodes().find(|(_, n)| n.type_id == ty).map(|(i, _)| i);
        let (set, sampler) = (of(SAMPLE_SET_ID).unwrap(), of(SAMPLER_ID).unwrap());
        // The sampler is inside a group, whose inputs are its controls.
        let group = graph.node(sampler).unwrap().parent.unwrap();
        assert_eq!(graph.node(set).unwrap().parent, Some(group));
        let ports = graph.group_ports(group);
        let names: Vec<_> = ports.inputs.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["notes", "tune", "bend range", "release", "gain"]);
        assert_eq!(ports.outputs.len(), 1);
        // The set holds a region each, from key 36.
        let Some(Value::Text(text)) = graph.node(set).unwrap().config.get(SAMPLE_KEY) else {
            panic!("the set has no entries")
        };
        let entries = noodle_nodes::parse_set(text);
        let roots: Vec<_> = entries.iter().map(|e| e.root).collect();
        assert_eq!(roots, [Some(36), Some(37), Some(38)]);
        let window = |i: usize| (entries[i].reference.start, entries[i].reference.end);
        assert!((window(0).0 - 0.125).abs() < 1e-4 && (window(0).1 - 0.25).abs() < 1e-4);
        assert!((window(2).0 - 0.5).abs() < 1e-4 && (window(2).1 - 0.75).abs() < 1e-4);
        // It compiles clean, and the Sampler gets the whole set.
        let (schedule, diagnostics) = noodle_engine::compile(graph, session.registry());
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        let node = schedule.nodes.iter().find(|n| n.id == sampler).unwrap();
        let Some(Value::Text(set)) = node.config.get(SAMPLE_KEY) else {
            panic!("the sampler has no sample")
        };
        assert_eq!(noodle_nodes::parse_set(set).len(), 3);
        session.undo();
        assert_eq!(crate::timeline::tracks(session.project()).len(), tracks);
        assert!(
            session
                .project()
                .graph()
                .nodes()
                .all(|(_, n)| n.type_id != SAMPLER_ID)
        );
    }

    #[test]
    fn one_cut_is_not_enough_to_make_a_sampler() {
        let dir = tempfile::tempdir().unwrap();
        let (mut session, clip) = rig(dir.path());
        session.cuts_to_sampler(clip, &[1.0]);
        assert!(session.message().unwrap().contains("at least two"));
        assert!(
            session
                .project()
                .graph()
                .nodes()
                .all(|(_, n)| n.type_id != SAMPLER_ID)
        );
    }
}
