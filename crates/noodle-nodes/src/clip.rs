//! The track input node: plays a track's audio clips along the transport.
//!
//! It has two outputs, `audio` (stereo) and `midi` (the notes of the track's
//! MIDI clips, as events). The clips come from the project, not from
//! the graph: [`ClipFeeds::update`] turns them into a schedule of sample
//! positions and hands it to the node without rebuilding it, so a clip edit
//! doesn't cut a clip that is playing off. A hub thread (see `hub.rs`) opens
//! the audio streams ahead of the playhead, so nothing in
//! [`Node::process`] opens a file, allocates, frees or waits.
//!
//! What plays at each sample follows the rule in `docs/ARCHITECTURE.md`: one
//! audio clip at a time, the one that started last, with no crossfade but the
//! clip's own fades. On top of that the node declicks itself: it fades out
//! when the transport stops, fades in when it starts or jumps, and dips (out
//! and back in over about 5 ms) when an edit changes what is heard at the
//! playhead.

mod ahead;
mod hub;
mod schedule;
mod stretch;
mod warp;

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use noodle_core::ClipId;
use noodle_engine::{
    Config, Context, Event, EventKind, Instance, Io, Layout, Node, NodeError, NodeInfo, NodeType,
    NoteId, ParamInfo, Setup, Shape, Unit,
};
use noodle_io::{ClipStream, MidiBus};
use rtrb::{Consumer, Producer, PushError};

use ahead::{AheadLinks, Lane, MAX_LANES};
use hub::{Links, MAX_CHANNELS, MAX_HEADS, MAX_STREAMS, Prepared, Retired, Shared, ToNode};
pub use schedule::{
    ClipFeeds, ClipProblem, ClipSource, ClipStatus, FileError, Notes, Schedule, ScheduledClip,
    ScheduledNote, WarpMap, active_at,
};
use schedule::{Segments, same_sound};
pub use stretch::{Source as StretchSource, Stretcher};
use warp::cubic;

use crate::midi_in::LiveMidi;

pub const TRACK_INPUT_ID: &str = "noodle.track.input";

const AUDIO: usize = 0;
const MIDI: usize = 0;
/// The track's gain and mute: parameters (inputs 0 and 1), passed on as
/// outputs 1 and 2 for the track's group output to take.
const GAIN_IN: usize = 0;
const MUTE_IN: usize = 1;
const GAIN_OUT: usize = 1;
const MUTE_OUT: usize = 2;
/// The most MIDI notes one track sounds at once; more are dropped.
const MAX_SOUNDING: usize = 128;
/// The note IDs of keys auditioned from the piano roll; see
/// [`CLIP_NOTE_MASK`].
const AUDITION_ID: u32 = 0x4000_0000;
const FADE_SECONDS: f32 = 0.005;
/// Offline, how often the node looks again while it waits for the hub or the
/// disk, and how long it waits before giving up on them.
const WAIT_STEP: Duration = Duration::from_micros(50);
const WAIT_LIMIT: Duration = Duration::from_secs(10);
/// How far behind a stream may be and still catch up by reading and dropping
/// audio, which is quicker than seeking.
const MAX_SKIP: u64 = 8192;
/// Source frames a warped clip keeps in hand to interpolate between: the
/// most any one stretch of output may reach over.
const WINDOW_FRAMES: usize = 8192;
const MAX_SPAN: f64 = (WINDOW_FRAMES - 16) as f64;
/// Warped clips are played in pieces of at most this many output frames.
const WARP_CHUNK: usize = 256;

static INFO: NodeInfo = NodeInfo {
    id: TRACK_INPUT_ID,
    version: 2,
    name: "Track Input",
    category: "Track",
};

/// The note IDs of live monitoring; see [`TrackInput::with_midi`].
const LIVE_ID: u32 = 0x2000_0000;

/// Registered by [`register_library`](crate::register_library).
pub struct TrackInput {
    feeds: ClipFeeds,
    midi: Option<MidiBus>,
}

impl TrackInput {
    pub fn new(feeds: &ClipFeeds) -> Self {
        Self {
            feeds: feeds.clone(),
            midi: None,
        }
    }

    /// A track input that, while its track is monitored (see
    /// [`ClipFeeds::set_monitoring`]), also plays what arrives on `bus`, from
    /// every source on every channel, whatever the transport is doing. A MIDI
    /// In node wired to the same synth plays the notes a second time: the
    /// two make their own note IDs.
    pub fn with_midi(feeds: &ClipFeeds, bus: &MidiBus) -> Self {
        Self {
            midi: Some(bus.clone()),
            ..Self::new(feeds)
        }
    }
}

impl NodeType for TrackInput {
    fn always_runs(&self) -> bool {
        true
    }

    fn info(&self) -> &NodeInfo {
        &INFO
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(Layout::realtime()
            .param(
                "gain",
                "Gain",
                ParamInfo::new(-60.0, 24.0, 0.0).unit(Unit::Decibels),
            )
            .param("mute", "Mute", ParamInfo::new(0.0, 1.0, 0.0))
            .output("audio", "Audio")
            .output("gain", "Gain")
            .output("mute", "Mute")
            .event_output("midi", "MIDI"))
    }

    fn output_shapes(
        &self,
        _config: &Config,
        _layout: &Layout,
        _inputs: &[Shape],
    ) -> Result<Vec<Shape>, NodeError> {
        Ok(vec![Shape::STEREO, Shape::MONO, Shape::MONO])
    }

