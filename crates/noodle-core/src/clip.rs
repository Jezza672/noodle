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
    /// Controllers, pitch bend, pressure and program changes, in no
    /// particular order. They leave through the `midi` output with the
    /// notes, and move, loop, split and trim with them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub controls: Vec<ControlPoint>,
}

fn is_zero(tick: &Tick) -> bool {
    *tick == Tick::ZERO
}

/// The shortest loop a MIDI clip may have: a sixteenth of a beat.
pub const MIN_LOOP_LEN: i64 = TICKS_PER_QUARTER / 16;

/// What a [`ControlPoint`] sets.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ControlKind {
    /// A controller by number, 0 to 127. Its value is 0 to 1.
    Controller(u8),
    /// Pitch bend, -1 to 1 with 0 at the centre.
    PitchBend,
    /// Channel pressure (aftertouch), 0 to 1.
    ChannelPressure,
    /// A program change; the value is the program, 0 to 127, over 127.
    ProgramChange,
}

impl ControlKind {
    /// The value that lets go of what the kind sets: pedals up, the wheel
    /// down, the bend centred, and the channel's mixing controllers (volume,
    /// pan, expression) where a synth starts. A program change is not
    /// released.
    pub fn rest(self) -> Option<f32> {
        match self {
            ControlKind::Controller(7) => Some(100.0 / 127.0),
            ControlKind::Controller(10) => Some(64.0 / 127.0),
            ControlKind::Controller(11) => Some(1.0),
            ControlKind::Controller(_) | ControlKind::PitchBend | ControlKind::ChannelPressure => {
                Some(0.0)
            }
            ControlKind::ProgramChange => None,
        }
    }
}

/// One value of a controller in a MIDI clip. Values are normalised, as the
/// engine's events are, and converted only at the device.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ControlPoint {
    /// From the start of the clip.
    pub tick: Tick,
    /// 0 to 15.
    pub channel: u8,
    pub kind: ControlKind,
    pub value: f32,
    /// Whether the value glides in a straight line to the next point of the
    /// same channel and kind (played as events at a fixed rate), instead of
    /// holding until it.
    #[serde(default, skip_serializing_if = "is_false")]
    pub ramp: bool,
}

impl ControlPoint {
    pub fn new(tick: Tick, channel: u8, kind: ControlKind, value: f32) -> Self {
        Self {
            tick,
            channel,
            kind,
            value,
            ramp: false,
        }
    }
}

/// What a [`NoteExpression`] sets, on one note.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ExpressionKind {
    /// In semitones.
    Pitch,
    /// Poly pressure, 0 to 1.
    Pressure,
    Timbre,
    Volume,
    Pan,
}

/// One value of a per-note expression, counted from the note's start.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct NoteExpression {
    pub tick: Tick,
    pub kind: ExpressionKind,
    pub value: f32,
    /// As [`ControlPoint::ramp`].
    #[serde(default, skip_serializing_if = "is_false")]
    pub ramp: bool,
}

/// The value at `tick` of the points `of` (one channel and kind, sorted by
/// tick): the last at or before it, or on the way to the next if it ramps.
/// `None` before the first.
fn value_along(of: impl Iterator<Item = (Tick, f32, bool)>, tick: Tick) -> Option<f32> {
    let mut before: Option<(Tick, f32, bool)> = None;
    for point in of {
        if point.0 <= tick {
            before = Some(point);
        } else {
            return Some(match before {
                Some((at, value, true)) => {
                    let along = (tick.0 - at.0) as f32 / (point.0.0 - at.0) as f32;
                    value + (point.1 - value) * along
                }
                Some((_, value, false)) => value,
                None => return None,
            });
        }
    }
    before.map(|p| p.1)
}

/// The value of a controller of `points` at `tick`; `None` before its first
/// point.
pub fn control_value_at(
    points: &[ControlPoint],
    channel: u8,
    kind: ControlKind,
    tick: Tick,
) -> Option<f32> {
    let mut of: Vec<&ControlPoint> = points
        .iter()
        .filter(|p| p.channel == channel && p.kind == kind)
        .collect();
    of.sort_by_key(|p| p.tick);
    value_along(of.into_iter().map(|p| (p.tick, p.value, p.ramp)), tick)
}

