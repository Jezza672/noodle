//! Clips: audio (and from M3, MIDI) placed on the timeline for a track input node to play.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::{NodeId, TICKS_PER_QUARTER, Tick, WarpId};

/// Identifies a clip within a project. Like node IDs, clip IDs aren't reused
/// within a session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ClipId(pub u64);

impl fmt::Display for ClipId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "clip #{}", self.0)
    }
}

/// Identifies a [`ClipGroup`] within a project. Like clip IDs, they aren't
/// reused within a session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ClipGroupId(pub u64);

impl fmt::Display for ClipGroupId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "clip group #{}", self.0)
    }
}

/// A link between clips, usually on different tracks: the members move, trim,
/// split, loop, warp and are deleted together, and share one warp (see
/// [`Project::section_edit`](crate::Project::section_edit)). Gain and fades
/// stay per clip. A group may hold audio and MIDI clips; the warp reaches the
/// audio ones.
///
/// A group is only a link while at least two of its members are in the
/// project: a member that has been deleted (and could come back with an undo)
/// is kept in the list but ignored, and a group with fewer live members than
/// that is neither saved nor reported by [`Project::group_of`](crate::Project::group_of).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClipGroup {
    pub members: std::collections::BTreeSet<ClipId>,
}

/// Something placed on a track's timeline. The start is in ticks, so it
/// follows the tempo. One track holds clips of every kind.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Clip {
    /// The track input node that plays this clip.
    pub node: NodeId,
    pub start: Tick,
    pub content: ClipContent,
}

/// What a clip plays.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ClipContent {
    Audio(AudioClip),
    Midi(MidiClip),
}

/// Notes on the timeline. Everything is in ticks, so a MIDI clip follows the
/// tempo (unlike audio, which keeps its own length in samples).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MidiClip {
    /// How long the clip is. Notes past its end are cut off there.
    pub length: Tick,
    /// Not in any particular order; the engine sorts what it plays.
    pub notes: Vec<MidiNote>,
    /// Whether the clip repeats a section of itself: it plays from its start
    /// to the end of the loop, then the loop section again and again until
    /// the clip's end. Notes after the loop's end don't play.
    #[serde(default, skip_serializing_if = "is_false")]
    pub loop_on: bool,
    /// Where the loop section starts, from the start of the clip.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub loop_start: Tick,
    /// How long the loop section is; at least [`MIN_LOOP_LEN`] while looping.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub loop_len: Tick,
}

fn is_zero(tick: &Tick) -> bool {
    *tick == Tick::ZERO
}

/// The shortest loop a MIDI clip may have: a sixteenth of a beat.
pub const MIN_LOOP_LEN: i64 = TICKS_PER_QUARTER / 16;

/// One note in a MIDI clip.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct MidiNote {
    /// Names the note within its clip, and stays with it as it is edited, so
    /// a view can keep it selected through undo. The project makes ids
    /// unique when a clip enters it (see [`MidiClip::assign_note_ids`]); files
    /// saved before notes had ids load with all zeros and are fixed that way.
    #[serde(default)]
    pub id: u32,
    /// Where the note starts, from the start of the clip.
    pub start: Tick,
    pub length: Tick,
    /// The MIDI key number, 0 to 127.
    pub key: u8,
    /// 0 to 1.
    pub velocity: f32,
}

impl MidiNote {
    pub fn new(start: Tick, length: Tick, key: u8) -> Self {
        Self {
            id: 0,
            start,
            length,
            key,
            velocity: 0.8,
        }
    }

    /// The first tick after the note.
    pub fn end(&self) -> Tick {
        Tick(self.start.0 + self.length.0)
    }
}

impl MidiClip {
    pub fn new(length: Tick) -> Self {
        Self {
            length,
            notes: Vec::new(),
            loop_on: false,
            loop_start: Tick::ZERO,
            loop_len: Tick::ZERO,
        }
    }

    /// The first tick after the loop section.
    pub fn loop_end(&self) -> Tick {
        Tick(self.loop_start.0 + self.loop_len.0)
    }