    fn cache_inputs(
        &self,
        node: noodle_core::NodeId,
        _config: &Config,
        env: &noodle_engine::CacheEnv<'_>,
        key: &mut noodle_core::KeyBuilder,
    ) -> Result<(), noodle_engine::Uncacheable> {
        // The clips live in the project, not in the graph, so they are keyed
        // here: where each one starts, what it plays, and the contents of its
        // file, since a file replaced at the same path must give a new key.
        for (id, clip) in env.project.clips_on(node) {
            key.u64(id.0).i64(clip.start.0);
            match &clip.content {
                noodle_core::ClipContent::Audio(audio) if audio.muted => {
                    // Silent, so its file doesn't matter, only that it is.
                    key.str("muted");
                }
                noodle_core::ClipContent::Audio(audio) => {
                    let file = (env.file_key)(&audio.source).ok_or_else(|| {
                        noodle_engine::Uncacheable::Because(format!("can't read {}", audio.source))
                    })?;
                    key.str("audio")
                        .key(&file)
                        .u64(audio.offset)
                        .u64(audio.length)
                        .f32(audio.gain)
                        .u64(audio.fade_in)
                        .u64(audio.fade_out);
                    // Only a warp that changes the sound is keyed, so the
                    // keys of clips that aren't warped stay as they were.
                    if let Some(warp) = audio
                        .warp
                        .and_then(|id| env.project.warp(id))
                        .filter(|warp| !warp.is_identity())
                    {
                        // Repitch is left out, so keys made before there
                        // were other modes still match.
                        if warp.mode.keeps_pitch() {
                            key.str("mode").str(&format!("{:?}", warp.mode));
                        }
                        key.str("warp")
                            .bool(warp.tempo_conversion)
                            .bool(warp.use_markers)
                            .u64(warp.tempo.points().len() as u64);
                        for point in warp.tempo.points() {
                            key.f64(point.seconds).f64(point.bpm);
                        }
                        // The markers as they play, so warp quantise (and the
                        // groove it follows) is in the key.
                        let markers = warp.effective_markers();
                        key.u64(markers.len() as u64);
                        for marker in markers.iter() {
                            key.f64(marker.seconds).f64(marker.beat);
                        }
                    }
                }
                noodle_core::ClipContent::Midi(midi) => {
                    key.str("midi")
                        .i64(midi.length.0)
                        .u64(midi.notes.len() as u64);
                    for note in &midi.notes {
                        key.i64(note.start.0)
                            .i64(note.length.0)
                            .u64(u64::from(note.key))
                            .f32(note.velocity);
                    }
                }
            }
        }
        Ok(())
    }

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        let shared = self.feeds.shared(setup.node);
        let rate = setup.sample_rate.round() as u32;
        let links = hub::spawn(shared.clone(), rate);
        let ahead = ahead::spawn(shared.clone(), rate);
        let mut node =
            TrackInputNode::new(shared, links, ahead, setup.sample_rate, setup.max_frames);
        node.live = self
            .midi
            .as_ref()
            .map(|bus| LiveMidi::new(bus, None, LIVE_ID));
        Ok(Instance::realtime(node))
    }
}

struct TrackInputNode {
    shared: Arc<Shared>,
    from_hub: Consumer<ToNode>,
    to_hub: Producer<Retired>,
    schedule: Option<Box<Schedule>>,
    /// A newer schedule waiting for the dip to reach silence.
    pending: Option<Box<Schedule>>,
    streams: Vec<Option<Box<Prepared>>>,
    /// Stretched sections (Grain and Beats), rendered ahead by a worker.
    lanes: Vec<Option<Box<Lane>>>,
    new_lanes: Consumer<Box<Lane>>,
    spent_lanes: Producer<Box<Lane>>,
    /// Stereo frames taken from a lane for the stretch being played.
    lane_frames: Vec<f32>,
    /// Decoded frames for the stretch being played.
    scratch: Vec<f32>,
    left: Vec<f32>,
    right: Vec<f32>,
    /// Source frames of the warped clip being played: frames
    /// `window_start..window_end` of its stream, interleaved. The stream is
    /// at `window_end`.
    window: Vec<f32>,
    window_start: u64,
    window_end: u64,
    /// The stream (by serial) the window was read from.
    window_serial: Option<u64>,
    /// Whether warped clips are being played as cheap Repitch (see
    /// [`ClipFeeds::set_preview`]), as of the last time the node switched.
    preview: bool,
    /// The preview switch was thrown while sound was playing; the node is
    /// dipping to silence and takes it on there, so the change of sound
    /// doesn't click.
    preview_dip: bool,
    /// The declick gain, from 0 to 1.
    level: f32,
    step: f32,
    /// The version of the newest schedule received.
    version: u64,
    /// The block being rendered: its first sample and its length. Offline
    /// waits need it to keep taking in what the hub sends.
    block: (u64, usize),
    looping: Option<(u64, u64)>,
    /// Offline: clips whose audio never came, which are left silent for the
    /// rest of the render instead of stalling every block they are in.
    abandoned: Vec<(ClipId, u64)>,
    /// A schedule that arrived while the current one was in use.
    deferred: Option<(u64, Box<Schedule>, Box<Notes>)>,
    /// The MIDI notes to play. They take over at once: a note that isn't in
    /// the new set is ended, and nothing else is disturbed.
    notes: Option<Box<Notes>>,
    sounding: Vec<Sounding>,
    /// Where the last block that played ended, to notice a jump.
    played_to: Option<u64>,
    /// New notes arrived since the sounding ones were last checked.
    recheck: bool,
    /// The keys of the piano roll's keyboard that have a note-on out.
    audition_held: [u64; 2],
    /// Live MIDI for monitoring, if the node was given a bus.
    live: Option<LiveMidi>,
}

/// A MIDI note that has started and not yet ended.
#[derive(Clone, Copy)]
struct Sounding {
    id: u32,
    start: u64,
    end: u64,
}

