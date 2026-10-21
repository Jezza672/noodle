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
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
