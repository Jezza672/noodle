//! The whole project, and saving and loading it.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::{
    AutomationLane, Clip, ClipId, EditError, Endpoint, Frame, FrameId, Graph, LaneId, NodeId,
    TempoMap,
};

/// Everything that gets saved: the graph, the frames drawn around parts of
/// it, and the timeline: tempo map, clips and automation lanes. Change it through a [`History`](crate::History), so every change can
/// be undone.
#[derive(Clone, Debug)]
pub struct Project {
    graph: Graph,
    frames: BTreeMap<FrameId, Frame>,
    next_frame_id: u64,
    tempo_map: TempoMap,
    clips: BTreeMap<ClipId, Clip>,
    next_clip_id: u64,
    lanes: BTreeMap<LaneId, AutomationLane>,
    next_lane_id: u64,
    track_order: Vec<NodeId>,
}

impl Default for Project {
    fn default() -> Self {
        Self {
            graph: Graph::default(),
            frames: BTreeMap::new(),
            next_frame_id: 1,
            tempo_map: TempoMap::default(),
            clips: BTreeMap::new(),
            next_clip_id: 1,
            lanes: BTreeMap::new(),
            next_lane_id: 1,
            track_order: Vec::new(),
        }
    }
}

/// Like [`Graph`], two projects are equal if their contents are, whichever IDs
/// they'd hand out next.
impl PartialEq for Project {
    fn eq(&self, other: &Self) -> bool {
        self.graph == other.graph
            && self.frames == other.frames
            && self.tempo_map == other.tempo_map
            && self.clips == other.clips
            && self.lanes == other.lanes
            && self.track_order == other.track_order
    }
}

/// How a project is laid out on disk.
#[derive(Serialize, Deserialize)]
struct ProjectFile {
    format: u32,
    graph: Graph,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    frames: BTreeMap<FrameId, Frame>,
    #[serde(default, skip_serializing_if = "is_default_tempo_map")]
    tempo_map: TempoMap,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    clips: BTreeMap<ClipId, Clip>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    lanes: BTreeMap<LaneId, AutomationLane>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    track_order: Vec<NodeId>,
}

/// The clips and lanes that went with a node.
pub(crate) type Dependents = (Vec<(ClipId, Clip)>, Vec<(LaneId, AutomationLane)>);

fn is_default_tempo_map(map: &TempoMap) -> bool {
    *map == TempoMap::default()
}

impl Project {
    /// The current file format version. Bump it when the format changes in a
    /// way older versions can't read, and migrate older files when loading.
    pub const FORMAT: u32 = 2;

    pub fn new() -> Self {
        Self::default()
    }

    pub fn graph(&self) -> &Graph {
        &self.graph
    }

    pub(crate) fn graph_mut(&mut self) -> &mut Graph {
        &mut self.graph
    }

    /// Reserves an ID for a node that's about to be added.
    pub fn new_node_id(&mut self) -> NodeId {
        self.graph.new_id()
    }

    /// The ID [`new_node_id`](Self::new_node_id) would hand out next, without
    /// reserving it. It's never the ID of a node in the project, even one
    /// that was removed, so it can't clash with an undo.
    pub fn next_node_id(&self) -> NodeId {
        self.graph.next_id()
    }

    pub fn frame(&self, id: FrameId) -> Option<&Frame> {
        self.frames.get(&id)
    }

    pub fn frames(&self) -> impl Iterator<Item = (FrameId, &Frame)> {
        self.frames.iter().map(|(&id, frame)| (id, frame))
    }

    /// Reserves an ID for a frame that's about to be added.
    pub fn new_frame_id(&mut self) -> FrameId {
        let id = self.next_frame_id();
        self.next_frame_id += 1;
        id
    }

    /// The ID [`new_frame_id`](Self::new_frame_id) would return next.
    pub fn next_frame_id(&self) -> FrameId {
        FrameId(self.next_frame_id)
    }

    pub(crate) fn insert_frame(&mut self, id: FrameId, frame: Frame) -> Result<(), EditError> {
        if self.frames.contains_key(&id) {
            return Err(EditError::FrameExists(id));
        }
        self.frames.insert(id, frame);
        self.next_frame_id = self.next_frame_id.max(id.0 + 1);
        Ok(())
    }