impl TrackInputNode {
    fn new(
        shared: Arc<Shared>,
        links: Links,
        ahead: AheadLinks,
        sample_rate: f32,
        max_frames: usize,
    ) -> Self {
        let fade_len = (sample_rate * FADE_SECONDS).round().max(1.0);
        Self {
            shared,
            from_hub: links.from_hub,
            to_hub: links.to_hub,
            schedule: None,
            pending: None,
            streams: (0..MAX_STREAMS + MAX_HEADS).map(|_| None).collect(),
            lanes: (0..MAX_LANES).map(|_| None).collect(),
            new_lanes: ahead.lanes,
            spent_lanes: ahead.returned,
            lane_frames: vec![0.0; max_frames * 2],
            scratch: vec![0.0; max_frames * MAX_CHANNELS],
            left: vec![0.0; max_frames],
            right: vec![0.0; max_frames],
            window: vec![0.0; WINDOW_FRAMES * MAX_CHANNELS],
            window_start: 0,
            window_end: 0,
            window_serial: None,
            preview: false,
            preview_dip: false,
            level: 0.0,
            step: 1.0 / fade_len,
            version: 0,
            block: (0, 0),
            looping: None,
            abandoned: Vec::new(),
            deferred: None,
            notes: None,
            sounding: Vec::with_capacity(MAX_SOUNDING),
            played_to: None,
            recheck: false,
            audition_held: [0; 2],
            live: None,
        }
    }

    /// Sends something back to the hub to be dropped. The queue is bigger than
    /// anything the hub has sent, so it has room; if that were ever wrong the
    /// item is leaked rather than freed on the audio thread.
    fn retire(&mut self, item: Retired) {
        if let Err(PushError::Full(item)) = self.to_hub.push(item) {
            std::mem::forget(item);
        }
    }

    fn adopt_pending(&mut self) {
        if let Some(next) = self.pending.take()
            && let Some(old) = self.schedule.replace(next)
        {
            self.retire(Retired::Schedule(old));
        }
        self.preview = self.shared.preview.load(Ordering::Relaxed);
        self.preview_dip = false;
    }

    /// Whether the node is running down to silence to take on a new schedule
    /// or the preview switch.
    fn dipping(&self) -> bool {
        self.pending.is_some() || self.preview_dip
    }

    /// Notices the preview switch being thrown. While sound plays the node
    /// dips (about 5 ms out and back in) and switches at the bottom; silent,
    /// it switches at once.
    fn watch_preview(&mut self) {
        let want = self.shared.preview.load(Ordering::Relaxed);
        if want == self.preview {
            return;
        }
        if self.level > 0.0 {
            self.preview_dip = true;
        } else {
            self.preview = want;
            self.preview_dip = false;
        }
    }

    /// Takes what the hub has sent. A new schedule that changes what is heard
    /// over this block waits for the dip; any other takes over at once.
    fn receive(&mut self, position: u64, frames: usize) {
        self.take_lanes();
        if let Some((version, next, notes)) = self.deferred.take() {
            self.take_schedule(version, next, notes, position, frames);
        }
        while let Ok(message) = self.from_hub.pop() {
            match message {
                ToNode::Stream(prepared) => self.take_stream(prepared),
                ToNode::Schedule(version, next, notes) => {
                    self.take_schedule(version, next, notes, position, frames)
                }
            }
        }
    }

    /// Like [`receive`](Self::receive) in the middle of a block, where the
    /// schedule is in use: streams are taken in, and a schedule is kept for
    /// the start of the next block.
    fn receive_streams(&mut self) {
        self.take_lanes();
        while let Ok(message) = self.from_hub.pop() {
            match message {
                ToNode::Stream(prepared) => self.take_stream(prepared),
                ToNode::Schedule(version, next, notes) => {
                    if let Some((_, older, older_notes)) =
                        self.deferred.replace((version, next, notes))
                    {
                        self.retire(Retired::Schedule(older));
                        self.retire(Retired::Notes(older_notes));
                    }
                }
            }
        }
    }

    /// Takes in the lanes the worker has opened. One that has no room is sent
    /// straight back to be dropped there.
    fn take_lanes(&mut self) {
        while let Ok(lane) = self.new_lanes.pop() {
            match self.lanes.iter_mut().find(|slot| slot.is_none()) {
                Some(slot) => *slot = Some(lane),
                None => self.retire_lane(lane),
            }
        }
    }

    /// Like [`retire`](Self::retire), for a lane. If the queue were ever full
    /// the lane is leaked rather than freed on the audio thread.
    fn retire_lane(&mut self, lane: Box<Lane>) {
        if let Err(PushError::Full(lane)) = self.spent_lanes.push(lane) {
            std::mem::forget(lane);
        }
    }

    fn take_stream(&mut self, prepared: Box<Prepared>) {
        match self.streams.iter_mut().find(|slot| slot.is_none()) {
            Some(slot) => *slot = Some(prepared),
            None => self.retire(Retired::Stream(prepared)),
        }
    }

    fn take_schedule(
        &mut self,
        version: u64,
        next: Box<Schedule>,
        notes: Box<Notes>,
        position: u64,
        frames: usize,
    ) {
        self.version = version;
        if let Some(old) = self.notes.replace(notes) {
            self.retire(Retired::Notes(old));
        }
        self.recheck = true;
        let (from, to) = (position, position + frames as u64);
        let audible = self.level > 0.0
            && self
                .schedule
                .as_ref()
                .is_some_and(|now| !same_sound(now, &next, from, to));
        if audible {
            if let Some(older) = self.pending.replace(next) {
                self.retire(Retired::Schedule(older));
            }
        } else {
            // Nothing audible changes, so it can take over now, including
            // over a dip that is waiting.
            if let Some(older) = self.pending.take() {
                self.retire(Retired::Schedule(older));
            }
            if let Some(old) = self.schedule.replace(next) {
                self.retire(Retired::Schedule(old));
            }
        }
    }

    /// Offline: waits until the hub has handed over the newest schedule.
    fn wait_for_schedule(&mut self) {
        let started = Instant::now();
        while self.version != self.shared.version() {
            if started.elapsed() > WAIT_LIMIT {
                self.shared.underruns.fetch_add(1, Ordering::Relaxed);
                return;
            }
            std::thread::sleep(WAIT_STEP);
            let (position, frames) = self.block;
            self.receive(position, frames);
        }
    }

