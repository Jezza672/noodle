//! What a track input plays, in the engine's own units: the project's clips
//! for one node, turned into sample positions, sorted by start.

use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use noodle_core::{
    ClipContent, ClipId, MIN_LOOP_SECONDS, NodeId, Project, TICKS_PER_QUARTER, Tick, WarpAxis,
    WarpMode,
};
use noodle_engine::TempoTable;
use noodle_io::{AudioFiles, Decoder, FileInfo, FileStamp, clip_frames};

use super::hub::Shared;
pub use super::warp::WarpMap;

/// Which part of which file a clip plays.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ClipSource {
    pub path: PathBuf,
    /// Where the clip starts in the file, in the file's frames.
    pub offset: u64,
    /// How much of the file it plays, in the file's frames.
    pub length: u64,
    /// The file's modification time when the clip was scheduled. A file
    /// replaced on disk gives the clip a new source, so streams open on the
    /// old one are dropped and the new one is read.
    pub stamp: Option<SystemTime>,
}

/// One clip, placed on the timeline in samples.
#[derive(Clone, Debug, PartialEq)]
pub struct ScheduledClip {
    pub id: ClipId,
    /// The sample the clip starts on.
    pub start: u64,
    /// How long it plays, in engine frames.
    pub length: u64,
    pub gain: f32,
    /// Linear fades at either end, in engine frames.
    pub fade_in: u64,
    pub fade_out: u64,
    pub source: ClipSource,
    /// Where each frame of the clip reads from the source, if the clip is
    /// warped. Without it, frame `n` of the clip reads source frame `n`.
    pub warp: Option<Arc<WarpMap>>,
    /// How the warp is rendered.
    pub mode: WarpMode,
    /// The gain envelope: (frame from the clip's start, dB), sorted. Empty
    /// for a flat clip.
    pub envelope: Option<Arc<[(i64, f32)]>>,
    /// The clip this one is crossfading with: it keeps playing, under this
    /// one, until it ends. Both fades of the pair are equal power.
    pub under: Option<ClipId>,
    pub xfade_in: bool,
    pub xfade_out: bool,
    /// How many frames the clip can be extended earlier and later with the
    /// audio the file has there, in engine frames.
    pub head_room: u64,
    pub tail_room: u64,
    /// File frames per engine frame, to turn an extension into the file's
    /// frames.
    src_ratio: f64,
    key: u64,
    lane: u64,
}

impl ScheduledClip {
    pub fn new(
        id: ClipId,
        start: u64,
        length: u64,
        gain: f32,
        fade_in: u64,
        fade_out: u64,
        source: ClipSource,
    ) -> Self {
        let mut hasher = DefaultHasher::new();
        source.hash(&mut hasher);
        Self {
            id,
            start,
            length,
            gain,
            fade_in,
            fade_out,
            source,
            warp: None,
            mode: WarpMode::Repitch,
            envelope: None,
            under: None,
            xfade_in: false,
            xfade_out: false,
            head_room: 0,
            tail_room: 0,
            src_ratio: 1.0,
            key: hasher.finish(),
            lane: 0,
        }
    }

    /// This clip, with a gain envelope of (frame from the clip's start, dB)
    /// points.
    pub fn with_envelope(mut self, mut points: Vec<(i64, f32)>) -> Self {
        points.sort_by_key(|p| p.0);
        self.envelope = (!points.is_empty()).then(|| points.into());
        self
    }

    /// This clip, told how far the file lets it be extended at either end
    /// (in engine frames), and how many of the file's frames make an engine
    /// frame.
    pub fn with_room(mut self, head: u64, tail: u64, src_ratio: f64) -> Self {
        self.head_room = head;
        self.tail_room = tail;
        self.src_ratio = src_ratio;
        self
    }

    /// The linear level at frame `at` of the clip: its gain, its envelope and
    /// its fades.
    pub fn level(&self, at: u64) -> f32 {
        let mut gain = self.gain;
        if let Some(points) = &self.envelope {
            gain *= db_to_gain(envelope_db(points, at as i64));
        }
        if self.fade_in > 0 && at < self.fade_in {
            let x = (at as f32 + 0.5) / self.fade_in as f32;
            gain *= if self.xfade_in { equal_power(x) } else { x };
        }
        let left = self.length.saturating_sub(at);
        if self.fade_out > 0 && left <= self.fade_out {
            let x = (left as f32 - 0.5) / self.fade_out as f32;
            gain *= if self.xfade_out { equal_power(x) } else { x };
        }
        gain
    }

    /// This clip lasting `head` frames longer before its start and `tail`
    /// after its end, playing the file's audio there (within the room it
    /// has). Its envelope keeps its place on the timeline.
    pub fn extended(&self, head: u64, tail: u64) -> Self {
        let (head, tail) = (head.min(self.head_room), tail.min(self.tail_room));
        let mut clip = self.clone();
        clip.start -= head.min(clip.start);
        let head = self.start - clip.start;
        clip.length += head + tail;
        clip.head_room -= head;
        clip.tail_room -= tail;
        let (mut head_src, mut tail_src) = (head as f64, tail as f64);
        if let Some(map) = &self.warp {
            let knots = map.knots();
            let first = knots[1].1 / knots[1].0;
            let n = knots.len();
            let last = (knots[n - 1].1 - knots[n - 2].1) / (knots[n - 1].0 - knots[n - 2].0);
            head_src *= first;
            tail_src *= last;
            clip.warp = Some(Arc::new(map.extended(
                head as f64,
                head_src,
                tail as f64,
                tail_src,
            )));
        }
        let to_file = |frames: f64| (frames * self.src_ratio).round() as u64;
        let (head_file, tail_file) = (to_file(head_src), to_file(tail_src));
        clip.source.offset = self.source.offset.saturating_sub(head_file);
        clip.source.length =
            self.source.length + (self.source.offset - clip.source.offset) + tail_file;
        if let Some(points) = &self.envelope {
            clip.envelope = Some(
                points
                    .iter()
                    .map(|&(f, db)| (f + head as i64, db))
                    .collect(),
            );
        }
        let mut hasher = DefaultHasher::new();
        clip.source.hash(&mut hasher);
        clip.key = hasher.finish();
        clip.lane = clip.lane_hash();
        clip
    }