    pub(crate) fn remove_frame(&mut self, id: FrameId) -> Result<Frame, EditError> {
        self.frames.remove(&id).ok_or(EditError::NoSuchFrame(id))
    }

    pub(crate) fn frame_mut(&mut self, id: FrameId) -> Result<&mut Frame, EditError> {
        self.frames.get_mut(&id).ok_or(EditError::NoSuchFrame(id))
    }

    pub fn tempo_map(&self) -> &TempoMap {
        &self.tempo_map
    }

    /// The order tracks are listed in, by group.
    pub fn track_order(&self) -> &[NodeId] {
        &self.track_order
    }

    /// Sorts `groups` into track order: the ones the stored order names
    /// first, in that order, then the rest by ID.
    pub fn sort_tracks(&self, groups: &mut [NodeId]) {
        groups.sort_by_key(|id| {
            let rank = self.track_order.iter().position(|g| g == id);
            (rank.unwrap_or(usize::MAX), *id)
        });
    }

    pub(crate) fn replace_track_order(&mut self, order: Vec<NodeId>) -> Vec<NodeId> {
        std::mem::replace(&mut self.track_order, order)
    }

    pub(crate) fn replace_tempo_map(&mut self, map: TempoMap) -> TempoMap {
        std::mem::replace(&mut self.tempo_map, map)
    }

    pub fn clip(&self, id: ClipId) -> Option<&Clip> {
        self.clips.get(&id)
    }

    pub fn clips(&self) -> impl Iterator<Item = (ClipId, &Clip)> {
        self.clips.iter().map(|(&id, clip)| (id, clip))
    }

    /// The clips a track input node plays, in ID order.
    pub fn clips_on(&self, node: NodeId) -> impl Iterator<Item = (ClipId, &Clip)> {
        self.clips().filter(move |(_, clip)| clip.node == node)
    }

    /// Reserves an ID for a clip that's about to be added.
    pub fn new_clip_id(&mut self) -> ClipId {
        let id = self.next_clip_id();
        self.next_clip_id += 1;
        id
    }

    /// The ID [`new_clip_id`](Self::new_clip_id) would return next.
    pub fn next_clip_id(&self) -> ClipId {
        ClipId(self.next_clip_id)
    }

    fn check_clip(&self, id: ClipId, clip: &Clip) -> Result<(), EditError> {
        if let Some(problem) = clip.problem() {
            return Err(EditError::InvalidClip(id, problem));
        }
        if self.graph.node(clip.node).is_none() {
            return Err(EditError::NoSuchNode(clip.node));
        }
        Ok(())
    }

    pub(crate) fn insert_clip(&mut self, id: ClipId, mut clip: Clip) -> Result<(), EditError> {
        clip.assign_note_ids();
        if self.clips.contains_key(&id) {
            return Err(EditError::ClipExists(id));
        }
        self.check_clip(id, &clip)?;
        self.clips.insert(id, clip);
        self.next_clip_id = self.next_clip_id.max(id.0 + 1);
        Ok(())
    }

    pub(crate) fn remove_clip(&mut self, id: ClipId) -> Result<Clip, EditError> {
        self.clips.remove(&id).ok_or(EditError::NoSuchClip(id))
    }

    pub(crate) fn replace_clip(&mut self, id: ClipId, mut clip: Clip) -> Result<Clip, EditError> {
        clip.assign_note_ids();
        if !self.clips.contains_key(&id) {
            return Err(EditError::NoSuchClip(id));
        }
        self.check_clip(id, &clip)?;
        Ok(std::mem::replace(
            self.clips.get_mut(&id).expect("checked"),
            clip,
        ))
    }

    pub fn lane(&self, id: LaneId) -> Option<&AutomationLane> {
        self.lanes.get(&id)
    }

    pub fn lanes(&self) -> impl Iterator<Item = (LaneId, &AutomationLane)> {
        self.lanes.iter().map(|(&id, lane)| (id, lane))
    }

    /// The lane driving an input, if there is one.
    pub fn lane_for(&self, target: &Endpoint) -> Option<(LaneId, &AutomationLane)> {
        self.lanes().find(|(_, lane)| lane.target == *target)
    }