    /// Turns looping on over the whole clip (or keeps the section the clip
    /// had the last time it looped, if it still fits), or off.
    pub fn set_looping(&mut self, on: bool) {
        self.loop_on = on;
        if on && (self.loop_len.0 < MIN_LOOP_LEN || self.loop_start.0 >= self.length.0) {
            self.loop_start = Tick::ZERO;
            self.loop_len = self.length;
        }
    }

    /// The loop section, put right: inside the clip and at least
    /// [`MIN_LOOP_LEN`] long.
    pub fn set_loop(&mut self, start: Tick, end: Tick) {
        let start = start.0.clamp(0, (self.length.0 - MIN_LOOP_LEN).max(0));
        self.loop_start = Tick(start);
        self.loop_len =
            Tick((end.0 - start).clamp(MIN_LOOP_LEN, (self.length.0 - start).max(MIN_LOOP_LEN)));
    }

    /// The notes of the later passes through the loop, as they sound: each
    /// note that starts in the loop section, shifted by whole loops and cut
    /// at the loop's end, until the clip's end. Empty when not looping.
    pub fn repeats(&self) -> Vec<MidiNote> {
        let mut out = Vec::new();
        if !self.loop_on || self.loop_len.0 < MIN_LOOP_LEN {
            return out;
        }
        let (from, end) = (self.loop_start.0, self.loop_end().0);
        let in_loop: Vec<&MidiNote> = self
            .notes
            .iter()
            .filter(|n| n.start.0 >= from && n.start.0 < end)
            .collect();
        let mut shift = self.loop_len.0;
        while from + shift < self.length.0 {
            for note in &in_loop {
                let start = note.start.0 + shift;
                if start >= self.length.0 {
                    continue;
                }
                out.push(MidiNote {
                    start: Tick(start),
                    length: Tick(note.length.0.min(end - note.start.0)),
                    ..**note
                });
            }
            shift += self.loop_len.0;
        }
        out
    }

    /// Every note as it sounds, in clip time: the first pass (cut at the
    /// loop's end, and the notes past it dropped, if the clip loops) then the
    /// repeats. Notes starting past the clip's end are left out; one that
    /// runs past it is cut by whoever plays it.
    pub fn played(&self) -> Vec<MidiNote> {
        let mut out: Vec<MidiNote> = Vec::with_capacity(self.notes.len());
        for note in &self.notes {
            if note.start.0 >= self.length.0 {
                continue;
            }
            if self.loop_on && self.loop_len.0 >= MIN_LOOP_LEN {
                let end = self.loop_end().0;
                if note.start.0 >= end {
                    continue;
                }
                out.push(MidiNote {
                    length: Tick(note.length.0.min(end - note.start.0)),
                    ..*note
                });
            } else {
                out.push(*note);
            }
        }
        out.extend(self.repeats());
        out
    }

    /// Gives each note whose id repeats an earlier one's a new id, so ids are
    /// unique in the clip. Notes with unique ids keep theirs.
    pub fn assign_note_ids(&mut self) {
        let mut seen = std::collections::BTreeSet::new();
        let Some(mut next) = self
            .notes
            .iter()
            .map(|n| n.id)
            .max()
            .map_or(Some(0), |m| m.checked_add(1))
        else {
            // No id above the largest to hand out: number them all afresh.
            for (i, note) in self.notes.iter_mut().enumerate() {
                note.id = i as u32;
            }
            return;
        };
        for note in &mut self.notes {
            if !seen.insert(note.id) {
                note.id = next;
                next += 1;
                seen.insert(note.id);
            }
        }
    }

    fn problem(&self) -> Option<&'static str> {
        if self.length <= Tick::ZERO {
            return Some("it is empty");
        }
        if self.loop_on && (self.loop_start < Tick::ZERO || self.loop_len.0 < MIN_LOOP_LEN) {
            return Some("its loop starts before the clip or is too short");
        }
        for note in &self.notes {
            if note.start < Tick::ZERO {
                return Some("a note starts before the clip");
            }
            if note.length <= Tick::ZERO {
                return Some("a note is empty");
            }
            if note.key > 127 {
                return Some("a note's key is out of range");
            }
            if !(0.0..=1.0).contains(&note.velocity) {
                return Some("a note's velocity is not between 0 and 1");
            }
        }
        None
    }
}