    /// Offline: waits for the hub to open the stream for `key`, handing back
    /// streams of clips that have ended meanwhile to make room.
    fn wait_for_stream(
        &mut self,
        key: (ClipId, u64),
        rel: u64,
        at: u64,
        current: &[ScheduledClip],
    ) -> Option<usize> {
        let started = Instant::now();
        loop {
            if self.abandoned.contains(&key) || self.shared.has_failed(key) {
                return None;
            }
            if let Some(slot) = self.pick(key, rel) {
                return Some(slot);
            }
            if started.elapsed() > WAIT_LIMIT {
                self.abandoned.push(key);
                return None;
            }
            std::thread::sleep(WAIT_STEP);
            self.receive_streams();
            self.sweep(at, self.looping, Some(current));
        }
    }

    /// Hands back streams that are no use any more: the clip has ended or
    /// gone, a stream for a clip that hasn't started yet was left part-way
    /// through (by a loop wrap or a seek back, so it would have to seek), or
    /// it was opened for the loop's start and looping is off.
    fn sweep(
        &mut self,
        position: u64,
        looping: Option<(u64, u64)>,
        current: Option<&[ScheduledClip]>,
    ) {
        for i in 0..self.streams.len() {
            let Some(stream) = &self.streams[i] else {
                continue;
            };
            let at = stream.stream.position();
            let wanted = |schedule: Option<&[ScheduledClip]>| {
                schedule.is_some_and(|s| {
                    s.iter().any(|c| {
                        c.stream_key() == (stream.id, stream.key)
                            && if stream.head {
                                looping.is_some_and(|(start, _)| c.end() > start)
                            } else {
                                c.end() > position && !(position < c.start && at != 0)
                            }
                    })
                })
            };
            if !(wanted(current) || wanted(self.pending.as_deref().map(Vec::as_slice)))
                && let Some(stream) = self.streams[i].take()
            {
                self.retire(Retired::Stream(stream));
            }
        }
        for i in 0..self.lanes.len() {
            let Some(lane) = &self.lanes[i] else { continue };
            let wanted = |schedule: Option<&[ScheduledClip]>| {
                schedule.is_some_and(|s| {
                    s.iter().any(|c| {
                        c.stretched()
                            && c.lane_key() == (lane.id, lane.key)
                            && if lane.head {
                                looping.is_some_and(|(start, _)| c.end() > start)
                            } else {
                                c.end() > position && !(position < c.start && lane.expected() != 0)
                            }
                    })
                })
            };
            if !(wanted(current) || wanted(self.pending.as_deref().map(Vec::as_slice)))
                && let Some(lane) = self.lanes[i].take()
            {
                self.retire_lane(lane);
            }
        }
        let bound = self.streams.iter().flatten().count() + self.lanes.iter().flatten().count();
        self.shared.bound.store(bound, Ordering::Relaxed);
    }

    /// Finds the stream to play `key` from at clip frame `rel`: one that is
    /// already there, else the nearest one behind it, else any. Once one is
    /// there, other streams for the clip that are ahead of the playhead are
    /// left over from before a loop wrap and are handed back.
    fn pick(&mut self, key: (ClipId, u64), rel: u64) -> Option<usize> {
        let mut exact = None;
        let mut behind: Option<(usize, u64)> = None;
        let mut any = None;
        for (i, slot) in self.streams.iter().enumerate() {
            let Some(p) = slot else { continue };
            if (p.id, p.key) != key {
                continue;
            }
            let at = p.stream.position();
            any.get_or_insert(i);
            if at == rel {
                exact.get_or_insert(i);
            } else if at < rel && behind.is_none_or(|(_, best)| at > best) {
                behind = Some((i, at));
            }
        }
        if let Some(keep) = exact {
            for i in 0..self.streams.len() {
                let stale = i != keep
                    && self.streams[i].as_ref().is_some_and(|p| {
                        (p.id, p.key) == key && !p.head && p.stream.position() > rel
                    });
                if stale && let Some(old) = self.streams[i].take() {
                    self.retire(Retired::Stream(old));
                }
            }
        }
        exact.or(behind.map(|(i, _)| i)).or(any)
    }

    /// Moves `level` towards `target` over `frames` frames.
    fn advance(&mut self, target: f32, frames: usize) {
        let delta = self.step * frames as f32;
        self.level = if target > self.level {
            (self.level + delta).min(target)
        } else {
            (self.level - delta).max(target)
        };
    }

    /// Has the stream for the clip at `position` ready to start from there.
    fn park(&mut self, position: u64) {
        let Some(clip) = self.schedule.as_ref().and_then(|s| active_at(s, position)) else {
            return;
        };
        if clip.stretched() && !self.preview {
            let rel = position - clip.start;
            if let Some(lane) = self
                .lanes
                .iter_mut()
                .flatten()
                .find(|l| (l.id, l.key) == clip.lane_key() && !l.head)
                && lane.expected() != rel
            {
                lane.seek(rel);
            }
            return;
        }
        let key = clip.stream_key();
        let at = clip.stream_start(position - clip.start);
        if let Some(prepared) = self
            .streams
            .iter_mut()
            .flatten()
            .find(|p| (p.id, p.key) == key)
            && prepared.stream.position() != at
        {
            prepared.stream.seek(at);
        }
    }

    /// Plays frames `from..to` of the block, which starts at `position`.
    fn render(&mut self, position: u64, from: usize, to: usize, target: f32) {
        let Some(schedule) = self.schedule.take() else {
            self.advance(target, to - from);
            return;
        };
        for (a, b, clip) in Segments::new(&schedule, position + from as u64, position + to as u64) {
            let (fa, fb) = ((a - position) as usize, (b - position) as usize);
            match clip {
                Some(clip) => self.play(clip, &schedule, a, fa, fb, target),
                None => self.advance(target, fb - fa),
            }
        }
        self.schedule = Some(schedule);
    }

