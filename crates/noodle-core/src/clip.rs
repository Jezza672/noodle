//! Clips: audio (and from M3, MIDI) placed on the timeline for a track input node to play.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::{NodeId, Tick, WarpId};

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
}

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
        }
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
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// One control point of a clip's gain envelope.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct GainPoint {
    /// Where, in ticks from the start of the clip.
    pub tick: Tick,
    pub db: f32,
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
            None
        }
    }
}

#[cfg(test)]
mod tests {
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
}
