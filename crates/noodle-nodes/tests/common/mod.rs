//! A track input node on its own, driven block by block with a project's
//! clips fed to it.

// Each test binary uses some of this.
#![allow(dead_code)]

use std::path::PathBuf;
use std::time::{Duration, Instant};

use noodle_core::{Clip, ClipId, Command, History, Node as GraphNode, NodeId, Project, Tick};
use noodle_engine::{
    Config, Context, EventsOut, Instance, Io, Node, NodeType, Setup, Shape, SignalBuffer,
    TempoTable, Transport,
};
use noodle_io::write_wav;
use noodle_nodes::{ClipFeeds, ClipProblem, TRACK_INPUT_ID, TrackInput};

pub const RATE: u32 = 48_000;
pub const BLOCK: usize = 256;
/// One quarter note at 120 beats per minute.
pub const BEAT: u64 = 24_000;

pub struct Rig {
    pub node: Box<dyn Node>,
    pub feeds: ClipFeeds,
    pub id: NodeId,
    pub project: Project,
    pub history: History,
    pub dir: PathBuf,
    pub problems: Vec<ClipProblem>,
    /// The loop in samples, handed to the node with every block.
    pub looping: Option<(u64, u64)>,
    /// The node waits for the hub (see [`ClipFeeds::blocking`]), so there is
    /// nothing to wait out.
    blocking: bool,
    out: SignalBuffer,
    midi: Vec<noodle_engine::Event>,
}

impl Rig {
    pub fn new(name: &str) -> Self {
        Self::with_feeds(name, ClipFeeds::default())
    }

    /// A rig whose node waits for the hub's schedule and the disk instead of
    /// playing silence when they are late, so what it plays doesn't depend
    /// on how busy the machine is.
    pub fn blocking(name: &str) -> Self {
        let mut rig = Self::with_feeds(name, ClipFeeds::blocking());
        rig.blocking = true;
        rig
    }

    pub fn with_feeds(name: &str, feeds: ClipFeeds) -> Self {
        let dir = std::env::temp_dir().join(format!("noodle-track-{name}"));
        std::fs::create_dir_all(&dir).unwrap();
        let mut project = Project::new();
        let mut history = History::new();
        let id = project.new_node_id();
        history
            .apply(
                &mut project,
                Command::AddNode {
                    id,
                    node: GraphNode::new(TRACK_INPUT_ID),
                },
            )
            .unwrap();
        let node_type = TrackInput::new(&feeds);
        let config = Config::new();
        let instance = node_type
            .instantiate(&Setup {
                node: id,
                config: &config,
                sample_rate: RATE as f32,
                max_frames: BLOCK,
                input_shapes: &[],
                output_shapes: &[Shape::STEREO],
                seed: 0,
            })
            .unwrap();
        let Instance::Realtime(node) = instance else {
            panic!("the track input is a real-time node")
        };
        Self {
            node,
            feeds,
            id,
            project,
            history,
            dir,
            problems: Vec::new(),
            looping: None,
            blocking: false,
            out: SignalBuffer::new(Shape::STEREO, BLOCK),
            midi: Vec::with_capacity(16),
        }
    }

    /// Writes a 48 kHz WAV of `frames` frames, each channel's sample given
    /// by `f(channel, frame)`.
    pub fn wav(&self, name: &str, channels: usize, frames: usize, f: impl Fn(usize, usize) -> f32) {
        let samples: Vec<f32> = (0..frames)
            .flat_map(|i| (0..channels).map(|c| f(c, i)).collect::<Vec<_>>())
            .collect();
        write_wav(&self.dir.join(name), &samples, channels, RATE).unwrap();
    }

    pub fn add_clip(&mut self, start_beats: i64, file: &str, offset: u64, length: u64) -> ClipId {
        let id = self.project.new_clip_id();
        let mut clip = Clip::audio(self.id, Tick(start_beats * 960), file, length);
        let noodle_core::ClipContent::Audio(audio) = &mut clip.content else {
            unreachable!("not an audio clip")
        };
        audio.offset = offset;
        self.history
            .apply(&mut self.project, Command::AddClip { id, clip })
            .unwrap();
        self.update();
        id
    }

    /// Adds a MIDI clip of `beats` beats at `start_beats`, with notes given
    /// as (start tick, length in ticks, key) from the clip's start.
    pub fn add_midi_clip(
        &mut self,
        start_beats: i64,
        beats: i64,
        notes: &[(i64, i64, u8)],
    ) -> ClipId {
        let id = self.project.new_clip_id();
        let mut clip = Clip::midi(self.id, Tick(start_beats * 960), Tick(beats * 960));
        let noodle_core::ClipContent::Midi(midi) = &mut clip.content else {
            unreachable!("not a MIDI clip")
        };
        midi.notes = notes
            .iter()
            .map(|&(start, length, key)| noodle_core::MidiNote::new(Tick(start), Tick(length), key))
            .collect();
        self.history
            .apply(&mut self.project, Command::AddClip { id, clip })
            .unwrap();
        self.update();
        id
    }