    /// This clip, warped by `map`. Its `length` is how long it lasts once
    /// warped.
    pub fn with_warp(mut self, map: Arc<WarpMap>) -> Self {
        self.warp = Some(map);
        self.lane = self.lane_hash();
        self
    }

    /// This clip, with its warp rendered in `mode`.
    pub fn with_mode(mut self, mode: WarpMode) -> Self {
        self.mode = mode;
        self.lane = self.lane_hash();
        self
    }

    fn lane_hash(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        self.key.hash(&mut hasher);
        self.mode.hash(&mut hasher);
        if let Some(map) = &self.warp {
            for (out, source) in map.knots() {
                out.to_bits().hash(&mut hasher);
                source.to_bits().hash(&mut hasher);
            }
        }
        hasher.finish()
    }

    /// Whether the clip is played by stretching ahead of the playhead (Grain
    /// and Beats), and not by resampling in the audio thread.
    pub fn stretched(&self) -> bool {
        self.warp.is_some() && self.mode.keeps_pitch()
    }

    /// Identifies the stretched audio of this clip: like
    /// [`stream_key`](Self::stream_key), but a new warp or mode is a new
    /// lane, because the stretch has to be done again.
    pub(super) fn lane_key(&self) -> (ClipId, u64) {
        (self.id, self.lane)
    }

    /// The source frame a stream playing this clip from clip frame `rel` has
    /// to be at: one before the frame being played, for the interpolation to
    /// look back at.
    pub fn stream_start(&self, rel: u64) -> u64 {
        match &self.warp {
            None => rel,
            Some(map) => (map.source_at(rel as f64).floor().max(0.0) as u64).saturating_sub(1),
        }
    }

    /// The first sample after the clip.
    pub fn end(&self) -> u64 {
        self.start + self.length
    }

    /// Identifies the audio stream playing this clip. Moving a clip or
    /// changing its gain or fades keeps it, because a stream counts from the
    /// clip's own start.
    pub(super) fn stream_key(&self) -> (ClipId, u64) {
        (self.id, self.key)
    }
}

/// The envelope's level in dB at `frame`: straight lines in dB between the
/// points, held before the first and after the last.
fn envelope_db(points: &[(i64, f32)], frame: i64) -> f32 {
    let after = points.partition_point(|p| p.0 <= frame);
    match (
        after.checked_sub(1).map(|i| points[i]),
        points.get(after).copied(),
    ) {
        (None, None) => 0.0,
        (Some(p), None) | (None, Some(p)) => p.1,
        (Some(a), Some(b)) => a.1 + (b.1 - a.1) * (frame - a.0) as f32 / (b.0 - a.0).max(1) as f32,
    }
}

