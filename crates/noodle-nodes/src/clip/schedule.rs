//! What a track input plays, in the engine's own units: the project's clips
//! for one node, turned into sample positions, sorted by start.

use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use noodle_core::{ClipContent, ClipId, NodeId, Project};
use noodle_engine::TempoTable;
use noodle_io::{Decoder, FileInfo, clip_frames};

use super::hub::Shared;

/// Which part of which file a clip plays.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ClipSource {
    pub path: PathBuf,
    /// Where the clip starts in the file, in the file's frames.
    pub offset: u64,
    /// How much of the file it plays, in the file's frames.
    pub length: u64,
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
    key: u64,
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
            key: hasher.finish(),
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

/// Note IDs of notes in clips use the low 30 bits. The top two are for notes
/// from elsewhere: the MIDI In node sets the top bit, and the piano roll's
/// keyboard (see [`ClipFeeds::audition`]) the one below it.
pub const CLIP_NOTE_MASK: u32 = 0x3FFF_FFFF;

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
    for note in &midi.notes {
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
    let mut a = Segments::new(a, from, to);
    let mut b = Segments::new(b, from, to);
    loop {
        match (a.next(), b.next()) {
            (None, None) => return true,
            (Some((s0, e0, x)), Some((s1, e1, y))) if (s0, e0) == (s1, e1) => {
                let same = match (x, y) {
                    (None, None) => true,
                    (Some(x), Some(y)) => same_clip_sound(x, y, s0, e0),
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
        && x.source == y.source
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
    files: Mutex<HashMap<PathBuf, (Option<SystemTime>, FileInfo)>>,
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
            .or_insert_with(|| Arc::new(Shared::new(blocking)))
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
            let length = clip_frames(&info, audio.offset, audio.length, rate);
            if length == 0 {
                problems.push(ClipProblem {
                    clip: id,
                    message: "the clip is past the end of its file".into(),
                });
                continue;
            }
            by_node
                .entry(clip.node)
                .or_default()
                .push(ScheduledClip::new(
                    id,
                    table.sample_at_tick(clip.start),
                    length,
                    audio.gain,
                    to_engine(audio.fade_in).min(length),
                    to_engine(audio.fade_out).min(length),
                    ClipSource {
                        path,
                        offset: audio.offset,
                        length: audio.length,
                    },
                ));
        }
        let mut hubs = self.inner.hubs.lock().expect("feeds lock");
        for node in by_node.keys().chain(notes_by_node.keys()) {
            hubs.entry(*node)
                .or_insert_with(|| Arc::new(Shared::new(self.inner.blocking)));
        }
        for (node, shared) in hubs.iter() {
            let mut schedule = by_node.remove(node).unwrap_or_default();
            schedule.sort_by_key(|c| (c.start, c.id));
            let mut notes = notes_by_node.remove(node).unwrap_or_default();
            notes.sort_by_key(|n| (n.start, n.id));
            shared.set(schedule, notes);
        }
        // Forget nodes that are gone: nothing holds their entry but this map
        // once the node (and with it the hub thread) has been dropped.
        hubs.retain(|_, shared| Arc::strong_count(shared) > 1 || !shared.is_empty());
        problems
    }

    fn file_info(&self, path: &Path) -> Result<FileInfo, String> {
        let modified = std::fs::metadata(path).and_then(|m| m.modified()).ok();
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
}