    /// Plays `clip` over block frames `fa..fb`, which start at timeline
    /// sample `a`.
    fn play(
        &mut self,
        clip: &ScheduledClip,
        schedule: &[ScheduledClip],
        a: u64,
        fa: usize,
        fb: usize,
        target: f32,
    ) {
        if let Some(map) = &clip.warp {
            if clip.stretched() && !self.preview {
                self.play_stretched(clip, schedule, a, fa, fb, target);
                return;
            }
            self.play_warped(clip, map, schedule, a, fa, fb, target);
            return;
        }
        let len = fb - fa;
        let key = clip.stream_key();
        let rel = a - clip.start;
        let found = if self.shared.blocking {
            self.wait_for_stream(key, rel, a, schedule)
        } else {
            self.pick(key, rel)
        };
        let Some(slot) = found else {
            // Not opened yet: silence, and the hub is on it.
            self.shared.underruns.fetch_add(1, Ordering::Relaxed);
            // Hold the declick at silence, so it ramps in on the first real frame.
            self.level = 0.0;
            return;
        };
        let blocking = self.shared.blocking;
        let prepared = self.streams[slot].as_mut().expect("found above");
        let stream = &mut prepared.stream;
        let channels = stream.channels();
        // Line the stream up with the playhead. If it's a little behind (it
        // missed some blocks while it caught up after a seek), drop audio to
        // join in rather than seeking again, which would restart it. Anything
        // else needs a seek.
        let at = stream.position();
        if at < rel && rel - at <= MAX_SKIP {
            while stream.position() < rel {
                let want = (rel - stream.position()).min((self.scratch.len() / channels) as u64);
                if read_frames(
                    stream,
                    &mut self.scratch[..want as usize * channels],
                    blocking,
                ) == 0
                {
                    break;
                }
            }
        } else if at != rel {
            stream.seek(rel);
        }
        if stream.position() != rel {
            // Still catching up: nothing to play yet.
            self.shared.underruns.fetch_add(1, Ordering::Relaxed);
            // Hold the declick at silence, so it ramps in on the first real frame.
            self.level = 0.0;
            return;
        }
        let promoted = std::mem::take(&mut prepared.head).then_some(prepared.serial);
        let got = read_frames(stream, &mut self.scratch[..len * channels], blocking);
        if let Some(serial) = promoted {
            // Playing from it now, so it is an ordinary stream.
            let _ = self.to_hub.push(Retired::Promoted(serial));
        }
        if got < len {
            self.shared.underruns.fetch_add(1, Ordering::Relaxed);
            if blocking && got < (clip.length - rel).min(len as u64) as usize {
                // Waited and the disk still didn't answer: stop waiting on
                // this clip.
                self.abandoned.push(key);
            }
        }
        let fade_in = clip.fade_in as f32;
        let fade_out = clip.fade_out as f32;
        for i in 0..got {
            let at = rel + i as u64;
            let mut gain = clip.gain;
            if clip.fade_in > 0 && at < clip.fade_in {
                gain *= (at as f32 + 0.5) / fade_in;
            }
            let left_in_clip = clip.length - at;
            if clip.fade_out > 0 && left_in_clip <= clip.fade_out {
                gain *= (left_in_clip as f32 - 0.5) / fade_out;
            }
            self.advance(target, 1);
            gain *= self.level;
            let frame = &self.scratch[i * channels..(i + 1) * channels];
            let (l, r) = match channels {
                1 => (frame[0], frame[0]),
                _ => (frame[0], frame[1]),
            };
            self.left[fa + i] = l * gain;
            self.right[fa + i] = r * gain;
        }
        self.advance(target, len - got);
    }
}

impl TrackInputNode {
    /// Plays a warped `clip` over block frames `fa..fb`, which start at
    /// timeline sample `a`. The map says where in the source each output
    /// frame reads, and the audio there is interpolated between the source's
    /// own frames (Repitch: the pitch follows the speed).
    #[allow(clippy::too_many_arguments)]
    fn play_warped(
        &mut self,
        clip: &ScheduledClip,
        map: &WarpMap,
        schedule: &[ScheduledClip],
        a: u64,
        fa: usize,
        fb: usize,
        target: f32,
    ) {
        let key = clip.stream_key();
        let total = map.source_len() as u64;
        let mut at = a;
        let mut index = fa;
        while index < fb {
            let rel = at - clip.start;
            let mut n = (fb - index).min(WARP_CHUNK);
            let first = map.source_at(rel as f64);
            let mut last = map.source_at((rel + n as u64) as f64);
            if last - first > MAX_SPAN {
                // Playing very fast: take fewer output frames at a time so the
                // source they reach over fits in the window.
                n = ((n as f64 * MAX_SPAN / (last - first)) as usize).max(1);
                last = map.source_at((rel + n as u64) as f64);
            }
            // The cubic reads one frame before and two after.
            let lo = (first.floor().max(1.0) as u64 - 1).min(total);
            let hi = ((last.floor().max(0.0) as u64) + 3)
                .clamp(lo, total.min(lo + WINDOW_FRAMES as u64));
            if !self.ready_window(schedule, key, lo, hi, at) {
                // Silent until the stream delivers, and ramp in then, not
                // behind the gap.
                self.level = 0.0;
                at += n as u64;
                index += n;
                continue;
            }
            let channels = self
                .streams
                .iter()
                .flatten()
                .find(|p| Some(p.serial) == self.window_serial)
                .map_or(2, |p| p.stream.channels());
            for k in 0..n {
                let s = map.source_at((rel + k as u64) as f64);
                let whole = s.floor();
                let t = (s - whole) as f32;
                let j = whole as i64;
                let l = self.tap(j, 0, t, channels, total);
                let r = if channels > 1 {
                    self.tap(j, 1, t, channels, total)
                } else {
                    l
                };
                self.put(clip, rel + k as u64, index + k, l, r, target);
            }
            at += n as u64;
            index += n;
        }
    }