fn db_to_gain(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

/// The gain of one side of an equal-power crossfade, `x` of the way (0 to 1)
/// through its fade in; the fade out uses the same curve backwards.
fn equal_power(x: f32) -> f32 {
    (x.clamp(0.0, 1.0) * std::f32::consts::FRAC_PI_2).sin()
}

/// One stretch of a section that plays a single run of the file: a section
/// plays as one piece, or, when its clip loops and it is longer than its
/// window, as the window's lead-in followed by one piece per repeat of the
/// loop. Pieces sit back to back, so a repeat is gapless without the node
/// knowing anything about loops.
struct Piece {
    /// The part of the file the piece plays, in the file's frames.
    offset: u64,
    length: u64,
    /// Where it starts and how long it lasts, in engine frames from the start
    /// of the section.
    from: u64,
    len: u64,
    warp: Option<(Arc<WarpMap>, WarpMode)>,
}

/// The most pieces one section is laid out as; a longer section is cut off.
/// (A loop is at least a beat long, so this is hours of music.)
const MAX_PIECES: usize = 20_000;

/// Gives piece `k` of a section its own ID, so its stream and its stretched
/// lane are its own. Real clip IDs are small counts, far below the bits used.
fn piece_id(id: ClipId, k: usize) -> ClipId {
    ClipId(id.0 | (k as u64) << 40)
}

/// How a section of an audio clip is laid out in engine frames: the pieces it
/// plays, each with the map from its frames to the source's if it is warped.
/// A warp that leaves the clip at its natural speed is left out, so those
/// play as they always did.
fn lay_out(
    project: &Project,
    clip: &noodle_core::Clip,
    audio: &noodle_core::AudioClip,
    info: &FileInfo,
    table: &TempoTable,
    rate: u32,
) -> (Vec<Piece>, Option<String>) {
    let source_frames = clip_frames(info, audio.offset, audio.length, rate);
    let Some(warp) = audio
        .warp
        .and_then(|id| project.warp(id))
        .filter(|warp| !warp.is_identity())
    else {
        let piece = Piece {
            offset: audio.offset,
            length: audio.length,
            from: 0,
            len: source_frames,
            warp: None,
        };
        return (vec![piece], None);
    };
    let origin = audio.offset as f64 / f64::from(info.sample_rate);
    let window = source_frames as f64 / f64::from(rate);
    let curve = warp.curve();
    let map = WarpMap::build(
        &curve,
        table,
        clip.start,
        f64::from(rate),
        origin,
        window,
        source_frames,
    );
    let len = (map.out_len().round() as u64).max(1);
    // How long the section lasts: the window, or as far as it was dragged
    // when it loops.
    let looped = warp.loop_seconds().filter(|&(_, end)| origin < end - 1e-6);
    let total = match (looped, audio.extent) {
        (Some(_), Some(extent)) => table
            .sample_at_tick(Tick(clip.start.0 + extent.0))
            .saturating_sub(table.sample_at_tick(clip.start))
            .max(1),
        _ => len,
    };
    let once = |map: WarpMap| {
        let map = if total < len {
            map.truncated(total as f64)
        } else {
            map
        };
        vec![Piece {
            offset: audio.offset,
            length: audio.length,
            from: 0,
            len: total.min(len),
            warp: Some((Arc::new(map), warp.mode)),
        }]
    };
    let Some((loop_start, loop_end)) = looped else {
        return (once(map), None);
    };
    let file_rate = f64::from(info.sample_rate);
    let engine_rate = f64::from(rate);
    // A round of the loop that is over too quickly to open a stream for each
    // is not played: the section plays once.
    let round = WarpMap::build(
        &curve,
        table,
        clip.start,
        engine_rate,
        loop_start,
        loop_end - loop_start,
        ((loop_end - loop_start) * engine_rate).round().max(1.0) as u64,
    );
    if round.out_len() / engine_rate < MIN_LOOP_SECONDS - 1e-9 {
        let problem =
            format!("the loop is shorter than {MIN_LOOP_SECONDS} s here, so it doesn't repeat");
        return (once(map), Some(problem));
    }
    let mut pieces: Vec<Piece> = Vec::new();
    let mut elapsed = 0.0f64;
    let mut tick = clip.start.0 as f64;
    let mut from = origin;
    while (elapsed.round() as u64) < total && pieces.len() < MAX_PIECES {
        let offset = (from * file_rate).round() as u64;
        let length = (((loop_end - from) * file_rate).round() as u64).max(1);
        let frames = clip_frames(info, offset, length, rate);
        if frames == 0 {
            break;
        }
        let begin = offset as f64 / file_rate;
        let mut piece_map = WarpMap::build(
            &curve,
            table,
            Tick(tick.round() as i64),
            engine_rate,
            begin,
            frames as f64 / engine_rate,
            frames,
        );
        let at = elapsed.round() as u64;
        let end = elapsed + piece_map.out_len();
        if curve.axis == WarpAxis::Beats {
            tick += (curve.at(begin + frames as f64 / engine_rate) - curve.at(begin))
                * TICKS_PER_QUARTER as f64;
        }
        let mut len = (end.round() as u64 - at).max(1);
        if at + len > total {
            len = total - at;
            piece_map = piece_map.truncated(len as f64);
        }
        let shared = match pieces.last().and_then(|p| p.warp.as_ref()) {
            Some((last, _)) if **last == piece_map => last.clone(),
            _ => Arc::new(piece_map),
        };
        pieces.push(Piece {
            offset,
            length,
            from: at,
            len,
            warp: Some((shared, warp.mode)),
        });
        elapsed = end;
        from = loop_start;
    }
    if pieces.len() < 2 {
        // The edge is inside the first time round to the loop's end: nothing
        // repeats, and the window plays (cut off at the edge).
        return (once(map), None);
    }
    (pieces, None)
}

/// Lays out the crossfades of one track. `wanted` lists, for each clip that
/// has one, its first piece's ID and how many frames the crossfade lasts
/// before the cut and after it. The clip butted against the start of that
/// one plays on under it past its end, and the clip itself plays from before
/// its start, each as far as the file lets it; the pair fade in equal power
/// across the whole stretch.
fn apply_crossfades(schedule: &mut Schedule, wanted: &[(ClipId, u64, u64)], rate: u32) {
    let tolerance = (f64::from(rate) * noodle_core::BUTT_SECONDS).round() as u64;
    let mut wanted = wanted.to_vec();
    wanted.sort_by_key(|&(id, ..)| schedule.iter().find(|c| c.id == id).map(|c| c.start));
    for (id, before, after) in wanted {
        let Some(b) = schedule.iter().position(|c| c.id == id) else {
            continue;
        };
        let cut = schedule[b].start;
        // The clip that ends where this one starts.
        let Some(a) = (0..schedule.len())
            .filter(|&i| i != b)
            .filter(|&i| schedule[i].start < cut)
            .filter(|&i| schedule[i].end().abs_diff(cut) <= tolerance)
            .min_by_key(|&i| schedule[i].end().abs_diff(cut))
        else {
            continue;
        };
        let (early, late) = (&schedule[a], &schedule[b]);
        let before = before.min(late.head_room).min(early.length / 2).min(cut);
        let after = after
            .min(early.tail_room + early.end().saturating_sub(cut))
            .min(late.length / 2)
            .max(early.end().saturating_sub(cut));
        if before + after == 0 {
            continue;
        }
        let span = before + after;
        let mut late = late.extended(before, 0);
        late.fade_in = span;
        late.xfade_in = true;
        late.under = Some(early.id);
        let mut early = early.extended(0, (cut + after).saturating_sub(early.end()));
        early.fade_out = span.min(early.length);
        early.xfade_out = true;
        schedule[a] = early;
        schedule[b] = late;
    }
}

/// A track's clips, sorted by start (then ID).
pub type Schedule = Vec<ScheduledClip>;

/// One note of a MIDI clip, placed on the timeline in samples.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScheduledNote {
    /// Tells the note apart from the others while it sounds. Stable across
    /// schedules while the note keeps its clip, start and key, so an edit
    /// elsewhere doesn't cut a note that is sounding.
    pub id: u32,
    /// The sample the note starts on.
    pub start: u64,
    /// The first sample after it, never before `start + 1` and never past
    /// the end of its clip.
    pub end: u64,
    pub key: u8,
    pub velocity: f32,
}