/// Part of an audio file. What plays, and for how long, is counted in the
/// file's own samples, so a tempo change moves the clip without changing how
/// it sounds.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AudioClip {
    /// The audio file, as a path relative to the project file.
    pub source: String,
    /// Where in the file the clip starts, in the file's sample frames.
    pub offset: u64,
    /// How much of the file plays, in the file's sample frames.
    pub length: u64,
    /// A linear gain.
    pub gain: f32,
    /// Fades at either end, in sample frames of the file, inside `length`.
    pub fade_in: u64,
    pub fade_out: u64,
    /// The gain envelope over the clip, on top of `gain`: points in timeline
    /// time from the clip's start, sorted by time. Between points the level
    /// moves in a straight line in dB; before the first and after the last it
    /// holds. Empty means a flat clip.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gain_points: Vec<GainPoint>,
    /// A crossfade into this clip from the one butted against its start (see
    /// [`Crossfade`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crossfade: Option<Crossfade>,
    /// How the clip is warped, if it is. Sections of the same clip name the
    /// same warp; `offset` and `length` still pick the part of the file this
    /// section shows, and the warp decides how that part is laid out in time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warp: Option<WarpId>,
    /// How long the section lasts, from its start, when its clip loops and
    /// the section is dragged out past the end of its window. `None` (or a
    /// clip that doesn't loop) lasts as long as the window does once warped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extent: Option<Tick>,
    /// Whether the section is silenced. It stays where it is, so it can be
    /// brought back; Split to stems mutes its source this way.
    #[serde(default, skip_serializing_if = "is_false")]
    pub muted: bool,
    /// The takes behind this section, when it is a take folder (see
    /// [`TakeFolder`]). `source`, `offset` and `length` then give the window
    /// every take is cut to, in the coordinates of the first take.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub takes: Option<TakeFolder>,
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// Several recordings of the same stretch of music, and the comp that picks
/// which one plays where. It is the `takes` of an [`AudioClip`], so a take
/// folder is one clip: moving, splitting, copying and linking it carries every
/// take, and the takes share the clip's warp.
///
/// The takes are windows into the clip's one audio file (a loop recording
/// writes a single file and each cycle is a take). The clip's `offset` and
/// `length` are counted in the *first coordinates*: the file's frames as the
/// first take sees them. Take `k` plays the file `delta` frames later than
/// that for the same moment of the music, so trimming and splitting the clip
/// move every take with it and need no care.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TakeFolder {
    pub takes: Vec<Take>,
    /// The comp: which take plays from each point on. Sorted by `at`; the
    /// first span also covers everything before it.
    pub comp: Vec<CompSpan>,
}

/// One take of a [`TakeFolder`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Take {
    /// How many frames later in the file this take plays what the first take
    /// plays at the same moment. A loop recording's take `k` is `k` cycles
    /// on.
    pub delta: u64,
}

/// A stretch of a comp: from `at` (in the clip's first coordinates) until the
/// next span, the take `take` plays.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompSpan {
    pub at: u64,
    pub take: usize,
    /// The crossfade at the join into this span (`None` for the first span).
    /// `None` gives the default of [`DEFAULT_COMP_FADE_SECONDS`]. The
    /// before and after are in ticks, as for [`AudioClip::crossfade`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crossfade: Option<Crossfade>,
}

/// How long a comp join crossfades unless it has been set: half of this
/// either side of the join.
pub const DEFAULT_COMP_FADE_SECONDS: f64 = 0.010;

impl TakeFolder {
    /// A folder of `takes` that plays `active` throughout.
    pub fn new(takes: Vec<Take>, active: usize) -> Self {
        Self {
            takes,
            comp: vec![CompSpan {
                at: 0,
                take: active,
                crossfade: None,
            }],
        }
    }

    /// The span that plays at `at`.
    pub fn span_at(&self, at: u64) -> usize {
        self.comp
            .partition_point(|span| span.at <= at)
            .saturating_sub(1)
    }

    /// Where span `index` ends, `None` if it runs on for good.
    pub fn span_end(&self, index: usize) -> Option<u64> {
        self.comp.get(index + 1).map(|next| next.at)
    }