    /// Gets source frames `lo..hi` of `clip` into the window, from the stream
    /// playing it. False if they can't be had yet (no stream, or it is still
    /// catching up after a seek), in which case the caller plays silence.
    fn ready_window(
        &mut self,
        schedule: &[ScheduledClip],
        key: (ClipId, u64),
        lo: u64,
        hi: u64,
        at: u64,
    ) -> bool {
        let blocking = self.shared.blocking;
        // The stream the window came from, if it carries on from here.
        let held = self.streams.iter().position(|slot| {
            slot.as_ref().is_some_and(|p| {
                Some(p.serial) == self.window_serial
                    && (p.id, p.key) == key
                    && p.stream.position() == self.window_end
            })
        });
        let carry_on = held.is_some() && self.window_start <= lo && lo <= self.window_end;
        let found = if carry_on {
            held
        } else if blocking {
            self.wait_for_stream(key, lo, at, schedule)
        } else {
            self.pick(key, lo)
        };
        let Some(slot) = found else {
            // Not opened yet: silence, and the hub is on it.
            self.shared.underruns.fetch_add(1, Ordering::Relaxed);
            return false;
        };
        let prepared = self.streams[slot].as_mut().expect("found above");
        let stream = &mut prepared.stream;
        let channels = stream.channels();
        if carry_on {
            // Forget the frames behind `lo`.
            let drop = (lo - self.window_start) as usize;
            if drop > 0 {
                let keep = (self.window_end - lo) as usize;
                self.window
                    .copy_within(drop * channels..(drop + keep) * channels, 0);
                self.window_start = lo;
            }
        } else {
            // Line the stream up with `lo`, as a clip that isn't warped does
            // with the playhead: join in by dropping audio if it is a little
            // behind, and seek otherwise.
            let here = stream.position();
            if here < lo && lo - here <= MAX_SKIP {
                while stream.position() < lo {
                    let want = (lo - stream.position()).min((self.scratch.len() / channels) as u64);
                    if read_frames(
                        stream,
                        &mut self.scratch[..want as usize * channels],
                        blocking,
                    ) == 0
                    {
                        break;
                    }
                }
            } else if here != lo {
                stream.seek(lo);
            }
            if stream.position() != lo {
                self.shared.underruns.fetch_add(1, Ordering::Relaxed);
                return false;
            }
            self.window_start = lo;
            self.window_end = lo;
            self.window_serial = Some(prepared.serial);
        }
        let promoted = std::mem::take(&mut prepared.head).then_some(prepared.serial);
        if hi > self.window_end {
            let want = (hi - self.window_end) as usize;
            let from = (self.window_end - self.window_start) as usize * channels;
            let got = read_frames(
                stream,
                &mut self.window[from..from + want * channels],
                blocking,
            );
            self.window_end += got as u64;
            if got < want {
                self.shared.underruns.fetch_add(1, Ordering::Relaxed);
                if blocking && stream.position() < stream.total_frames() {
                    // Waited and the disk still didn't answer: stop waiting
                    // on this clip.
                    self.abandoned.push(key);
                }
            }
        }
        if let Some(serial) = promoted {
            // Playing from it now, so it is an ordinary stream.
            let _ = self.to_hub.push(Retired::Promoted(serial));
        }
        true
    }

    /// Source frame `j`, channel `c`, between it and the next by `t`, by
    /// cubic interpolation. The first and last frames of the clip hold past
    /// its ends, so the cut doesn't ring; frames the window lacks are silent.
    fn tap(&self, j: i64, c: usize, t: f32, channels: usize, total: u64) -> f32 {
        let at = |j: i64| -> f32 {
            let j = (j.max(0) as u64).min(total.saturating_sub(1));
            if j < self.window_start || j >= self.window_end {
                return 0.0;
            }
            self.window[(j - self.window_start) as usize * channels + c]
        };
        cubic(at(j - 1), at(j), at(j + 1), at(j + 2), t)
    }

    /// Puts one frame of `clip` in the output at block index `index`, with the
    /// clip's gain and fades (`at` is the frame's place in the clip) and the
    /// declick level.
    fn put(&mut self, clip: &ScheduledClip, at: u64, index: usize, l: f32, r: f32, target: f32) {
        let mut gain = clip.gain;
        if clip.fade_in > 0 && at < clip.fade_in {
            gain *= (at as f32 + 0.5) / clip.fade_in as f32;
        }
        let left_in_clip = clip.length.saturating_sub(at);
        if clip.fade_out > 0 && left_in_clip <= clip.fade_out {
            gain *= (left_in_clip as f32 - 0.5) / clip.fade_out as f32;
        }
        self.advance(target, 1);
        gain *= self.level;
        self.left[index] = l * gain;
        self.right[index] = r * gain;
    }
}