/// Note IDs of notes in clips use the low 29 bits. The top three are for notes
/// from elsewhere: the MIDI In node sets the top bit, the piano roll's
/// keyboard (see [`ClipFeeds::audition`]) the one below it, and live
/// monitoring (see [`TrackInput::with_midi`](super::TrackInput::with_midi))
/// the one below that.
pub const CLIP_NOTE_MASK: u32 = 0x1FFF_FFFF;

/// A track's MIDI notes, sorted by start (then ID). Notes of overlapping
/// clips all play.
pub type Notes = Vec<ScheduledNote>;

/// Turns the notes of a MIDI clip into scheduled notes.
fn schedule_notes(
    table: &TempoTable,
    id: ClipId,
    clip: &noodle_core::Clip,
    midi: &noodle_core::MidiClip,
    out: &mut Notes,
) {
    let at = |ticks: i64| table.sample_at_tick(noodle_core::Tick(clip.start.0 + ticks));
    let clip_end = at(midi.length.0);
    for note in &midi.played() {
        let start = at(note.start.0);
        if start >= clip_end {
            continue;
        }
        let end = at(note.end().0).min(clip_end).max(start + 1);
        // A note is known by its clip, place and key, so removing or moving
        // other notes leaves it alone. Two notes can only share an identity
        // by being the same note twice.
        let mut hasher = DefaultHasher::new();
        (id, note.start, note.key).hash(&mut hasher);
        out.push(ScheduledNote {
            id: hasher.finish() as u32 & CLIP_NOTE_MASK,
            start,
            end,
            key: note.key,
            velocity: note.velocity,
        });
    }
}

/// The clip that plays at sample `t`: the one that started last, if it hasn't
/// ended. An earlier clip it overlaps is cut where it begins and doesn't
/// resume.
pub fn active_at(schedule: &[ScheduledClip], t: u64) -> Option<&ScheduledClip> {
    let after = schedule.partition_point(|c| c.start <= t);
    schedule[..after].last().filter(|c| t < c.end())
}

/// The clip that `clip` is crossfading with, if it is still playing at `t`.
pub fn under_at<'a>(
    schedule: &'a [ScheduledClip],
    clip: &ScheduledClip,
    t: u64,
) -> Option<&'a ScheduledClip> {
    let id = clip.under?;
    let before = schedule.partition_point(|c| c.start <= clip.start);
    schedule[..before]
        .iter()
        .rev()
        .find(|c| c.id == id)
        .filter(|c| c.start <= t && t < c.end())
}

/// Splits a stretch of the timeline into runs where the same clip (or none)
/// plays. Yields `(from, to, clip)`.
pub struct Segments<'a> {
    schedule: &'a [ScheduledClip],
    at: u64,
    end: u64,
}

impl<'a> Segments<'a> {
    pub fn new(schedule: &'a [ScheduledClip], from: u64, to: u64) -> Self {
        Self {
            schedule,
            at: from,
            end: to,
        }
    }
}

impl<'a> Iterator for Segments<'a> {
    type Item = (u64, u64, Option<&'a ScheduledClip>);

    fn next(&mut self) -> Option<Self::Item> {
        if self.at >= self.end {
            return None;
        }
        let after = self.schedule.partition_point(|c| c.start <= self.at);
        let active = self.schedule[..after].last().filter(|c| self.at < c.end());
        let mut to = self.end;
        if let Some(c) = active {
            to = to.min(c.end());
            if let Some(under) = under_at(self.schedule, c, self.at) {
                to = to.min(under.end());
            }
        }
        if let Some(next) = self.schedule.get(after) {
            to = to.min(next.start);
        }
        let item = (self.at, to, active);
        self.at = to;
        Some(item)
    }
}

/// Whether two schedules sound the same over `from..to`: the same clips play
/// the same audio, at the same gain, over the same stretches. A clip's fades
/// only count where they reach the stretch being compared, so dragging a fade
/// handle far from the playhead changes nothing audible.
pub fn same_sound(a: &[ScheduledClip], b: &[ScheduledClip], from: u64, to: u64) -> bool {
    let (a_all, b_all) = (a, b);
    let mut a = Segments::new(a, from, to);
    let mut b = Segments::new(b, from, to);
    loop {
        match (a.next(), b.next()) {
            (None, None) => return true,
            (Some((s0, e0, x)), Some((s1, e1, y))) if (s0, e0) == (s1, e1) => {
                let same = match (x, y) {
                    (None, None) => true,
                    (Some(x), Some(y)) => {
                        same_clip_sound(x, y, s0, e0)
                            && match (under_at(a_all, x, s0), under_at(b_all, y, s0)) {
                                (None, None) => true,
                                (Some(ux), Some(uy)) => same_clip_sound(ux, uy, s0, e0),
                                _ => false,
                            }
                    }
                    _ => false,
                };
                if !same {
                    return false;
                }
            }
            _ => return false,
        }
    }
}

/// Whether two versions of a clip play the same over `from..to`, which lies
/// inside the clip. A fade only matters if the stretch reaches into it.
fn same_clip_sound(x: &ScheduledClip, y: &ScheduledClip, from: u64, to: u64) -> bool {
    let same_fade_in = x.fade_in == y.fade_in || {
        let rel = from - x.start;
        rel >= x.fade_in && rel >= y.fade_in
    };
    let same_fade_out = x.fade_out == y.fade_out || {
        // The fade-out covers the last `fade_out` frames.
        let rel_end = to - x.start;
        rel_end + x.fade_out <= x.length && rel_end + y.fade_out <= x.length
    };
    x.id == y.id
        && x.start == y.start
        && x.length == y.length
        && x.gain == y.gain
        && x.envelope == y.envelope
        && x.under == y.under
        && x.xfade_in == y.xfade_in
        && x.xfade_out == y.xfade_out
        && x.source == y.source
        && x.warp == y.warp
        && x.mode == y.mode
        && same_fade_in
        && same_fade_out
}

/// A clip that couldn't be scheduled, and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClipProblem {
    pub clip: ClipId,
    pub message: String,
}