/// The controllers `points` as they are from `delta` ticks in, for a clip
/// cut there (or, if `delta` is negative, made longer at the front): the
/// later points moved earlier by `delta`, and, for each controller that had a
/// value by then, that value at the start, so a held pedal stays held.
pub fn controls_from(points: &[ControlPoint], delta: i64) -> Vec<ControlPoint> {
    let mut out: Vec<ControlPoint> = Vec::new();
    let mut sorted = points.to_vec();
    sorted.sort_by_key(|p| p.tick);
    let mut lanes: Vec<(u8, ControlKind)> = sorted.iter().map(|p| (p.channel, p.kind)).collect();
    lanes.sort();
    lanes.dedup();
    for (channel, kind) in lanes {
        let lane = || {
            sorted
                .iter()
                .filter(|p| p.channel == channel && p.kind == kind)
        };
        if delta > 0 && !lane().any(|p| p.tick.0 == delta) {
            let value = value_along(lane().map(|p| (p.tick, p.value, p.ramp)), Tick(delta));
            let ramp = lane().rfind(|p| p.tick.0 < delta).is_some_and(|p| p.ramp)
                && lane().any(|p| p.tick.0 > delta);
            if let Some(value) = value {
                out.push(ControlPoint {
                    tick: Tick::ZERO,
                    channel,
                    kind,
                    value,
                    ramp,
                });
            }
        }
        out.extend(lane().filter(|p| p.tick.0 >= delta).map(|p| ControlPoint {
            tick: Tick(p.tick.0 - delta),
            ..*p
        }));
    }
    out.sort_by_key(|p| p.tick);
    out
}

/// One note in a MIDI clip.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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
    /// Per-note modulation (poly pressure, MPE), from the note's start. It
    /// plays while the note sounds.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub expressions: Vec<NoteExpression>,
}

impl MidiNote {
    pub fn new(start: Tick, length: Tick, key: u8) -> Self {
        Self {
            id: 0,
            start,
            length,
            key,
            velocity: 0.8,
            expressions: Vec::new(),
        }
    }

    /// The expressions of the note as they are from `delta` ticks in on: the
    /// later ones moved earlier by `delta`, and, for each kind that had a
    /// value by then, that value at the start. For a note cut in two.
    pub fn expressions_from(&self, delta: i64) -> Vec<NoteExpression> {
        if delta <= 0 {
            return self.expressions.clone();
        }
        let mut sorted = self.expressions.clone();
        sorted.sort_by_key(|e| e.tick);
        let mut kinds: Vec<ExpressionKind> = sorted.iter().map(|e| e.kind).collect();
        kinds.sort();
        kinds.dedup();
        let mut out = Vec::new();
        for kind in kinds {
            let of = || {
                sorted
                    .iter()
                    .filter(move |e| e.kind == kind)
                    .map(|e| (e.tick, e.value, e.ramp))
            };
            let ramps = of()
                .rfind(|p| p.0.0 <= delta)
                .is_some_and(|p| p.2 && of().any(|q| q.0.0 > delta));
            if let Some(value) = value_along(of(), Tick(delta)) {
                out.push(NoteExpression {
                    tick: Tick::ZERO,
                    kind,
                    value,
                    ramp: ramps,
                });
            }
            out.extend(of().filter(|p| p.0.0 > delta).map(|p| NoteExpression {
                tick: Tick(p.0.0 - delta),
                kind,
                value: p.1,
                ramp: p.2,
            }));
        }
        out
    }