impl TrackInputNode {
    /// Plays a stretched `clip` (Grain or Beats) over block frames `fa..fb`,
    /// which start at timeline sample `a`, by copying out of its lane: the
    /// worker has rendered it already.
    fn play_stretched(
        &mut self,
        clip: &ScheduledClip,
        schedule: &[ScheduledClip],
        a: u64,
        fa: usize,
        fb: usize,
        target: f32,
    ) {
        let len = fb - fa;
        let key = clip.lane_key();
        let rel = a - clip.start;
        let blocking = self.shared.blocking;
        let found = if blocking {
            self.wait_for_lane(clip, rel, a, schedule)
        } else {
            self.pick_lane(key, rel)
        };
        let Some(slot) = found else {
            // Not opened yet: silence, and the worker is on it.
            self.shared.underruns.fetch_add(1, Ordering::Relaxed);
            // Hold the declick at silence, so it ramps in on the first real frame.
            self.level = 0.0;
            return;
        };
        let lane = self.lanes[slot].as_mut().expect("found above");
        lane.promote();
        if lane.expected() != rel {
            lane.seek(rel);
        }
        // Offline, wait for the worker to deliver everything the clip has in
        // this stretch; live, take what is there.
        let want = (clip.length.saturating_sub(rel)).min(len as u64) as usize;
        let started = Instant::now();
        let mut got = 0;
        loop {
            if lane.catch_up() {
                got += lane.read(&mut self.lane_frames[got * 2..want * 2]);
            }
            if got >= want || !blocking {
                break;
            }
            if started.elapsed() > WAIT_LIMIT {
                self.abandoned.push(key);
                break;
            }
            std::thread::sleep(WAIT_STEP);
        }
        if got < want {
            lane.miss(want - got);
            self.shared.underruns.fetch_add(1, Ordering::Relaxed);
        }
        for i in 0..got {
            let (l, r) = (self.lane_frames[i * 2], self.lane_frames[i * 2 + 1]);
            self.put(clip, rel + i as u64, fa + i, l, r, target);
        }
        if got == 0 && want > 0 {
            // Nothing to play: stay silent until the lane delivers, and ramp
            // in then, not behind the gap. (A block that is only short keeps
            // its level, or a lane that runs a little behind would never get
            // loud.)
            self.level = 0.0;
        } else {
            self.advance(target, len - got);
        }
    }

    /// Finds the lane to play `key` from at section frame `rel`: one that is
    /// already there, else any. Once one is, lanes for the section that are
    /// ahead of the playhead are left over from before a loop wrap and are
    /// handed back.
    fn pick_lane(&mut self, key: (ClipId, u64), rel: u64) -> Option<usize> {
        let mut exact = None;
        let (mut normal, mut head) = (None, None);
        for (i, slot) in self.lanes.iter().enumerate() {
            let Some(lane) = slot else { continue };
            if (lane.id, lane.key) != key {
                continue;
            }
            // A lane opened ahead for the loop's start is only for when the
            // playhead wraps, so it is the last resort.
            if lane.head {
                head.get_or_insert(i);
            } else {
                normal.get_or_insert(i);
            }
            if lane.expected() == rel {
                exact.get_or_insert(i);
            }
        }
        if let Some(keep) = exact {
            for i in 0..self.lanes.len() {
                let stale = i != keep
                    && self.lanes[i]
                        .as_ref()
                        .is_some_and(|l| (l.id, l.key) == key && !l.head && l.expected() > rel);
                if stale && let Some(old) = self.lanes[i].take() {
                    self.retire_lane(old);
                }
            }
        }
        exact.or(normal).or(head)
    }

    /// Offline: waits until the worker has opened the lane for `key`.
    fn wait_for_lane(
        &mut self,
        clip: &ScheduledClip,
        rel: u64,
        at: u64,
        current: &[ScheduledClip],
    ) -> Option<usize> {
        let key = clip.lane_key();
        let started = Instant::now();
        loop {
            if self.abandoned.contains(&key) || self.shared.has_failed(clip.stream_key()) {
                return None;
            }
            if let Some(slot) = self.pick_lane(key, rel) {
                return Some(slot);
            }
            if started.elapsed() > WAIT_LIMIT {
                self.abandoned.push(key);
                return None;
            }
            std::thread::sleep(WAIT_STEP);
            self.receive_streams();
            self.sweep(at, self.looping, Some(current));
        }
    }
}

impl TrackInputNode {
    /// Writes the MIDI events for the block of `n` frames at `position`:
    /// note-ons for notes that start in it, note-offs for those that end in
    /// it, and, when the transport has stopped or jumped, an off for every
    /// note still sounding, so nothing hangs.
    fn play_notes(
        &mut self,
        position: u64,
        n: usize,
        playing: bool,
        out: &mut noodle_engine::EventsOut<'_>,
    ) {
        let block_end = position + n as u64;
        let continuing = self.played_to == Some(position);
        self.played_to = playing.then_some(block_end);
        let off = |id: u32, time: u64| Event {
            time: time as u32,
            kind: EventKind::NoteOff {
                note: NoteId(id),
                velocity: 0.0,
            },
        };
        self.play_audition(out);
        // A note that can't be ended because the buffer is full stays
        // sounding and is tried again next block.
        if !playing || !continuing {
            self.sounding.retain(|s| out.insert(off(s.id, 0)).is_err());
        }
        let notes = self.notes.take();
        if self.recheck {
            self.recheck = false;
            let current = notes.as_deref().map_or(&[][..], Vec::as_slice);
            self.sounding.retain_mut(|s| {
                let lo = current.partition_point(|note| note.start < s.start);
                let found = current[lo..]
                    .iter()
                    .take_while(|note| note.start == s.start)
                    .find(|note| note.id == s.id);
                match found {
                    Some(note) => {
                        s.end = note.end;
                        true
                    }
                    None => out.insert(off(s.id, 0)).is_err(),
                }
            });
        }
        if playing {
            let current = notes.as_deref().map_or(&[][..], Vec::as_slice);
            let mut next = current.partition_point(|note| note.start < position);
            loop {
                let on = current.get(next).filter(|note| note.start < block_end);
                let ending = self
                    .sounding
                    .iter()
                    .enumerate()
                    .filter(|(_, s)| s.end < block_end)
                    .min_by_key(|(_, s)| s.end);
                // Ends come first at a tie, so a note can be struck again.
                match (on, ending) {
                    (Some(note), Some((_, s))) if note.start < s.end => {
                        self.start_note(note, position, out);
                        next += 1;
                    }
                    (_, Some((i, s))) => {
                        let time = s.end.max(position) - position;
                        if out.insert(off(s.id, time)).is_err() {
                            break;
                        }
                        self.sounding.swap_remove(i);
                    }
                    (Some(note), None) => {
                        self.start_note(note, position, out);
                        next += 1;
                    }
                    (None, None) => break,
                }
            }
        }
        self.notes = notes;
    }