    /// Reserves an ID for a lane that's about to be added.
    pub fn new_lane_id(&mut self) -> LaneId {
        let id = self.next_lane_id();
        self.next_lane_id += 1;
        id
    }

    /// The ID [`new_lane_id`](Self::new_lane_id) would return next.
    pub fn next_lane_id(&self) -> LaneId {
        LaneId(self.next_lane_id)
    }

    /// A lane is fine if its points are, its node exists, and no other lane
    /// drives the same input.
    fn check_lane(&self, id: LaneId, lane: &AutomationLane) -> Result<(), EditError> {
        if let Some(problem) = lane.problem() {
            return Err(EditError::InvalidLane(id, problem));
        }
        if self.graph.node(lane.target.node).is_none() {
            return Err(EditError::NoSuchNode(lane.target.node));
        }
        match self.lane_for(&lane.target) {
            Some((other, _)) if other != id => {
                Err(EditError::LaneTargetTaken(lane.target.clone(), other))
            }
            _ => Ok(()),
        }
    }

    pub(crate) fn insert_lane(
        &mut self,
        id: LaneId,
        lane: AutomationLane,
    ) -> Result<(), EditError> {
        if self.lanes.contains_key(&id) {
            return Err(EditError::LaneExists(id));
        }
        self.check_lane(id, &lane)?;
        self.lanes.insert(id, lane);
        self.next_lane_id = self.next_lane_id.max(id.0 + 1);
        Ok(())
    }

    pub(crate) fn remove_lane(&mut self, id: LaneId) -> Result<AutomationLane, EditError> {
        self.lanes.remove(&id).ok_or(EditError::NoSuchLane(id))
    }

    pub(crate) fn replace_lane(
        &mut self,
        id: LaneId,
        lane: AutomationLane,
    ) -> Result<AutomationLane, EditError> {
        if !self.lanes.contains_key(&id) {
            return Err(EditError::NoSuchLane(id));
        }
        self.check_lane(id, &lane)?;
        Ok(std::mem::replace(
            self.lanes.get_mut(&id).expect("checked"),
            lane,
        ))
    }

    /// Takes out what hangs off a node that's going: the clips it plays and
    /// the lanes driving its inputs.
    pub(crate) fn remove_dependents(&mut self, node: NodeId) -> Dependents {
        let clip_ids: Vec<_> = self.clips_on(node).map(|(id, _)| id).collect();
        let lane_ids: Vec<_> = self
            .lanes()
            .filter(|(_, lane)| lane.target.node == node)
            .map(|(id, _)| id)
            .collect();
        let clips = clip_ids
            .into_iter()
            .map(|id| (id, self.clips.remove(&id).expect("just found")))
            .collect();
        let lanes = lane_ids
            .into_iter()
            .map(|id| (id, self.lanes.remove(&id).expect("just found")))
            .collect();
        (clips, lanes)
    }

    /// The project as RON, the text format project files use: it's readable
    /// and diffs well.
    pub fn to_ron(&self) -> String {
        let file = ProjectFile {
            format: Self::FORMAT,
            graph: self.graph.clone(),
            frames: self.frames.clone(),
            tempo_map: self.tempo_map.clone(),
            clips: self.clips.clone(),
            lanes: self.lanes.clone(),
            // Not IDs that have gone: a later node could be handed the same ID.
            track_order: self
                .track_order
                .iter()
                .copied()
                .filter(|id| self.graph.node(*id).is_some())
                .collect(),
        };
        // Depth 3 puts each node and each connection on its own line.
        let pretty = ron::ser::PrettyConfig::default().depth_limit(3);
        ron::ser::to_string_pretty(&file, pretty).expect("projects always serialize")
    }