/// Where the track input nodes get their clips from. Make one with
/// [`register_library`](crate::register_library), give the node types it
/// registers to the engine, and call [`update`](Self::update) whenever clips
/// or the tempo map change. It's cheap to clone; clones share everything.
#[derive(Clone, Default)]
pub struct ClipFeeds {
    inner: Arc<Inner>,
}

#[derive(Default)]
struct Inner {
    /// See [`ClipFeeds::blocking`].
    blocking: bool,
    hubs: Mutex<HashMap<NodeId, Arc<Shared>>>,
    /// See [`ClipFeeds::set_preview`].
    preview: Arc<std::sync::atomic::AtomicBool>,
    files: Mutex<HashMap<PathBuf, (Option<FileStamp>, FileInfo)>>,
}

/// What a track input is doing, for the UI.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ClipStatus {
    /// Streams ready to play.
    pub streams: usize,
    /// Blocks where audio was missing, because a stream wasn't ready or
    /// the disk fell behind.
    pub underruns: u64,
    /// The files that couldn't be opened, and why. Each is tried again every
    /// couple of seconds; an entry goes once its file opens.
    pub errors: Vec<FileError>,
}

/// A clip's file that couldn't be opened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileError {
    pub path: PathBuf,
    pub message: String,
}

impl std::fmt::Display for FileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.path.display(), self.message)
    }
}

impl ClipFeeds {
    pub(super) fn shared(&self, node: NodeId) -> Arc<Shared> {
        let blocking = self.inner.blocking;
        self.inner
            .hubs
            .lock()
            .expect("feeds lock")
            .entry(node)
            .or_insert_with(|| Arc::new(Shared::new(blocking, self.inner.preview.clone())))
            .clone()
    }

    /// Feeds for rendering offline. The track input nodes they serve wait
    /// for the schedule, their streams and the disk instead of playing
    /// silence when those are late, so a render comes out the same every
    /// time. Don't use them to play live: a slow disk would stall the audio
    /// thread.
    pub fn blocking() -> Self {
        Self {
            inner: Arc::new(Inner {
                blocking: true,
                ..Inner::default()
            }),
        }
    }

    /// Whether these feeds are for rendering offline (see
    /// [`blocking`](Self::blocking)).
    pub fn is_blocking(&self) -> bool {
        self.inner.blocking
    }

    /// Blocks where audio was missing, summed over every node.
    pub fn underruns(&self) -> u64 {
        let hubs = self.inner.hubs.lock().expect("feeds lock");
        hubs.values().map(|s| s.status().underruns).sum()
    }

    /// Every file that couldn't be opened, over every node.
    pub fn errors(&self) -> Vec<FileError> {
        let hubs = self.inner.hubs.lock().expect("feeds lock");
        let mut errors: Vec<FileError> = Vec::new();
        for error in hubs.values().flat_map(|s| s.errors()) {
            if !errors.contains(&error) {
                errors.push(error);
            }
        }
        errors
    }

    /// Holds `key` down (or lets it go) on the track input `node`, as if a
    /// note were playing, for the piano roll's keyboard. The node plays it
    /// from the next block, whether or not the transport runs.
    pub fn audition(&self, node: NodeId, key: u8, on: bool) {
        self.shared(node).set_audition(key, on);
    }

    /// Turns live monitoring on or off for the track input `node`: while on,
    /// it plays the live MIDI of the bus it was built with (see
    /// [`TrackInput::with_midi`](super::TrackInput::with_midi)), without the
    /// transport having to run.
    pub fn set_monitoring(&self, node: NodeId, on: bool) {
        self.shared(node)
            .monitoring
            .store(on, std::sync::atomic::Ordering::Relaxed);
    }

    /// While on, warped clips play as Repitch whatever their mode: cheap
    /// enough to follow a warp marker that is being dragged, where the
    /// stretch would have to start over with every change. Turn it off when
    /// the edit is done and the stretch takes over (silent for a moment while
    /// it renders).
    pub fn set_preview(&self, on: bool) {
        self.inner
            .preview
            .store(on, std::sync::atomic::Ordering::Relaxed);
        for shared in self.inner.hubs.lock().expect("feeds lock").values() {
            shared.ring();
        }
    }