    /// Makes `take` play from `from` to `to` (in the clip's first
    /// coordinates), leaving the rest of the comp as it is. The new joins get
    /// the default crossfade; spans left side by side with the same take
    /// become one. Does nothing if the range is empty or `take` doesn't exist.
    pub fn swipe(&mut self, from: u64, to: u64, take: usize) {
        if from >= to || take >= self.takes.len() {
            return;
        }
        // The comp as stretches: start, end, take, the join into it.
        let mut parts: Vec<(u64, u64, usize, Option<Crossfade>)> = Vec::new();
        for (i, span) in self.comp.iter().enumerate() {
            let start = if i == 0 { 0 } else { span.at };
            let end = self.span_end(i).unwrap_or(u64::MAX);
            parts.push((start, end, span.take, span.crossfade));
        }
        let mut out: Vec<(u64, u64, usize, Option<Crossfade>)> = Vec::new();
        for (start, end, was, join) in parts {
            if end <= from || start >= to {
                out.push((start, end, was, join));
                continue;
            }
            if start < from {
                out.push((start, from, was, join));
            }
            if end > to {
                out.push((to, end, was, None));
            }
        }
        let at = out.partition_point(|part| part.0 < from);
        out.insert(at, (from, to, take, None));
        // The first span has no join into it, and equal neighbours merge.
        let mut comp: Vec<CompSpan> = Vec::new();
        for (start, _, take, join) in out {
            if comp.last().is_some_and(|last| last.take == take) {
                continue;
            }
            comp.push(CompSpan {
                at: start,
                take,
                crossfade: if comp.is_empty() { None } else { join },
            });
        }
        if let Some(first) = comp.first_mut() {
            first.at = 0;
        }
        self.comp = comp;
    }

    /// Sets the crossfade of the join into span `index` (not the first).
    pub fn set_join(&mut self, index: usize, crossfade: Option<Crossfade>) {
        if index > 0
            && let Some(span) = self.comp.get_mut(index)
        {
            span.crossfade = crossfade;
        }
    }

    /// The takes the comp plays, in order of first use, without repeats.
    pub fn used(&self) -> Vec<usize> {
        let mut used = Vec::new();
        for span in &self.comp {
            if !used.contains(&span.take) {
                used.push(span.take);
            }
        }
        used
    }

    /// Why this folder can't be played, if it can't.
    fn problem(&self) -> Option<&'static str> {
        if self.takes.is_empty() {
            Some("its take folder has no takes")
        } else if self.comp.is_empty() {
            Some("its comp is empty")
        } else if self.comp.iter().any(|span| span.take >= self.takes.len()) {
            Some("its comp names a take that isn't there")
        } else if self.comp.windows(2).any(|w| w[1].at <= w[0].at) {
            Some("its comp is out of order")
        } else if self.comp.iter().any(|span| {
            span.crossfade
                .is_some_and(|x| x.before < Tick::ZERO || x.after < Tick::ZERO)
        }) {
            Some("a join in its comp has a negative crossfade")
        } else {
            None
        }
    }
}

/// One control point of a clip's gain envelope.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct GainPoint {
    /// Where, in ticks from the start of the clip.
    pub tick: Tick,
    pub db: f32,
}

/// Gain points as text, for a setting that holds an envelope (a Sampler's,
/// where `tick` counts millionths of the sample): `tick:db` pairs, comma
/// separated.
pub fn gain_points_to_text(points: &[GainPoint]) -> String {
    points
        .iter()
        .map(|p| format!("{}:{}", p.tick.0, p.db))
        .collect::<Vec<_>>()
        .join(",")
}

/// The points `text` holds, sorted by tick and with the levels kept in
/// range; what can't be read is left out.
pub fn gain_points_from_text(text: &str) -> Vec<GainPoint> {
    let mut points: Vec<GainPoint> = text
        .split(',')
        .filter_map(|pair| {
            let (tick, db) = pair.trim().split_once(':')?;
            let db: f32 = db.parse().ok()?;
            let tick: i64 = tick.parse().ok()?;
            db.is_finite().then(|| GainPoint {
                tick: Tick(tick),
                db: db.clamp(GAIN_POINT_MIN_DB, GAIN_POINT_MAX_DB),
            })
        })
        .collect();
    points.sort_by_key(|p| p.tick);
    points
}