    pub fn from_ron(text: &str) -> Result<Self, LoadError> {
        let file: ProjectFile = ron::from_str(text).map_err(LoadError::Invalid)?;
        if file.format > Self::FORMAT {
            return Err(LoadError::NewerFormat(file.format));
        }
        let next_frame_id = file.frames.keys().last().map_or(1, |id| id.0 + 1);
        let next_clip_id = file.clips.keys().last().map_or(1, |id| id.0 + 1);
        let next_lane_id = file.lanes.keys().last().map_or(1, |id| id.0 + 1);
        let mut clips = file.clips;
        for clip in clips.values_mut() {
            clip.assign_note_ids();
        }
        let project = Self {
            graph: file.graph,
            frames: file.frames,
            next_frame_id,
            tempo_map: file.tempo_map,
            clips,
            next_clip_id,
            lanes: file.lanes,
            next_lane_id,
            track_order: file.track_order,
        };
        for (id, clip) in &project.clips {
            project.check_clip(*id, clip).map_err(LoadError::Broken)?;
        }
        for (id, lane) in &project.lanes {
            project.check_lane(*id, lane).map_err(LoadError::Broken)?;
        }
        Ok(project)
    }
}

#[derive(Debug)]
pub enum LoadError {
    /// Bad syntax, or contents that don't make sense, such as a connection to
    /// a node that doesn't exist.
    Invalid(ron::error::SpannedError),
    /// A clip or lane that can't be in a project, such as one for a node that
    /// doesn't exist.
    Broken(EditError),
    /// Saved by a newer version of Noodle.
    NewerFormat(u32),
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(error) => error.fmt(f),
            Self::Broken(error) => error.fmt(f),
            Self::NewerFormat(format) => write!(
                f,
                "this project was saved in format {format} by a newer version; \
                 this version reads up to format {}",
                Project::FORMAT
            ),
        }
    }
}

