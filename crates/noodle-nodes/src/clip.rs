//! The track input node: plays a track's audio clips along the transport.
//!
//! It has two outputs, `audio` (stereo) and `midi` (events, which stay empty
//! until MIDI clips arrive with M3). The clips come from the project, not from
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

mod hub;
mod schedule;

use std::sync::Arc;
use std::sync::atomic::Ordering;

use noodle_core::ClipId;
use noodle_engine::{
    Config, Context, Instance, Io, Layout, Node, NodeError, NodeInfo, NodeType, Setup, Shape,
};
use rtrb::{Consumer, Producer, PushError};

use hub::{Links, MAX_CHANNELS, MAX_STREAMS, Prepared, Retired, Shared, ToNode};
pub use schedule::{
    ClipFeeds, ClipProblem, ClipSource, ClipStatus, Schedule, ScheduledClip, active_at,
};
use schedule::{Segments, same_sound};

pub const TRACK_INPUT_ID: &str = "noodle.track.input";

const AUDIO: usize = 0;
const FADE_SECONDS: f32 = 0.005;
/// How far behind a stream may be and still catch up by reading and dropping
/// audio, which is quicker than seeking.
const MAX_SKIP: u64 = 8192;

static INFO: NodeInfo = NodeInfo {
    id: TRACK_INPUT_ID,
    version: 1,
    name: "Track Input",
    category: "Track",
};

/// Registered by [`register_library`](crate::register_library).
pub struct TrackInput {
    feeds: ClipFeeds,
}

impl TrackInput {
    pub fn new(feeds: &ClipFeeds) -> Self {
        Self {
            feeds: feeds.clone(),
        }
    }
}

impl NodeType for TrackInput {
    fn info(&self) -> &NodeInfo {
        &INFO
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(Layout::realtime()
            .output("audio", "Audio")
            .event_output("midi", "MIDI"))
    }

    fn output_shapes(
        &self,
        _config: &Config,
        _layout: &Layout,
        _inputs: &[Shape],
    ) -> Result<Vec<Shape>, NodeError> {
        Ok(vec![Shape::STEREO])
    }

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        let shared = self.feeds.shared(setup.node);
        let links = hub::spawn(shared.clone(), setup.sample_rate.round() as u32);
        Ok(Instance::realtime(TrackInputNode::new(
            shared,
            links,
            setup.sample_rate,
            setup.max_frames,
        )))
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
    /// Decoded frames for the stretch being played.
    scratch: Vec<f32>,
    left: Vec<f32>,
    right: Vec<f32>,
    /// The declick gain, from 0 to 1.
    level: f32,
    step: f32,
}

impl TrackInputNode {
    fn new(shared: Arc<Shared>, links: Links, sample_rate: f32, max_frames: usize) -> Self {
        let fade_len = (sample_rate * FADE_SECONDS).round().max(1.0);
        Self {
            shared,
            from_hub: links.from_hub,
            to_hub: links.to_hub,
            schedule: None,
            pending: None,
            streams: (0..MAX_STREAMS).map(|_| None).collect(),
            scratch: vec![0.0; max_frames * MAX_CHANNELS],
            left: vec![0.0; max_frames],
            right: vec![0.0; max_frames],
            level: 0.0,
            step: 1.0 / fade_len,
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
    }

    /// Takes what the hub has sent. A new schedule that changes what is heard
    /// over this block waits for the dip; any other takes over at once.
    fn receive(&mut self, ctx: &Context) {
        while let Ok(message) = self.from_hub.pop() {
            match message {
                ToNode::Stream(prepared) => {
                    match self.streams.iter_mut().find(|slot| slot.is_none()) {
                        Some(slot) => *slot = Some(prepared),
                        None => self.retire(Retired::Stream(prepared)),
                    }
                }
                ToNode::Schedule(next) => {
                    let from = ctx.transport.position;
                    let to = from + ctx.frames as u64;
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
                        // Nothing audible changes, so it can take over now,
                        // including over a dip that is waiting.
                        if let Some(older) = self.pending.take() {
                            self.retire(Retired::Schedule(older));
                        }
                        if let Some(old) = self.schedule.replace(next) {
                            self.retire(Retired::Schedule(old));
                        }
                    }
                }
            }
        }
    }