    /// Starts and ends the notes the piano roll's keyboard holds. Their IDs
    /// are their own, and they sound whatever the transport is doing.
    fn play_audition(&mut self, out: &mut noodle_engine::EventsOut<'_>) {
        for word in 0..2 {
            let wanted = self.shared.audition[word].load(Ordering::Relaxed);
            let mut changed = wanted ^ self.audition_held[word];
            while changed != 0 {
                let bit = changed.trailing_zeros();
                changed &= changed - 1;
                let key = word as u32 * 64 + bit;
                let note = NoteId(AUDITION_ID | key);
                let kind = if wanted >> bit & 1 == 1 {
                    EventKind::NoteOn {
                        note,
                        channel: 0,
                        key: key as u8,
                        velocity: 0.8,
                    }
                } else {
                    EventKind::NoteOff {
                        note,
                        velocity: 0.0,
                    }
                };
                // A full buffer leaves the key as it was, to try again.
                if out.insert(Event { time: 0, kind }).is_ok() {
                    self.audition_held[word] ^= 1 << bit;
                }
            }
        }
    }

    fn start_note(
        &mut self,
        note: &ScheduledNote,
        position: u64,
        out: &mut noodle_engine::EventsOut<'_>,
    ) {
        if self.sounding.len() == MAX_SOUNDING {
            return;
        }
        let on = Event {
            time: (note.start - position) as u32,
            kind: EventKind::NoteOn {
                note: NoteId(note.id),
                channel: 0,
                key: note.key,
                velocity: note.velocity,
            },
        };
        if out.insert(on).is_ok() {
            self.sounding.push(Sounding {
                id: note.id,
                start: note.start,
                end: note.end,
            });
        }
    }
}

/// Reads from `stream` into `out` (whole frames). Offline, it waits until the
/// disk has caught up, or the clip ends or the stream fails, so it returns
/// fewer frames than asked for only at the end of the clip.
fn read_frames(stream: &mut ClipStream, out: &mut [f32], blocking: bool) -> usize {
    let channels = stream.channels();
    let want = out.len() / channels;
    let mut got = stream.read(out);
    if blocking {
        let started = Instant::now();
        while got < want
            && !stream.failed()
            && stream.position() < stream.total_frames()
            && started.elapsed() < WAIT_LIMIT
        {
            std::thread::sleep(WAIT_STEP);
            got += stream.read(&mut out[got * channels..]);
        }
    }
    got
}

impl Node for TrackInputNode {
    fn constant_output(&self, output: usize, constants: &[Option<f32>]) -> Option<f32> {
        match output {
            GAIN_OUT => constants[GAIN_IN],
            MUTE_OUT => constants[MUTE_IN],
            _ => None,
        }
    }

    fn process(&mut self, ctx: &Context, io: Io<'_, '_>) {
        let n = ctx.frames;
        // The track's own controls, passed on for its group output.
        // (A rig that drives the node by hand may have only the audio output.)
        for (input, output) in [(GAIN_IN, GAIN_OUT), (MUTE_IN, MUTE_OUT)] {
            if let (Some(input), Some(output)) = (io.inputs.get(input), io.outputs.get_mut(output))
            {
                output.lane_mut(0, 0).copy_from_slice(input.lane(0, 0));
            }
        }
        let transport = ctx.transport;
        self.shared
            .position
            .store(transport.position, Ordering::Relaxed);
        let (start, end) = transport.loop_range.unwrap_or((0, 0));
        self.shared.loop_start.store(start, Ordering::Relaxed);
        self.shared.loop_end.store(end, Ordering::Relaxed);
        self.block = (transport.position, n);
        self.looping = transport.loop_range;
        self.receive(transport.position, n);
        self.watch_preview();
        if self.shared.blocking {
            self.wait_for_schedule();
        }
        let current = self.schedule.take();
        self.sweep(
            transport.position,
            transport.loop_range,
            current.as_deref().map(Vec::as_slice),
        );
        self.schedule = current;
        self.left[..n].fill(0.0);
        self.right[..n].fill(0.0);
        // Live notes are played whatever the transport does. A track that
        // isn't monitored lets go of any it holds.
        if let Some(live) = &mut self.live {
            let monitoring = self.shared.monitoring.load(Ordering::Relaxed);
            live.process(ctx, monitoring.then_some(0), &mut io.event_outputs[MIDI]);
        }
        self.play_notes(
            transport.position,
            n,
            transport.playing,
            &mut io.event_outputs[MIDI],
        );

        // While stopped the playhead holds still, but a fade-out still plays
        // on from it, so the streams end up a little past it. Once silent,
        // line them up with the playhead ready for the next start.
        if !transport.playing && self.level <= 0.0 && !self.dipping() {
            self.park(transport.position);
            let out = io.outputs;
            out[AUDIO].lane_mut(0, 0).copy_from_slice(&self.left[..n]);
            out[AUDIO].lane_mut(0, 1).copy_from_slice(&self.right[..n]);
            return;
        }
        let mut from = 0;
        while from < n {
            if self.dipping() && self.level <= 0.0 {
                self.adopt_pending();
            }
            let target = if transport.playing && !self.dipping() {
                1.0
            } else {
                0.0
            };
            let mut to = n;
            if self.dipping() {
                // Run the old schedule down to silence, then swap.
                let frames = (self.level / self.step).ceil() as usize;
                to = (from + frames.max(1)).min(n);
            }
            self.render(transport.position, from, to, target);
            from = to;
        }

        let out = &mut io.outputs[AUDIO];
        out.lane_mut(0, 0).copy_from_slice(&self.left[..n]);
        out.lane_mut(0, 1).copy_from_slice(&self.right[..n]);
    }

    fn reset(&mut self) {
        // Whatever sounds is ended by the next block, which finds that it
        // doesn't continue from the last.
        self.played_to = None;
        // The engine has faded out and is about to jump: start from silence at
        // the new place, with whatever schedule is waiting.
        self.level = 0.0;
        self.adopt_pending();
    }
}