    /// The expressions with those past the note's end dropped.
    pub fn expressions_until(&self, end: i64) -> Vec<NoteExpression> {
        self.expressions
            .iter()
            .filter(|e| e.tick.0 < end)
            .copied()
            .collect()
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
            controls: Vec::new(),
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
                    ..(*note).clone()
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
                    ..note.clone()
                });
            } else {
                out.push(note.clone());
            }
        }
        out.extend(self.repeats());
        out
    }

    /// The controllers as they sound, in clip time and sorted by tick, the
    /// way [`played`](Self::played) gives the notes: points past the clip's
    /// end (or, if it loops, the loop's) are dropped, the loop section's
    /// points repeat, and a ramp that is cut off ends on the value it had
    /// there. At each repeat, every controller of the clip is put back to
    /// the value it had where the loop section starts (or released if it had
    /// none yet), so what an earlier pass left behind does not carry over.
    pub fn played_controls(&self) -> Vec<ControlPoint> {
        let mut groups: std::collections::BTreeMap<(u8, ControlKind), Vec<ControlPoint>> =
            Default::default();
        for point in &self.controls {
            // Points past the end stay, for a ramp to head towards.
            if point.tick.0 >= 0 {
                groups
                    .entry((point.channel, point.kind))
                    .or_default()
                    .push(*point);
            }
        }
        let looping = self.loop_on && self.loop_len.0 >= MIN_LOOP_LEN;
        let (from, end) = (self.loop_start.0, self.loop_end().0);
        let limit = self.length.0;
        let mut out = Vec::new();
        for ((_, kind), group) in groups.iter_mut() {
            group.sort_by_key(|p| p.tick);
            let group = &*group;
            if group[0].tick.0 >= limit {
                continue;
            }
            let value_at = |tick: i64| {
                value_along(group.iter().map(|p| (p.tick, p.value, p.ramp)), Tick(tick))
            };
            let pass = |lo: i64, hi: i64, shift: i64, out: &mut Vec<ControlPoint>| {
                let mark = out.len();
                for p in group.iter().filter(|p| p.tick.0 >= lo && p.tick.0 < hi) {
                    if p.tick.0 + shift < limit {
                        out.push(ControlPoint {
                            tick: Tick(p.tick.0 + shift),
                            ..*p
                        });
                    }
                }
                let cut = (hi + shift).min(limit);
                if out.len() > mark
                    && out.last().is_some_and(|p| p.ramp)
                    && let Some(value) = value_at(cut - shift)
                {
                    let last = *out.last().expect("checked above");
                    out.push(ControlPoint {
                        tick: Tick(cut),
                        value,
                        ramp: false,
                        ..last
                    });
                }
            };
            let channel = group[0].channel;
            if looping {
                pass(0, end.min(limit), 0, &mut out);
                let mut shift = self.loop_len.0;
                while from + shift < limit {
                    let set_there = group.iter().any(|p| p.tick.0 == from);
                    if !set_there && let Some(value) = value_at(from).or(kind.rest()) {
                        out.push(ControlPoint::new(Tick(from + shift), channel, *kind, value));
                    }
                    pass(from, end, shift, &mut out);
                    shift += self.loop_len.0;
                }
            } else {
                pass(0, limit, 0, &mut out);
            }
        }
        out.sort_by_key(|p| p.tick);
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
        for point in &self.controls {
            if point.tick < Tick::ZERO {
                return Some("a controller point starts before the clip");
            }
            if point.channel > 15 {
                return Some("a controller point's channel is out of range");
            }
            let range = match point.kind {
                ControlKind::Controller(number) if number > 127 => {
                    return Some("a controller number is out of range");
                }
                ControlKind::PitchBend => -1.0..=1.0,
                _ => 0.0..=1.0,
            };
            if !range.contains(&point.value) {
                return Some("a controller value is out of range");
            }
        }
        for note in &self.notes {
            if note
                .expressions
                .iter()
                .any(|e| e.tick < Tick::ZERO || !e.value.is_finite())
            {
                return Some("a note's expression starts before the note or is not a number");
            }
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
    fn cc(tick: i64, kind: ControlKind, value: f32, ramp: bool) -> ControlPoint {
        ControlPoint {
            ramp,
            ..ControlPoint::new(Tick(tick), 0, kind, value)
        }
    }

    const PEDAL: ControlKind = ControlKind::Controller(64);

    #[test]
    fn controls_and_expressions_survive_a_round_trip_and_an_old_file_has_none() {
        let mut clip = looping(3840, 0, 960, &[(0, 480, 60)]);
        clip.controls = vec![
            cc(0, PEDAL, 1.0, false),
            cc(10, ControlKind::PitchBend, -0.5, true),
        ];
        clip.notes[0].expressions = vec![NoteExpression {
            tick: Tick(5),
            kind: ExpressionKind::Pressure,
            value: 0.5,
            ramp: true,
        }];
        let text = ron::to_string(&clip).unwrap();
        assert_eq!(ron::from_str::<MidiClip>(&text).unwrap(), clip);
        let old = ron::from_str::<MidiClip>("(length: 960, notes: [])").unwrap();
        assert!(old.controls.is_empty());
    }

    #[test]
    fn out_of_range_controls_are_problems() {
        let mut clip = MidiClip::new(Tick(960));
        for bad in [
            cc(0, ControlKind::Controller(128), 0.5, false),
            cc(0, PEDAL, 1.5, false),
            cc(0, ControlKind::PitchBend, -1.5, false),
            cc(-1, PEDAL, 0.5, false),
            ControlPoint {
                channel: 16,
                ..cc(0, PEDAL, 0.5, false)
            },
        ] {
            clip.controls = vec![bad];
            assert!(clip.problem().is_some(), "{bad:?}");
        }
        clip.controls = vec![cc(0, ControlKind::PitchBend, -1.0, false)];
        assert_eq!(clip.problem(), None);
    }

    #[test]
    fn played_controls_drop_what_is_past_the_end_and_cut_ramps_there() {
        let mut clip = MidiClip::new(Tick(960));
        clip.controls = vec![
            cc(0, ControlKind::Controller(1), 0.0, true),
            cc(1920, ControlKind::Controller(1), 1.0, false),
            cc(2000, PEDAL, 1.0, false),
        ];
        let played = clip.played_controls();
        // The ramp is cut at the clip's end, half way up; the pedal is gone.
        assert_eq!(played.len(), 2);
        assert_eq!(
            (played[1].tick, played[1].value, played[1].ramp),
            (Tick(960), 0.5, false)
        );
    }

    #[test]
    fn a_looping_clip_repeats_its_controls_and_puts_each_pass_back() {
        let mut clip = looping(3840, 960, 960, &[]);
        clip.controls = vec![
            cc(0, ControlKind::Controller(1), 0.25, false),
            cc(1200, PEDAL, 1.0, false),
            // After the loop's end: never played.
            cc(2000, PEDAL, 0.0, false),
        ];
        let played = clip.played_controls();
        let pedal: Vec<(i64, f32)> = played
            .iter()
            .filter(|p| p.kind == PEDAL)
            .map(|p| (p.tick.0, p.value))
            .collect();
        // The loop is the second beat; the first pass plays everything up to
        // its end, and each repeat starts by releasing the pedal (it had no
        // value where the loop starts) before pressing it again.
        assert_eq!(
            pedal,
            [
                (1200, 1.0),
                (1920, 0.0),
                (2160, 1.0),
                (2880, 0.0),
                (3120, 1.0)
            ]
        );
        // The wheel, set before the loop, is put back at each repeat.
        let wheel: Vec<(i64, f32)> = played
            .iter()
            .filter(|p| p.kind == ControlKind::Controller(1))
            .map(|p| (p.tick.0, p.value))
            .collect();
        assert_eq!(wheel, [(0, 0.25), (1920, 0.25), (2880, 0.25)]);
    }

    #[test]
    fn cutting_the_front_off_keeps_a_held_pedal_held_and_ramps_going() {
        let points = vec![
            cc(0, PEDAL, 1.0, false),
            cc(1000, PEDAL, 0.0, false),
            cc(0, ControlKind::Controller(1), 0.0, true),
            cc(800, ControlKind::Controller(1), 1.0, false),
        ];
        let from = controls_from(&points, 400);
        let of = |kind| -> Vec<(i64, f32, bool)> {
            from.iter()
                .filter(|p| p.kind == kind)
                .map(|p| (p.tick.0, p.value, p.ramp))
                .collect()
        };
        assert_eq!(of(PEDAL), [(0, 1.0, false), (600, 0.0, false)]);
        assert_eq!(
            of(ControlKind::Controller(1)),
            [(0, 0.5, true), (400, 1.0, false)]
        );
        // Made longer at the front: everything moves later.
        assert_eq!(controls_from(&points, -100)[0].tick, Tick(100));
    }

    #[test]
    fn a_note_cut_in_two_keeps_its_expressions_going() {
        let mut note = MidiNote::new(Tick(0), Tick(1000), 60);
        let at = |tick, value, ramp| NoteExpression {
            tick: Tick(tick),
            kind: ExpressionKind::Pressure,
            value,
            ramp,
        };
        note.expressions = vec![at(0, 0.0, true), at(800, 1.0, false)];
        let later = note.expressions_from(400);
        assert_eq!(later, [at(0, 0.5, true), at(400, 1.0, false)]);
        assert_eq!(note.expressions_until(500), [at(0, 0.0, true)]);
    }
}