    /// The events the last block produced.
    pub fn events(&self) -> Vec<noodle_engine::Event> {
        self.midi.clone()
    }

    /// Runs one block and returns its events with their time moved to the
    /// timeline (in samples).
    pub fn run_events(
        &mut self,
        position: u64,
        frames: usize,
        playing: bool,
    ) -> Vec<(u64, noodle_engine::EventKind)> {
        self.run_quiet(position, frames, playing);
        self.midi
            .iter()
            .map(|e| (position + u64::from(e.time), e.kind))
            .collect()
    }

    /// Changes a clip and feeds the result to the node.
    pub fn edit_clip(&mut self, id: ClipId, change: impl FnOnce(&mut Clip)) {
        let mut clip = self.project.clip(id).unwrap().clone();
        change(&mut clip);
        self.history
            .apply(&mut self.project, Command::SetClip { id, clip })
            .unwrap();
        self.update();
    }

    /// Waits for the hub to hand the node the schedule from the last update.
    /// The hub is a background thread, so a loaded CI machine (macOS runners
    /// in particular) can take much longer than a quiet one.
    ///
    /// A blocking rig (see [`Rig::blocking`]) needs no wait.
    pub fn settle(&self) {
        if self.blocking {
            return;
        }
        std::thread::sleep(Duration::from_millis(400));
    }

    pub fn update(&mut self) {
        self.problems = self.feeds.update(
            &self.project,
            &TempoTable::new(self.project.tempo_map(), RATE as f32),
            RATE,
            &self.dir,
        );
    }

    /// Runs one block and returns left and right.
    pub fn run(&mut self, position: u64, frames: usize, playing: bool) -> (Vec<f32>, Vec<f32>) {
        self.run_quiet(position, frames, playing);
        let out = self.out.as_in(frames);
        (out.lane(0, 0).to_vec(), out.lane(0, 1).to_vec())
    }

    /// Runs one block, keeping the output in the rig. Does not allocate, so
    /// it can run under the real-time check.
    pub fn run_quiet(&mut self, position: u64, frames: usize, playing: bool) {
        let ctx = Context {
            sample_rate: RATE as f32,
            frames,
            transport: Transport {
                playing,
                position,
                loop_range: self.looping,
                ..Transport::default()
            },
        };
        let mut outputs = [self.out.as_out(frames)];
        let mut events = [EventsOut::new(&mut self.midi)];
        self.node.process(
            &ctx,
            Io {
                inputs: &[],
                outputs: &mut outputs,
                event_inputs: &[],
                event_outputs: &mut events,
            },
        );
    }

    /// Plays from `from` to `to` in blocks and returns the whole output.
    pub fn play(&mut self, from: u64, to: u64) -> (Vec<f32>, Vec<f32>) {
        let (mut left, mut right) = (Vec::new(), Vec::new());
        let mut at = from;
        while at < to {
            let n = BLOCK.min((to - at) as usize);
            let (l, r) = self.run(at, n, true);
            left.extend(l);
            right.extend(r);
            at += n as u64;
            // The test runs faster than real time; let the disk keep up.
            std::thread::sleep(Duration::from_millis(1));
        }
        (left, right)
    }

    /// Plays from `from` for `frames` frames, wrapping at the end of the loop
    /// the way the engine does (a block never crosses it), and returns the
    /// whole output.
    pub fn play_looping(&mut self, from: u64, frames: usize) -> (Vec<f32>, Vec<f32>) {
        let (start, end) = self.looping.expect("set `looping` first");
        let (mut left, mut right) = (Vec::new(), Vec::new());
        let mut at = from;
        while left.len() < frames {
            let n = BLOCK.min((end - at) as usize).min(frames - left.len());
            let (l, r) = self.run(at, n, true);
            left.extend(l);
            right.extend(r);
            at += n as u64;
            if at == end {
                at = start;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        (left, right)
    }

    /// Stopped at `position`, waits until the node has `streams` ready.
    pub fn wait_ready(&mut self, position: u64, streams: usize) {
        let started = Instant::now();
        while self.feeds.status(self.id).streams < streams {
            self.run(position, BLOCK, false);
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "streams never became ready: {:?}",
                self.feeds.status(self.id)
            );
            std::thread::sleep(Duration::from_millis(2));
        }
        // Give the stream's worker a moment to fill its queue.
        std::thread::sleep(Duration::from_millis(100));
        self.run(position, BLOCK, false);
    }
}