/// The lowest and highest a gain point can be set to, in dB.
pub const GAIN_POINT_MIN_DB: f32 = -60.0;
pub const GAIN_POINT_MAX_DB: f32 = 24.0;

/// How far apart, in seconds, the end of one audio clip and the start of the
/// next can be and still count as butted together, so a crossfade can sit
/// between them.
pub const BUTT_SECONDS: f64 = 0.002;

/// A crossfade between two audio clips butted together, stored on the later
/// one, whose start is the cut. It lasts `before` ticks before the cut and
/// `after` ticks after it: over that time the earlier clip fades out, playing
/// on past its end, while this one fades in, playing from before its start
/// (equal power). Each side is held to the audio the clip has left in that
/// direction. In timeline time, so warped clips need nothing special.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Crossfade {
    pub before: Tick,
    pub after: Tick,
}

impl Crossfade {
    /// How long the whole crossfade lasts.
    pub fn len(&self) -> Tick {
        Tick(self.before.0 + self.after.0)
    }

    pub fn is_empty(&self) -> bool {
        self.before.0 + self.after.0 <= 0
    }
}

impl AudioClip {
    /// `length` frames of `source` from its start, at unit gain and without
    /// fades.
    pub fn new(source: impl Into<String>, length: u64) -> Self {
        Self {
            source: source.into(),
            offset: 0,
            length,
            gain: 1.0,
            fade_in: 0,
            fade_out: 0,
            gain_points: Vec::new(),
            crossfade: None,
            warp: None,
            extent: None,
            muted: false,
            takes: None,
        }
    }
}

impl Clip {
    /// An audio clip of `length` frames of `source`, from its start.
    pub fn audio(node: NodeId, start: Tick, source: impl Into<String>, length: u64) -> Self {
        Self {
            node,
            start,
            content: ClipContent::Audio(AudioClip::new(source, length)),
        }
    }

    /// An empty MIDI clip of `length` ticks.
    pub fn midi(node: NodeId, start: Tick, length: Tick) -> Self {
        Self {
            node,
            start,
            content: ClipContent::Midi(MidiClip::new(length)),
        }
    }

    /// Makes the ids of a MIDI clip's notes unique; audio clips are left be.
    pub(crate) fn assign_note_ids(&mut self) {
        if let ClipContent::Midi(midi) = &mut self.content {
            midi.assign_note_ids();
        }
    }

    /// The audio, if this is an audio clip.
    pub fn as_audio(&self) -> Option<&AudioClip> {
        match &self.content {
            ClipContent::Audio(audio) => Some(audio),
            ClipContent::Midi(_) => None,
        }
    }

    /// The notes, if this is a MIDI clip.
    pub fn as_midi(&self) -> Option<&MidiClip> {
        match &self.content {
            ClipContent::Midi(midi) => Some(midi),
            ClipContent::Audio(_) => None,
        }
    }

    /// Why this clip can't be in a project, if it can't.
    pub(crate) fn problem(&self) -> Option<&'static str> {
        if self.start < Tick::ZERO {
            return Some("it starts before the beginning");
        }
        match &self.content {
            ClipContent::Audio(audio) => audio.problem(),
            ClipContent::Midi(midi) => midi.problem(),
        }
    }
}

impl AudioClip {
    /// The gain envelope's level in dB at `tick` from the clip's start, 0
    /// for a clip without one.
    pub fn envelope_db(&self, tick: Tick) -> f32 {
        let points = &self.gain_points;
        let after = points.partition_point(|p| p.tick <= tick);
        match (
            after.checked_sub(1).map(|i| points[i]),
            points.get(after).copied(),
        ) {
            (None, None) => 0.0,
            (Some(p), None) | (None, Some(p)) => p.db,
            (Some(a), Some(b)) => {
                let t = (tick.0 - a.tick.0) as f32 / (b.tick.0 - a.tick.0).max(1) as f32;
                a.db + (b.db - a.db) * t
            }
        }
    }