    /// Whether the preview is on (see [`set_preview`](Self::set_preview)).
    pub fn preview(&self) -> bool {
        self.inner
            .preview
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// How many times the background tasks of the track input `node` (the one
    /// that opens streams, and the one that stretches clips) have run since it
    /// was built. They sleep until there is work, so this stays put while
    /// nothing plays. For tests.
    pub fn background_runs(&self, node: NodeId) -> u64 {
        self.shared(node).runs()
    }

    /// How many nodes the feeds are keeping a schedule for.
    pub fn tracked(&self) -> usize {
        self.inner.hubs.lock().expect("feeds lock").len()
    }

    pub fn status(&self, node: NodeId) -> ClipStatus {
        self.shared(node).status()
    }

    /// Schedules every clip in `project` on the track input node it
    /// names, at the positions the tempo map gives. Files are looked up
    /// relative to `base`, normally the folder of the project file. Nodes
    /// that no longer have clips are emptied. Returns the clips that couldn't
    /// be scheduled, which are left out.
    pub fn update(
        &self,
        project: &Project,
        table: &TempoTable,
        rate: u32,
        base: &Path,
    ) -> Vec<ClipProblem> {
        let mut problems = Vec::new();
        let mut by_node: HashMap<NodeId, Schedule> = HashMap::new();
        let mut notes_by_node: HashMap<NodeId, Notes> = HashMap::new();
        let mut crossfades: HashMap<NodeId, Vec<(ClipId, u64, u64)>> = HashMap::new();
        for (id, clip) in project.clips() {
            let audio = match &clip.content {
                ClipContent::Audio(audio) => audio,
                ClipContent::Midi(midi) => {
                    schedule_notes(
                        table,
                        id,
                        clip,
                        midi,
                        notes_by_node.entry(clip.node).or_default(),
                    );
                    continue;
                }
            };
            // A muted section plays nothing, so it is neither scheduled nor
            // read.
            if audio.muted {
                continue;
            }
            let path = base.join(&audio.source);
            let info = match self.file_info(&path) {
                Ok(info) => info,
                Err(message) => {
                    problems.push(ClipProblem { clip: id, message });
                    continue;
                }
            };
            let to_engine = |frames: u64| {
                (frames as f64 * f64::from(rate) / f64::from(info.sample_rate)).round() as u64
            };
            if clip_frames(&info, audio.offset, audio.length, rate) == 0 {
                problems.push(ClipProblem {
                    clip: id,
                    message: "the clip is past the end of its file".into(),
                });
                continue;
            }
            let (pieces, problem) = lay_out(project, clip, audio, &info, table, rate);
            if let Some(message) = problem {
                problems.push(ClipProblem { clip: id, message });
            }
            let first_start = table.sample_at_tick(clip.start);
            let last = pieces.len() - 1;
            let src_ratio = f64::from(info.sample_rate) / f64::from(rate);
            let after_window = info.frames.map_or(0, |frames| {
                frames.saturating_sub(audio.offset + audio.length)
            });
            if let Some(x) = audio.crossfade.filter(|x| !x.is_empty()) {
                let at = |ticks: i64| table.sample_at_tick(Tick(ticks.max(0)));
                crossfades.entry(clip.node).or_default().push((
                    piece_id(id, 0),
                    first_start - at(clip.start.0 - x.before.0),
                    at(clip.start.0 + x.after.0) - first_start,
                ));
            }
            for (k, piece) in pieces.into_iter().enumerate() {
                // Fades are counted in the file's frames; a warp stretches
                // them along with the audio. A section fades in over its
                // first piece and out over its last.
                let (mut fade_in, mut fade_out) = (0, 0);
                match &piece.warp {
                    None => {
                        fade_in = to_engine(audio.fade_in);
                        fade_out = to_engine(audio.fade_out);
                    }
                    Some((map, _)) => {
                        if k == 0 {
                            fade_in = map.out_at(to_engine(audio.fade_in) as f64).round() as u64;
                        }
                        if k == last {
                            fade_out = (map.out_len()
                                - map.out_at(map.source_len() - to_engine(audio.fade_out) as f64))
                            .round() as u64;
                        }
                    }
                }
                // How far the file lets the clip play on at either end: the
                // section's first piece back before its window, its last
                // past it. A warped piece reads at the speed it has there.
                let (mut head_room, mut tail_room) = (0, 0);
                let slopes = piece.warp.as_ref().map(|(map, _)| {
                    let knots = map.knots();
                    let n = knots.len();
                    (
                        knots[1].1 / knots[1].0,
                        (knots[n - 1].1 - knots[n - 2].1) / (knots[n - 1].0 - knots[n - 2].0),
                    )
                });
                let (head_slope, tail_slope) = slopes.unwrap_or((1.0, 1.0));
                if k == 0 {
                    head_room = (to_engine(piece.offset) as f64 / head_slope).floor() as u64;
                }
                if k == last && info.frames.is_some() {
                    tail_room = (to_engine(after_window) as f64 / tail_slope).floor() as u64;
                }
                let mut scheduled = ScheduledClip::new(
                    piece_id(id, k),
                    first_start + piece.from,
                    piece.len,
                    audio.gain,
                    fade_in.min(piece.len),
                    fade_out.min(piece.len),
                    ClipSource {
                        stamp: std::fs::metadata(&path).and_then(|m| m.modified()).ok(),
                        path: path.clone(),
                        offset: piece.offset,
                        length: piece.length,
                    },
                )
                .with_room(head_room, tail_room, src_ratio);
                if !audio.gain_points.is_empty() {
                    let place = first_start as i64 + piece.from as i64;
                    scheduled = scheduled.with_envelope(
                        audio
                            .gain_points
                            .iter()
                            .map(|p| {
                                let at = table.sample_at_tick(Tick(clip.start.0 + p.tick.0));
                                (at as i64 - place, p.db)
                            })
                            .collect(),
                    );
                }
                by_node
                    .entry(clip.node)
                    .or_default()
                    .push(match piece.warp {
                        Some((map, mode)) => scheduled.with_warp(map).with_mode(mode),
                        None => scheduled,
                    });
            }
        }
        let mut hubs = self.inner.hubs.lock().expect("feeds lock");
        for node in by_node.keys().chain(notes_by_node.keys()) {
            hubs.entry(*node).or_insert_with(|| {
                Arc::new(Shared::new(self.inner.blocking, self.inner.preview.clone()))
            });
        }
        for (node, shared) in hubs.iter() {
            let mut schedule = by_node.remove(node).unwrap_or_default();
            schedule.sort_by_key(|c| (c.start, c.id));
            if let Some(wanted) = crossfades.get(node) {
                apply_crossfades(&mut schedule, wanted, rate);
                schedule.sort_by_key(|c| (c.start, c.id));
            }
            let mut notes = notes_by_node.remove(node).unwrap_or_default();
            notes.sort_by_key(|n| (n.start, n.id));
            shared.set(schedule, notes);
        }
        // Forget nodes that are gone: nothing holds their entry but this map
        // once the node (and with it the hub thread) has been dropped.
        hubs.retain(|_, shared| Arc::strong_count(shared) > 1 || !shared.is_empty());
        problems
    }

    /// Where the last clip in `project` ends, in frames at `rate`; 0 for a
    /// project with no clips. A clip whose file can't be read counts as
    /// ending where it starts.
    pub fn project_frames(
        &self,
        project: &Project,
        table: &TempoTable,
        rate: u32,
        base: &Path,
    ) -> u64 {
        project
            .clips()
            .map(|(_, clip)| {
                let start = table.sample_at_tick(clip.start);
                match &clip.content {
                    ClipContent::Midi(midi) => {
                        table.sample_at_tick(noodle_core::Tick(clip.start.0 + midi.length.0))
                    }
                    ClipContent::Audio(audio) => {
                        let path = base.join(&audio.source);
                        self.file_info(&path).map_or(start, |info| {
                            let (pieces, _) = lay_out(project, clip, audio, &info, table, rate);
                            start + pieces.last().map_or(0, |p| p.from + p.len)
                        })
                    }
                }
            })
            .max()
            .unwrap_or(0)
    }

    fn file_info(&self, path: &Path) -> Result<FileInfo, String> {
        // The one rule for "changed" (see `noodle_io::AudioFiles`), read from
        // what the watcher last saw rather than from the disk.
        let modified = AudioFiles::global().stamp(path);
        let mut files = self.inner.files.lock().expect("feeds lock");
        if let Some((seen, info)) = files.get(path)
            && *seen == modified
        {
            return Ok(*info);
        }
        let info = Decoder::open(path)
            .map_err(|e| format!("{}: {e}", path.display()))?
            .info();
        files.insert(path.to_path_buf(), (modified, info));
        Ok(info)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn clip(id: u64, start: u64, length: u64) -> ScheduledClip {
        ScheduledClip::new(
            ClipId(id),
            start,
            length,
            1.0,
            0,
            0,
            ClipSource {
                path: format!("{id}.wav").into(),
                offset: 0,
                length,
                stamp: None,
            },
        )
    }

    fn runs(schedule: &[ScheduledClip], from: u64, to: u64) -> Vec<(u64, u64, Option<u64>)> {
        Segments::new(schedule, from, to)
            .map(|(a, b, c)| (a, b, c.map(|c| c.id.0)))
            .collect()
    }

    #[test]
    fn segments_follow_the_clips_with_gaps_between() {
        let schedule = [clip(1, 100, 50), clip(2, 200, 100)];
        assert_eq!(
            runs(&schedule, 0, 400),
            [
                (0, 100, None),
                (100, 150, Some(1)),
                (150, 200, None),
                (200, 300, Some(2)),
                (300, 400, None)
            ]
        );
        // A window inside a clip, and one that ends at a boundary.
        assert_eq!(runs(&schedule, 110, 120), [(110, 120, Some(1))]);
        assert_eq!(runs(&schedule, 140, 150), [(140, 150, Some(1))]);
        assert_eq!(runs(&schedule, 5, 5), []);
    }

    #[test]
    fn a_later_clip_cuts_an_earlier_one_which_does_not_resume() {
        // 2 starts inside 1 and ends before it would have.
        let schedule = [clip(1, 0, 100), clip(2, 40, 20)];
        assert_eq!(
            runs(&schedule, 0, 120),
            [(0, 40, Some(1)), (40, 60, Some(2)), (60, 120, None)]
        );
        assert_eq!(active_at(&schedule, 70).map(|c| c.id.0), None);
        assert_eq!(active_at(&schedule, 50).map(|c| c.id.0), Some(2));
        assert_eq!(active_at(&schedule, 39).map(|c| c.id.0), Some(1));
    }

    #[test]
    fn schedules_that_differ_only_far_away_sound_the_same_here() {
        let a = [clip(1, 0, 100), clip(2, 1000, 50)];
        let mut b = a.clone();
        b[1].gain = 0.5;
        assert!(same_sound(&a, &b, 0, 512));
        assert!(!same_sound(&a, &b, 990, 1100));
        // Moving the later clip far away leaves the early window alone.
        b[1].gain = 1.0;
        b[1].start = 2000;
        assert!(same_sound(&a, &b, 0, 512));
        assert!(!same_sound(&a, &b, 990, 1100));
        // Changing the clip that is heard changes the sound, even before the
        // part that differs, because the comparison is clip by clip.
        b[1] = a[1].clone();
        b[0].length = 90;
        assert!(!same_sound(&a, &b, 80, 100));
        assert!(!same_sound(&a, &b, 0, 80));
        assert!(same_sound(&a, &b, 100, 512));
    }

    #[test]
    fn a_fade_only_matters_where_it_reaches() {
        let a = [clip(1, 0, 10_000)];
        // Dragging the fade-in handle: it only reaches the first 1000 frames.
        let mut b = a.clone();
        b[0].fade_in = 1_000;
        assert!(same_sound(&a, &b, 2_000, 2_512));
        assert!(!same_sound(&a, &b, 900, 1_412));
        assert!(!same_sound(&a, &b, 0, 512));
        // Likewise the fade-out, over the last 1000 frames.
        let mut c = a.clone();
        c[0].fade_out = 1_000;
        assert!(same_sound(&a, &c, 2_000, 2_512));
        assert!(same_sound(&a, &c, 8_488, 9_000));
        assert!(!same_sound(&a, &c, 8_489, 9_001));
        assert!(!same_sound(&a, &c, 9_488, 10_000));
        // Moving a handle between two places the block never reaches.
        let mut d = b.clone();
        d[0].fade_in = 1_500;
        assert!(same_sound(&b, &d, 2_000, 2_512));
        assert!(!same_sound(&b, &d, 1_200, 1_712));
        // Gain is heard everywhere in the clip.
        let mut e = a.clone();
        e[0].gain = 0.5;
        assert!(!same_sound(&a, &e, 2_000, 2_512));
    }

    #[test]
    fn an_extended_clip_plays_more_of_its_file_and_keeps_its_envelope_in_place() {
        let mut c = clip(1, 1_000, 500).with_room(300, 200, 1.0);
        c.source.offset = 400;
        let c = c.with_envelope(vec![(0, -6.0), (500, 0.0)]);
        let e = c.extended(250, 150);
        assert_eq!((e.start, e.length), (750, 900));
        assert_eq!((e.source.offset, e.source.length), (150, 900));
        assert_eq!((e.head_room, e.tail_room), (50, 50));
        // The same envelope, 250 frames later in the longer clip.
        assert_eq!(e.envelope.as_deref(), Some(&[(250, -6.0), (750, 0.0)][..]));
        assert!((e.level(250) - c.level(0)).abs() < 1e-6);
        // It can't go past the room the file has.
        let e = c.extended(900, 900);
        assert_eq!((e.start, e.end()), (700, 1_700));
    }

    #[test]
    fn butted_clips_are_laid_out_as_an_overlapping_crossfade_pair() {
        let a = clip(1, 0, 1_000).with_room(0, 400, 1.0);
        let b = clip(2, 1_000, 1_000).with_room(300, 0, 1.0);
        let mut schedule = vec![a, b];
        apply_crossfades(&mut schedule, &[(ClipId(2), 200, 350)], 48_000);
        let (a, b) = (&schedule[0], &schedule[1]);
        // B reaches back 200 frames (it has 300 free) and A plays 350 on.
        assert_eq!((b.start, b.fade_in, b.under), (800, 550, Some(ClipId(1))));
        assert_eq!((a.end(), a.fade_out), (1_350, 550));
        assert!(a.xfade_out && b.xfade_in);
        // Both are heard from the start of B to the end of A, and each pair
        // of levels adds up to the same power.
        schedule.sort_by_key(|c| (c.start, c.id));
        let runs: Vec<_> = Segments::new(&schedule, 0, 2_000)
            .map(|(from, to, c)| (from, to, c.map(|c| c.id.0)))
            .collect();
        assert_eq!(
            runs,
            [
                (0, 800, Some(1)),
                (800, 1_350, Some(2)),
                (1_350, 2_000, Some(2))
            ]
        );
        let under = under_at(&schedule, &schedule[1], 900).map(|c| c.id.0);
        assert_eq!(under, Some(1));
        assert!(under_at(&schedule, &schedule[1], 1_400).is_none());
        let power = schedule[0].level(900).powi(2) + schedule[1].level(100).powi(2);
        assert!((power - 1.0).abs() < 0.01, "{power}");
    }

    #[test]
    fn clips_that_are_not_butted_do_not_crossfade() {
        let a = clip(1, 0, 900).with_room(0, 400, 1.0);
        let b = clip(2, 1_000, 1_000).with_room(300, 0, 1.0);
        let mut schedule = vec![a.clone(), b.clone()];
        apply_crossfades(&mut schedule, &[(ClipId(2), 200, 200)], 48_000);
        assert_eq!(schedule, [a, b]);
    }

    #[test]
    fn the_sound_differs_when_a_crossfade_does() {
        let a = clip(1, 0, 1_000).with_room(0, 400, 1.0);
        let b = clip(2, 1_000, 1_000).with_room(300, 0, 1.0);
        let plain = vec![a.clone(), b.clone()];
        let mut crossfaded = plain.clone();
        apply_crossfades(&mut crossfaded, &[(ClipId(2), 100, 100)], 48_000);
        crossfaded.sort_by_key(|c| (c.start, c.id));
        assert!(!same_sound(&plain, &crossfaded, 900, 1_100));
        let mut longer = plain.clone();
        apply_crossfades(&mut longer, &[(ClipId(2), 100, 200)], 48_000);
        longer.sort_by_key(|c| (c.start, c.id));
        assert!(!same_sound(&crossfaded, &longer, 900, 1_100));
    }

    #[test]
    fn the_envelope_is_straight_in_db() {
        let c = clip(1, 0, 1_000).with_envelope(vec![(100, 0.0), (300, -20.0)]);
        assert!((c.level(0) - 1.0).abs() < 1e-6);
        assert!((c.level(200) - 10f32.powf(-0.5)).abs() < 1e-4);
        assert!((c.level(900) - 0.1).abs() < 1e-5);
    }

    #[test]
    fn a_file_replaced_with_the_same_mtime_but_another_length_is_read_again() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.wav");
        noodle_io::write_wav(&path, &vec![0.0; 100], 1, 48_000).unwrap();
        let mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
        let feeds = ClipFeeds::default();
        assert_eq!(feeds.file_info(&path).unwrap().frames, Some(100));

        noodle_io::write_wav(&path, &vec![0.0; 200], 1, 48_000).unwrap();
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(mtime)
            .unwrap();
        // The watcher thread looks once a second.
        let start = std::time::Instant::now();
        while feeds.file_info(&path).unwrap().frames != Some(200) {
            assert!(
                start.elapsed() < std::time::Duration::from_secs(10),
                "still the old length"
            );
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }
}
