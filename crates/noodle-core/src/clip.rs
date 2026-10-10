//! Clips: audio (and from M3, MIDI) placed on the timeline for a track input node to play.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::{NodeId, Tick};

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
        let mut next = self.notes.iter().map(|n| n.id).max().map_or(0, |m| m + 1);
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
        } else {
            None
        }
    }
}