    /// Moves the envelope later by `delta` ticks (the clip's start moved
    /// earlier by that much), or earlier for a negative `delta`. Points that
    /// fall before the start go, leaving a point at the start with the level
    /// the envelope had there.
    pub fn shift_gain_points(&mut self, delta: Tick) {
        if self.gain_points.is_empty() || delta == Tick::ZERO {
            return;
        }
        let at_start = self.envelope_db(Tick(-delta.0));
        self.gain_points.retain_mut(|p| {
            p.tick = Tick(p.tick.0 + delta.0);
            p.tick > Tick::ZERO
        });
        if delta.0 < 0 {
            self.gain_points.insert(
                0,
                GainPoint {
                    tick: Tick::ZERO,
                    db: at_start,
                },
            );
        }
    }

    fn problem(&self) -> Option<&'static str> {
        if self.length == 0 {
            Some("it is empty")
        } else if self.source.is_empty() {
            Some("it has no audio file")
        } else if !self.gain.is_finite() || self.gain < 0.0 {
            Some("its gain is not a number or is negative")
        } else if self.fade_in.saturating_add(self.fade_out) > self.length {
            Some("its fades are longer than the clip")
        } else if self.extent.is_some_and(|e| e <= Tick::ZERO) {
            Some("its extent is empty")
        } else if self
            .gain_points
            .iter()
            .any(|p| !p.db.is_finite() || p.tick < Tick::ZERO)
            || self.gain_points.windows(2).any(|w| w[1].tick < w[0].tick)
        {
            Some("its gain points are out of order or not numbers")
        } else if self
            .crossfade
            .is_some_and(|x| x.before < Tick::ZERO || x.after < Tick::ZERO)
        {
            Some("its crossfade is negative")
        } else {
            self.takes.as_ref().and_then(TakeFolder::problem)
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn gain_points_round_trip_through_text_sorted_and_in_range() {
        let points = vec![
            GainPoint {
                tick: Tick(100),
                db: -6.5,
            },
            GainPoint {
                tick: Tick(900_000),
                db: 3.0,
            },
        ];
        assert_eq!(gain_points_from_text(&gain_points_to_text(&points)), points);
        let messy = gain_points_from_text("500:-99,junk,100:2,7:nan");
        assert_eq!(messy.len(), 2);
        assert_eq!(messy[0].tick, Tick(100));
        assert_eq!(messy[1].db, GAIN_POINT_MIN_DB);
        assert!(gain_points_from_text("").is_empty());
    }

    use super::*;

    fn clip_with(points: &[(i64, f32)]) -> AudioClip {
        let mut audio = AudioClip::new("a.wav", 1000);
        audio.gain_points = points
            .iter()
            .map(|&(tick, db)| GainPoint {
                tick: Tick(tick),
                db,
            })
            .collect();
        audio
    }

    #[test]
    fn the_envelope_runs_straight_in_db_and_holds_at_its_ends() {
        let audio = clip_with(&[(100, -6.0), (300, -26.0)]);
        assert_eq!(audio.envelope_db(Tick(0)), -6.0);
        assert_eq!(audio.envelope_db(Tick(100)), -6.0);
        assert_eq!(audio.envelope_db(Tick(200)), -16.0);
        assert_eq!(audio.envelope_db(Tick(900)), -26.0);
        // No points: flat.
        assert_eq!(clip_with(&[]).envelope_db(Tick(50)), 0.0);
    }

    #[test]
    fn moving_the_start_shifts_the_points_and_keeps_the_level_at_the_new_start() {
        let mut later = clip_with(&[(0, 0.0), (400, -20.0)]);
        // The start moves 100 ticks later: the envelope is 100 ticks earlier.
        later.shift_gain_points(Tick(-100));
        assert_eq!(later.envelope_db(Tick(0)), -5.0);
        assert_eq!(
            later
                .gain_points
                .iter()
                .map(|p| p.tick.0)
                .collect::<Vec<_>>(),
            [0, 300]
        );
        // The start moves 50 ticks earlier: everything is 50 ticks later.
        let mut earlier = clip_with(&[(0, 0.0), (400, -20.0)]);
        earlier.shift_gain_points(Tick(50));
        assert_eq!(
            earlier
                .gain_points
                .iter()
                .map(|p| p.tick.0)
                .collect::<Vec<_>>(),
            [50, 450]
        );
        assert_eq!(earlier.envelope_db(Tick(0)), 0.0);
    }

    #[test]
    fn a_clip_with_unordered_points_or_a_negative_crossfade_is_refused() {
        assert!(clip_with(&[(0, 0.0), (10, 3.0)]).problem().is_none());
        assert!(clip_with(&[(10, 0.0), (0, 3.0)]).problem().is_some());
        assert!(clip_with(&[(0, f32::NAN)]).problem().is_some());
        let mut audio = clip_with(&[]);
        audio.crossfade = Some(Crossfade {
            before: Tick(-1),
            after: Tick(5),
        });
        assert!(audio.problem().is_some());
    }

    #[test]
    fn points_and_crossfades_survive_a_round_trip() {
        let mut audio = clip_with(&[(0, 1.5)]);
        audio.crossfade = Some(Crossfade {
            before: Tick(20),
            after: Tick(30),
        });
        let text = ron::to_string(&audio).unwrap();
        assert_eq!(ron::from_str::<AudioClip>(&text).unwrap(), audio);
        // A flat clip writes neither.
        let flat = ron::to_string(&clip_with(&[])).unwrap();
        assert!(
            !flat.contains("gain_points") && !flat.contains("crossfade"),
            "{flat}"
        );
    }

    fn folder(takes: usize) -> TakeFolder {
        TakeFolder::new(
            (0..takes as u64)
                .map(|k| Take { delta: k * 1000 })
                .collect(),
            0,
        )
    }

    fn comp(folder: &TakeFolder) -> Vec<(u64, usize)> {
        folder.comp.iter().map(|s| (s.at, s.take)).collect()
    }

    #[test]
    fn swiping_a_range_cuts_the_spans_it_crosses_and_merges_equal_neighbours() {
        let mut f = folder(3);
        f.swipe(100, 200, 1);
        assert_eq!(comp(&f), [(0, 0), (100, 1), (200, 0)]);
        // Across the join: the later span is shortened, the earlier one cut.
        f.swipe(150, 400, 2);
        assert_eq!(comp(&f), [(0, 0), (100, 1), (150, 2), (400, 0)]);
        // Covering a whole span removes it, and equal neighbours merge.
        f.swipe(90, 410, 2);
        assert_eq!(comp(&f), [(0, 0), (90, 2), (410, 0)]);
        f.swipe(0, 90, 2);
        assert_eq!(comp(&f), [(0, 2), (410, 0)]);
        assert_eq!(f.used(), [2, 0]);
        assert_eq!(f.span_at(409), 0);
        assert_eq!(f.span_at(410), 1);
        // Nothing to do for an empty range or a take that isn't there.
        let before = f.clone();
        f.swipe(5, 5, 1);
        f.swipe(5, 9, 7);
        assert_eq!(f, before);
    }

    #[test]
    fn a_swipe_keeps_the_crossfades_of_the_joins_it_leaves_alone() {
        let mut f = folder(2);
        f.swipe(100, 200, 1);
        let x = Crossfade {
            before: Tick(3),
            after: Tick(4),
        };
        f.set_join(2, Some(x));
        f.swipe(300, 400, 1);
        assert_eq!(f.comp[2].crossfade, Some(x));
        assert_eq!(f.comp[1].crossfade, None);
        // Joins are never on the first span.
        f.set_join(0, Some(x));
        assert_eq!(f.comp[0].crossfade, None);
    }

    #[test]
    fn a_broken_folder_is_a_problem() {
        let mut audio = AudioClip::new("a.wav", 1000);
        audio.takes = Some(folder(2));
        assert!(audio.problem().is_none());
        audio.takes.as_mut().unwrap().comp[0].take = 5;
        assert!(audio.problem().is_some());
        audio.takes = Some(TakeFolder {
            takes: vec![Take { delta: 0 }],
            comp: vec![],
        });
        assert!(audio.problem().is_some());
        let mut f = folder(2);
        f.swipe(10, 20, 1);
        f.comp[2].at = 5;
        assert!(f.problem().is_some());
    }

    #[test]
    fn a_take_folder_survives_a_round_trip_and_a_plain_clip_writes_none() {
        let mut audio = AudioClip::new("a.wav", 1000);
        let mut f = folder(3);
        f.swipe(10, 20, 2);
        audio.takes = Some(f);
        let text = ron::to_string(&audio).unwrap();
        assert_eq!(ron::from_str::<AudioClip>(&text).unwrap(), audio);
        assert!(!ron::to_string(&clip_with(&[])).unwrap().contains("takes"));
    }

    fn looping(length: i64, from: i64, len: i64, notes: &[(i64, i64, u8)]) -> MidiClip {
        let mut clip = MidiClip::new(Tick(length));
        clip.notes = notes
            .iter()
            .map(|&(s, l, k)| MidiNote::new(Tick(s), Tick(l), k))
            .collect();
        clip.loop_on = true;
        clip.loop_start = Tick(from);
        clip.loop_len = Tick(len);
        clip
    }

    fn shape(notes: Vec<MidiNote>) -> Vec<(i64, i64, u8)> {
        let mut out: Vec<_> = notes
            .iter()
            .map(|n| (n.start.0, n.length.0, n.key))
            .collect();
        out.sort();
        out
    }

    #[test]
    fn a_clip_that_does_not_loop_plays_its_notes_once() {
        let mut clip = looping(3840, 0, 960, &[(0, 480, 60), (4000, 10, 61)]);
        clip.loop_on = false;
        // A note past the clip's end is left out.
        assert_eq!(shape(clip.played()), [(0, 480, 60)]);
    }

    #[test]
    fn a_loop_repeats_its_section_until_the_clips_end() {
        // Loop beats 1..2 of a four beat clip. The first note is a lead-in
        // that plays once; the one after the loop never plays.
        let clip = looping(
            3840,
            960,
            960,
            &[(0, 480, 59), (960, 480, 60), (2000, 100, 61)],
        );
        assert_eq!(
            shape(clip.played()),
            [
                (0, 480, 59),
                (960, 480, 60),
                (1920, 480, 60),
                (2880, 480, 60)
            ]
        );
    }

    #[test]
    fn a_note_is_cut_at_the_loops_end_on_every_pass() {
        let clip = looping(2880, 0, 960, &[(480, 960, 60)]);
        assert_eq!(
            shape(clip.played()),
            [(480, 480, 60), (1440, 480, 60), (2400, 480, 60)]
        );
    }

    #[test]
    fn the_last_repeat_is_not_started_past_the_clips_end() {
        let clip = looping(1500, 0, 960, &[(0, 100, 60), (800, 100, 62)]);
        assert_eq!(
            shape(clip.played()),
            [(0, 100, 60), (800, 100, 62), (960, 100, 60)]
        );
    }

    #[test]
    fn turning_looping_on_loops_the_whole_clip() {
        let mut clip = MidiClip::new(Tick(3840));
        clip.set_looping(true);
        assert!(clip.loop_on);
        assert_eq!((clip.loop_start, clip.loop_len), (Tick(0), Tick(3840)));
        clip.set_loop(Tick(960), Tick(1920));
        clip.set_looping(false);
        clip.set_looping(true);
        assert_eq!((clip.loop_start, clip.loop_len), (Tick(960), Tick(960)));
    }

    #[test]
    fn set_loop_keeps_the_section_in_the_clip_and_not_tiny() {
        let mut clip = MidiClip::new(Tick(3840));
        clip.set_loop(Tick(-50), Tick(100000));
        assert_eq!((clip.loop_start, clip.loop_len), (Tick(0), Tick(3840)));
        clip.set_loop(Tick(960), Tick(960));
        assert_eq!(clip.loop_len, Tick(MIN_LOOP_LEN));
    }

    #[test]
    fn a_loop_that_is_too_short_is_a_problem() {
        let mut clip = looping(3840, 0, 960, &[]);
        assert_eq!(MidiClip::problem(&clip), None);
        clip.loop_len = Tick(MIN_LOOP_LEN - 1);
        assert!(clip.problem().is_some());
        // Not looping, the loop's numbers don't matter.
        clip.loop_on = false;
        assert_eq!(clip.problem(), None);
    }

    #[test]
    fn the_loop_survives_a_round_trip_and_an_old_file_has_none() {
        let clip = looping(3840, 960, 1920, &[(0, 10, 60)]);
        let text = ron::to_string(&clip).unwrap();
        assert_eq!(ron::from_str::<MidiClip>(&text).unwrap(), clip);
        let old = ron::from_str::<MidiClip>("(length: 960, notes: [])").unwrap();
        assert!(!old.loop_on);
    }
}