    /// Hands back streams that are no use any more: the clip has ended or
    /// gone, a stream for a clip that hasn't started yet was left part-way
    /// through (by a loop wrap or a seek back, so it would have to seek), or
    /// it was opened for the loop's start and looping is off.
    fn sweep(&mut self, position: u64, looping: Option<(u64, u64)>) {
        for i in 0..self.streams.len() {
            let Some(stream) = &self.streams[i] else {
                continue;
            };
            let at = stream.stream.position();
            let wanted = |schedule: &Option<Box<Schedule>>| {
                schedule.as_ref().is_some_and(|s| {
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
            if !(wanted(&self.schedule) || wanted(&self.pending))
                && let Some(stream) = self.streams[i].take()
            {
                self.retire(Retired::Stream(stream));
            }
        }
        let bound = self.streams.iter().flatten().count();
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
        let key = clip.stream_key();
        let rel = position - clip.start;
        if let Some(prepared) = self
            .streams
            .iter_mut()
            .flatten()
            .find(|p| (p.id, p.key) == key)
            && prepared.stream.position() != rel
        {
            prepared.stream.seek(rel);
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
                Some(clip) => self.play(clip, a, fa, fb, target),
                None => self.advance(target, fb - fa),
            }
        }
        self.schedule = Some(schedule);
    }

    /// Plays `clip` over block frames `fa..fb`, which start at timeline
    /// sample `a`.
    fn play(&mut self, clip: &ScheduledClip, a: u64, fa: usize, fb: usize, target: f32) {
        let len = fb - fa;
        let key = clip.stream_key();
        let rel = a - clip.start;
        let Some(slot) = self.pick(key, rel) else {
            // Not opened yet: silence, and the hub is on it.
            self.shared.underruns.fetch_add(1, Ordering::Relaxed);
            self.advance(target, len);
            return;
        };
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
                if stream.read(&mut self.scratch[..want as usize * channels]) == 0 {
                    break;
                }
            }
        } else if at != rel {
            stream.seek(rel);
        }
        if stream.position() != rel {
            // Still catching up: nothing to play yet.
            self.shared.underruns.fetch_add(1, Ordering::Relaxed);
            self.advance(target, len);
            return;
        }
        let promoted = std::mem::take(&mut prepared.head).then_some(prepared.serial);
        let got = stream.read(&mut self.scratch[..len * channels]);
        if let Some(serial) = promoted {
            // Playing from it now, so it is an ordinary stream.
            let _ = self.to_hub.push(Retired::Promoted(serial));
        }
        if got < len {
            self.shared.underruns.fetch_add(1, Ordering::Relaxed);
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

impl Node for TrackInputNode {
    fn process(&mut self, ctx: &Context, io: Io<'_, '_>) {
        let n = ctx.frames;
        let transport = ctx.transport;
        self.shared
            .position
            .store(transport.position, Ordering::Relaxed);
        let (start, end) = transport.loop_range.unwrap_or((0, 0));
        self.shared.loop_start.store(start, Ordering::Relaxed);
        self.shared.loop_end.store(end, Ordering::Relaxed);
        self.receive(ctx);
        self.sweep(transport.position, transport.loop_range);
        self.left[..n].fill(0.0);
        self.right[..n].fill(0.0);

        // While stopped the playhead holds still, but a fade-out still plays
        // on from it, so the streams end up a little past it. Once silent,
        // line them up with the playhead ready for the next start.
        if !transport.playing && self.level <= 0.0 && self.pending.is_none() {
            self.park(transport.position);
            let out = io.outputs;
            out[AUDIO].lane_mut(0, 0).copy_from_slice(&self.left[..n]);
            out[AUDIO].lane_mut(0, 1).copy_from_slice(&self.right[..n]);
            return;
        }
        let mut from = 0;
        while from < n {
            if self.pending.is_some() && self.level <= 0.0 {
                self.adopt_pending();
            }
            let target = if transport.playing && self.pending.is_none() {
                1.0
            } else {
                0.0
            };
            let mut to = n;
            if self.pending.is_some() {
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
        // The engine has faded out and is about to jump: start from silence at
        // the new place, with whatever schedule is waiting.
        self.level = 0.0;
        self.adopt_pending();
    }
}