impl std::error::Error for LoadError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Command, Config, Connection, Endpoint, Frame, FrameId, History, Node, NodeId, Position,
        Value,
    };

    const HAND_WRITTEN: &str = r#"
        (
            format: 1,
            graph: (
                nodes: {
                    1: (type: "noodle.osc.sine", params: {"frequency": 220.0}),
                    2: (type: "noodle.util.mix", config: {"inputs": 3}, position: (x: 200.0, y: 0.0)),
                },
                connections: [
                    (from: (node: 1, port: "out"), to: (node: 2, port: "in1")),
                ],
            ),
        )
    "#;

    #[test]
    fn a_project_with_nans_equals_its_copy() {
        let mut project = Project::from_ron(HAND_WRITTEN).unwrap();
        let mut history = History::new();
        let frame = project.new_frame_id();
        let node = Node::new("noodle.util.gain")
            .with_param("gain", f32::NAN)
            .with_config(Config::new().with("x", Value::Float(f64::NAN)))
            .at(f32::NAN, 0.0);
        let id = project.new_node_id();
        history
            .apply(&mut project, Command::AddNode { id, node })
            .unwrap();
        let frame_value = Frame {
            label: "f".into(),
            position: Position {
                x: 0.0,
                y: f32::NAN,
            },
            width: f32::NAN,
            height: 1.0,
        };
        history
            .apply(
                &mut project,
                Command::AddFrame {
                    id: frame,
                    frame: frame_value,
                },
            )
            .unwrap();
        assert_eq!(project, project.clone());
    }

    #[test]
    fn reads_a_hand_written_file() {
        let project = Project::from_ron(HAND_WRITTEN).unwrap();
        let graph = project.graph();
        let sine = graph.node(NodeId(1)).unwrap();
        assert_eq!(sine.params["frequency"], 220.0);
        let mix = graph.node(NodeId(2)).unwrap();
        assert_eq!(mix.config.get("inputs"), Some(&Value::Int(3)));
        assert_eq!(mix.position.x, 200.0);
        assert_eq!(
            graph.source(&Endpoint::new(NodeId(2), "in1")),
            Some(&Endpoint::new(NodeId(1), "out"))
        );
    }

    #[test]
    fn round_trips_through_ron() {
        let mut project = Project::from_ron(HAND_WRITTEN).unwrap();
        let mut history = History::new();
        let id = project.new_node_id();
        let node = Node::new("noodle.util.gain")
            .with_param("gain", -6.0)
            .with_config(Config::new().with("flag", Value::Bool(true)))
            .at(1.5, -2.0);
        history
            .apply(&mut project, Command::AddNode { id, node })
            .unwrap();
        history
            .apply(
                &mut project,
                Command::Connect(Connection {
                    from: Endpoint::new(NodeId(2), "out"),
                    to: Endpoint::new(id, "in"),
                }),
            )
            .unwrap();

        let frame = Frame {
            label: "Drums".into(),
            position: Position { x: -20.0, y: -40.0 },
            width: 400.0,
            height: 250.5,
        };
        let frame_id = project.new_frame_id();
        history
            .apply(
                &mut project,
                Command::AddFrame {
                    id: frame_id,
                    frame,
                },
            )
            .unwrap();

        let text = project.to_ron();
        let loaded = Project::from_ron(&text).unwrap();
        assert_eq!(loaded, project, "{text}");
        assert!(loaded.frame(frame_id).is_some(), "{text}");
    }

    #[test]
    fn new_frame_ids_follow_the_highest_loaded_id() {
        let text = HAND_WRITTEN.trim_end().trim_end_matches(')').to_string()
            + r#"frames: { 4: (label: "A", position: (x: 0.0, y: 0.0), width: 10.0, height: 10.0) }, )"#;
        let mut project = Project::from_ron(&text).unwrap();
        assert_eq!(project.frame(FrameId(4)).unwrap().label, "A");
        assert_eq!(project.new_frame_id(), FrameId(5));
    }

    #[test]
    fn new_ids_follow_the_highest_loaded_id() {
        let mut project = Project::from_ron(HAND_WRITTEN).unwrap();
        assert_eq!(project.new_node_id(), NodeId(3));
    }

    #[test]
    fn rejects_a_connection_to_a_missing_node() {
        let text = HAND_WRITTEN.replace("node: 2, port", "node: 7, port");
        let error = Project::from_ron(&text).unwrap_err().to_string();
        assert!(error.contains("there's no node #7"), "{error}");
    }

    #[test]
    fn rejects_an_input_with_two_connections() {
        let text = HAND_WRITTEN.replace(
            "connections: [",
            r#"connections: [ (from: (node: 2, port: "out"), to: (node: 2, port: "in1")),"#,
        );
        let error = Project::from_ron(&text).unwrap_err().to_string();
        assert!(error.contains("more than one connection"), "{error}");
    }

    #[test]
    fn rejects_a_newer_format() {
        let text = HAND_WRITTEN.replace("format: 1", "format: 99");
        assert!(matches!(
            Project::from_ron(&text),
            Err(LoadError::NewerFormat(99))
        ));
    }

    // The timeline: tempo map, clips and automation lanes.

    use crate::{
        AudioClip, AutomationLane, AutomationPoint, Clip, ClipContent, ClipId, Curve, LaneId,
        SignatureChange, TempoChange, TempoMap, Tick, TimeSignature,
    };

    fn apply(project: &mut Project, history: &mut History, command: Command) {
        history.apply(project, command).unwrap();
    }

    /// A project with a player node, a clip on it and a lane driving it.
    fn with_timeline() -> (Project, History, NodeId, ClipId, LaneId) {
        let mut project = Project::new();
        let mut history = History::new();
        let player = project.new_node_id();
        apply(
            &mut project,
            &mut history,
            Command::AddNode {
                id: player,
                node: Node::new("noodle.clip.node"),
            },
        );
        let clip = project.new_clip_id();
        apply(
            &mut project,
            &mut history,
            Command::AddClip {
                id: clip,
                clip: Clip::audio(player, Tick(960), "kick.wav", 48_000),
            },
        );
        let lane = project.new_lane_id();
        let points = vec![AutomationPoint {
            tick: Tick(0),
            value: 0.5,
            curve: Curve::Linear,
        }];
        apply(
            &mut project,
            &mut history,
            Command::AddLane {
                id: lane,
                lane: AutomationLane::new(Endpoint::new(player, "gain"), points),
            },
        );
        (project, history, player, clip, lane)
    }

    #[test]
    fn the_timeline_round_trips_through_ron() {
        let (mut project, mut history, player, clip, _) = with_timeline();
        let map = TempoMap::new(
            vec![
                TempoChange {
                    tick: Tick(0),
                    bpm: 96.5,
                },
                TempoChange {
                    tick: Tick(7680),
                    bpm: 140.0,
                },
            ],
            vec![SignatureChange {
                bar: 0,
                signature: TimeSignature {
                    numerator: 7,
                    denominator: 8,
                },
            }],
        )
        .unwrap();
        apply(&mut project, &mut history, Command::SetTempoMap(map));
        let text = project.to_ron();
        let loaded = Project::from_ron(&text).unwrap();
        assert_eq!(loaded, project);
        assert_eq!(loaded.clip(clip).unwrap().node, player);
        assert_eq!(loaded.to_ron(), text);
        // New IDs carry on after the loaded ones.
        assert_eq!(loaded.next_clip_id(), ClipId(clip.0 + 1));
    }

    #[test]
    fn a_project_without_a_timeline_writes_none_and_reads_old_files() {
        let text = Project::new().to_ron();
        assert!(!text.contains("tempo_map"), "{text}");
        assert!(!text.contains("clips"), "{text}");
        // A format 1 file, from before the timeline.
        let project = Project::from_ron(HAND_WRITTEN).unwrap();
        assert_eq!(*project.tempo_map(), TempoMap::default());
        assert_eq!(project.clips().count(), 0);
        assert_eq!(project.lanes().count(), 0);
    }

    #[test]
    fn a_clip_or_lane_for_a_missing_node_is_refused_on_load() {
        let (project, ..) = with_timeline();
        let text = project.to_ron();
        assert!(Project::from_ron(&text).is_ok());
        // Not `"node: 1,\n"`: RON writes `\r\n` on Windows.
        let no_player = text.replace("node: 1,", "node: 9,");
        assert!(matches!(
            Project::from_ron(&no_player),
            Err(LoadError::Broken(EditError::NoSuchNode(NodeId(9))))
        ));
        let no_target = text.replace("node: 1", "node: 9");
        assert!(matches!(
            Project::from_ron(&no_target),
            Err(LoadError::Broken(EditError::NoSuchNode(NodeId(9))))
        ));
    }

    #[test]
    fn removing_a_node_takes_its_clips_and_lanes_and_undo_brings_them_back() {
        let (mut project, mut history, player, clip, lane) = with_timeline();
        let before = project.clone();
        apply(
            &mut project,
            &mut history,
            Command::RemoveNode { id: player },
        );
        assert!(project.clip(clip).is_none());
        assert!(project.lane(lane).is_none());
        assert_eq!(project.clips_on(player).count(), 0);

        history.undo(&mut project).unwrap();
        assert_eq!(project, before);
        assert_eq!(
            project.clip(clip).unwrap().as_audio().unwrap().source,
            "kick.wav"
        );
        history.redo(&mut project).unwrap();
        assert!(project.clip(clip).is_none());
    }

    #[test]
    fn midi_clips_are_checked_and_survive_a_save() {
        use crate::{MidiClip, MidiNote};
        let (mut project, _, player, _, _) = with_timeline();
        let id = project.new_clip_id();
        let mut good = Clip::midi(player, Tick(960), Tick(3840));
        let ClipContent::Midi(midi) = &mut good.content else {
            unreachable!("not a MIDI clip")
        };
        midi.notes.push(MidiNote::new(Tick(0), Tick(480), 60));
        midi.notes.push(MidiNote::new(Tick(960), Tick(960), 64));
        let add = |project: &mut Project, id, clip| Command::AddClip { id, clip }.apply(project);
        let bad = |change: &dyn Fn(&mut MidiClip)| {
            let mut clip = good.clone();
            let ClipContent::Midi(midi) = &mut clip.content else {
                unreachable!()
            };
            change(midi);
            clip
        };
        for clip in [
            bad(&|m| m.length = Tick(0)),
            bad(&|m| m.notes[0].length = Tick(0)),
            bad(&|m| m.notes[0].start = Tick(-1)),
            bad(&|m| m.notes[0].key = 128),
            bad(&|m| m.notes[0].velocity = 1.5),
            bad(&|m| m.notes[0].velocity = f32::NAN),
        ] {
            assert!(matches!(
                add(&mut project, id, clip),
                Err(EditError::InvalidClip(..))
            ));
        }
        add(&mut project, id, good.clone()).unwrap();
        assert!(project.clip(id).unwrap().as_audio().is_none());
        assert_eq!(project.clip(id).unwrap().as_midi().unwrap().notes.len(), 2);
        let loaded = Project::from_ron(&project.to_ron()).unwrap();
        // The project gave the second note an id of its own.
        assert_eq!(loaded.clip(id), project.clip(id));
        let notes = &project.clip(id).unwrap().as_midi().unwrap().notes;
        assert_ne!(notes[0].id, notes[1].id);
    }

    #[test]
    fn clips_are_checked_when_added_or_changed() {
        let (mut project, _, player, clip, _) = with_timeline();
        let add = |project: &mut Project, id, clip| Command::AddClip { id, clip }.apply(project);
        let id = project.new_clip_id();
        let good = Clip::audio(player, Tick(0), "a.wav", 10);

        assert_eq!(
            add(&mut project, clip, good.clone()),
            Err(EditError::ClipExists(clip))
        );
        let ghost = Clip::audio(NodeId(77), Tick(0), "a.wav", 10);
        assert_eq!(
            add(&mut project, id, ghost),
            Err(EditError::NoSuchNode(NodeId(77)))
        );
        let tweak = |change: &dyn Fn(&mut AudioClip)| {
            let mut clip = good.clone();
            let ClipContent::Audio(audio) = &mut clip.content else {
                unreachable!("not an audio clip")
            };
            change(audio);
            clip
        };
        for bad in [
            Clip::audio(player, Tick(-1), "a.wav", 10),
            Clip::audio(player, Tick(0), "a.wav", 0),
            Clip::audio(player, Tick(0), "", 10),
            tweak(&|a| a.gain = f32::NAN),
            tweak(&|a| a.gain = -1.0),
            tweak(&|a| {
                a.fade_in = 6;
                a.fade_out = 5;
            }),
        ] {
            assert!(
                matches!(
                    add(&mut project, id, bad.clone()),
                    Err(EditError::InvalidClip(..))
                ),
                "{bad:?}"
            );
            let set = Command::SetClip {
                id: clip,
                clip: bad,
            }
            .apply(&mut project);
            assert!(matches!(set, Err(EditError::InvalidClip(..))));
        }
        assert_eq!(
            project.clip(clip).unwrap().as_audio().unwrap().source,
            "kick.wav",
            "unchanged"
        );
        assert!(add(&mut project, id, good).is_ok());
        assert_eq!(
            Command::RemoveClip { id: ClipId(99) }.apply(&mut project),
            Err(EditError::NoSuchClip(ClipId(99)))
        );
    }

    #[test]
    fn one_lane_drives_an_input() {
        let (mut project, _, player, _, lane) = with_timeline();
        let other = project.new_lane_id();
        let same_input = AutomationLane::new(Endpoint::new(player, "gain"), vec![]);
        assert_eq!(
            Command::AddLane {
                id: other,
                lane: same_input
            }
            .apply(&mut project),
            Err(EditError::LaneTargetTaken(
                Endpoint::new(player, "gain"),
                lane
            ))
        );
        // Another input is fine, and so is the lane replacing itself.
        let elsewhere = AutomationLane::new(Endpoint::new(player, "pan"), vec![]);
        Command::AddLane {
            id: other,
            lane: elsewhere,
        }
        .apply(&mut project)
        .unwrap();
        let again = project.lane(lane).unwrap().clone();
        Command::SetLane {
            id: lane,
            lane: again,
        }
        .apply(&mut project)
        .unwrap();
        // But it can't be moved onto the other's input.
        let onto = AutomationLane::new(Endpoint::new(player, "pan"), vec![]);
        assert!(matches!(
            Command::SetLane {
                id: lane,
                lane: onto
            }
            .apply(&mut project),
            Err(EditError::LaneTargetTaken(..))
        ));
        assert_eq!(
            project.lane_for(&Endpoint::new(player, "gain")).unwrap().0,
            lane
        );
    }

    #[test]
    fn dragging_a_clip_is_one_undo_step() {
        let (mut project, mut history, _, clip, _) = with_timeline();
        let original = project.clip(clip).unwrap().clone();
        history.begin_group();
        for tick in [1000, 1100, 1200] {
            let moved = Clip {
                start: Tick(tick),
                ..original.clone()
            };
            apply(
                &mut project,
                &mut history,
                Command::SetClip {
                    id: clip,
                    clip: moved,
                },
            );
        }
        history.end_group();
        assert_eq!(project.clip(clip).unwrap().start, Tick(1200));
        history.undo(&mut project).unwrap();
        assert_eq!(project.clip(clip).unwrap(), &original);
    }

    #[test]
    fn a_tempo_change_undoes() {
        let (mut project, mut history, ..) = with_timeline();
        let fast = TempoMap::constant(180.0, TimeSignature::COMMON).unwrap();
        apply(
            &mut project,
            &mut history,
            Command::SetTempoMap(fast.clone()),
        );
        assert_eq!(*project.tempo_map(), fast);
        history.undo(&mut project).unwrap();
        assert_eq!(*project.tempo_map(), TempoMap::default());
    }

    #[test]
    fn a_track_order_undoes_saves_and_sorts() {
        let (mut project, mut history, first, ..) = with_timeline();
        let mut ids = vec![first];
        for _ in 0..2 {
            let id = project.new_node_id();
            let node = Node::new("noodle.util.gain");
            apply(&mut project, &mut history, Command::AddNode { id, node });
            ids.push(id);
        }
        let (a, b) = (ids[1], ids[2]);
        let order = vec![b, a];
        apply(
            &mut project,
            &mut history,
            Command::SetTrackOrder(order.clone()),
        );
        let mut groups = [first, a, b];
        project.sort_tracks(&mut groups);
        // Named groups first, in the stored order; the rest by ID.
        assert_eq!(groups, [b, a, first]);
        let loaded = Project::from_ron(&project.to_ron()).unwrap();
        assert_eq!(loaded.track_order(), order);
        history.undo(&mut project).unwrap();
        assert!(project.track_order().is_empty());
        assert!(!project.to_ron().contains("track_order"));
    }

    #[test]
    fn saving_drops_ids_of_nodes_that_have_gone_from_the_track_order() {
        let (mut project, mut history, ..) = with_timeline();
        let live = project.graph().nodes().next().unwrap().0;
        let order = vec![NodeId(9999), live];
        apply(&mut project, &mut history, Command::SetTrackOrder(order));
        let loaded = Project::from_ron(&project.to_ron()).unwrap();
        assert_eq!(loaded.track_order(), [live]);
    }

    #[test]
    fn midi_notes_get_unique_ids_when_a_clip_enters_the_project() {
        use crate::MidiNote;
        let (mut project, mut history, player, _, _) = with_timeline();
        let mut clip = Clip::midi(player, Tick(0), Tick(3840));
        let ClipContent::Midi(midi) = &mut clip.content else {
            unreachable!()
        };
        // All zeros, as notes from a file saved before ids existed are.
        midi.notes = (0..3)
            .map(|i| MidiNote::new(Tick(i * 240), Tick(240), 60))
            .collect();
        midi.notes[2].id = 1;
        let id = project.new_clip_id();
        apply(
            &mut project,
            &mut history,
            Command::AddClip {
                id,
                clip: clip.clone(),
            },
        );
        let ids = |project: &Project| -> Vec<u32> {
            project
                .clip(id)
                .and_then(Clip::as_midi)
                .unwrap()
                .notes
                .iter()
                .map(|n| n.id)
                .collect()
        };
        let mut unique = ids(&project);
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), 3, "{:?}", ids(&project));
        // A note whose id was already unique keeps it.
        assert_eq!(ids(&project)[2], 1);
        // Ids survive a save and a load, and an edit that leaves them alone.
        let loaded = Project::from_ron(&project.to_ron()).unwrap();
        assert_eq!(ids(&loaded), ids(&project));
        // A file from before notes had ids has none to read.
        let note: MidiNote =
            ron::from_str("(start: 0, length: 240, key: 60, velocity: 0.5)").unwrap();
        assert_eq!(note.id, 0);
    }

    #[test]
    fn a_note_id_at_the_top_of_the_range_does_not_overflow() {
        use crate::MidiNote;
        let mut midi = crate::MidiClip::new(Tick(960));
        midi.notes = vec![MidiNote::new(Tick(0), Tick(10), 60); 2];
        midi.notes[0].id = u32::MAX;
        midi.notes[1].id = u32::MAX;
        midi.assign_note_ids();
        assert_ne!(midi.notes[0].id, midi.notes[1].id);
    }
}
