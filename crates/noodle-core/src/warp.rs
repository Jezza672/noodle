//! Time warping an audio clip: the clip's tempo, its warp markers and the
//! curve they make together. See CONTEXT.md (warp, clip tempo, beat marker,
//! warp marker, tempo conversion) and ADR 0002.
//!
//! Everything here is plain arithmetic on the document; the engine turns the
//! [`WarpCurve`] into sample positions (`noodle-nodes`, `clip/warp.rs`) and the
//! app uses it to lay clips out.
//!
//! Times in the clip ("source seconds") count from the start of the file.
//! Beats are quarter notes.

use std::borrow::Cow;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::{TempoMap, Tick};

/// Identifies a [`ClipWarp`] within a project. Sections that show the same
/// clip name the same warp, so editing it changes them all; a section that is
/// modified on its own is given a copy first (see
/// [`Project::warp_edit`](crate::Project::warp_edit)).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WarpId(pub u64);

impl fmt::Display for WarpId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "warp #{}", self.0)
    }
}

/// How a warp is rendered.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum WarpMode {
    /// Plain resampling, so pitch follows speed. Cheap, and the mode used
    /// while a warp marker is dragged.
    #[default]
    Repitch,
    /// Waveform-similarity overlap-add: the pitch holds while the speed
    /// changes. Best for tonal and sustained material.
    Grain,
    /// Like Grain, but each transient is kept whole: the audio is cut at the
    /// clip's transients, each attack plays at natural speed, and only what
    /// follows is stretched. Best for drums and plucked sounds.
    Beats,
}

impl WarpMode {
    /// Whether the mode keeps the pitch, which the engine renders ahead of
    /// the playhead (Repitch is done in the audio thread).
    pub fn keeps_pitch(self) -> bool {
        !matches!(self, WarpMode::Repitch)
    }
}

/// The slowest and fastest a clip may play, as a multiple of natural speed,
/// between two warp markers. Beyond these the sound is noise. The speed the
/// engine plays at also includes tempo conversion (timeline bpm over clip
/// bpm); [`ENGINE_MAX_SPEED`] bounds that.
pub const MIN_WARP_SPEED: f64 = 1.0 / 16.0;
pub const MAX_WARP_SPEED: f64 = 16.0;

/// The fastest the engine reads a clip's file, as a multiple of natural
/// speed, whatever the markers and the tempos make of it. Faster than this,
/// the disk stream can't keep up, so the engine slows a section's fast parts
/// down to it.
pub const ENGINE_MAX_SPEED: f64 = 32.0;

/// The tempos a clip tempo may have, in beats per minute.
pub const MIN_CLIP_BPM: f64 = 1.0;
pub const MAX_CLIP_BPM: f64 = 1000.0;

/// Where the clip tempo changes. The tempo holds until the next point.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct TempoPoint {
    /// Source seconds.
    pub seconds: f64,
    pub bpm: f64,
}

/// The clip tempo: the tempo the unwarped clip naturally has, which may
/// change along the clip. It decides where the clip's beat markers fall.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TempoCurve {
    /// Sorted by time; the first is at 0 and its tempo holds before that too.
    points: Vec<TempoPoint>,
}

impl TempoCurve {
    /// One tempo all along the clip.
    pub fn constant(bpm: f64) -> Self {
        Self {
            points: vec![TempoPoint { seconds: 0.0, bpm }],
        }
    }

    /// A curve from tempo changes. The first point is moved to time 0.
    pub fn new(mut points: Vec<TempoPoint>) -> Result<Self, &'static str> {
        points.sort_by(|a, b| a.seconds.total_cmp(&b.seconds));
        let Some(first) = points.first_mut() else {
            return Err("the clip tempo has no points");
        };
        first.seconds = 0.0;
        let curve = Self { points };
        match curve.problem() {
            Some(problem) => Err(problem),
            None => Ok(curve),
        }
    }

    /// The tempo `map` has over `seconds` from `start`: what a clip recorded
    /// there was played at, tempo changes included. The clip's own seconds
    /// begin `lead` seconds before `start`, so a clip whose first moments are
    /// not used (a take trimmed for latency) lines up; the first tempo holds
    /// back to the clip's start.
    pub fn from_tempo_map(map: &TempoMap, start: Tick, lead: f64, seconds: f64) -> Self {
        let began = map.seconds_at(start);
        let mut points = vec![TempoPoint {
            seconds: 0.0,
            bpm: map.bpm_at(start),
        }];
        for change in map.tempos().iter().filter(|c| c.tick > start) {
            let at = map.seconds_at(change.tick) - began + lead;
            if at < seconds {
                points.push(TempoPoint {
                    seconds: at,
                    bpm: change.bpm,
                });
            }
        }
        Self { points }
    }

    pub fn points(&self) -> &[TempoPoint] {
        &self.points
    }

    /// The tempo at `seconds`.
    pub fn bpm_at(&self, seconds: f64) -> f64 {
        let i = self
            .points
            .partition_point(|p| p.seconds <= seconds)
            .saturating_sub(1);
        self.points[i].bpm
    }

    /// Beats from the start of the clip to `seconds`. Before the clip, the
    /// first tempo carries on backwards; after the last point, the last tempo
    /// carries on.
    pub fn beats_at(&self, seconds: f64) -> f64 {
        self.as_curve().at(seconds)
    }

    /// The inverse of [`beats_at`](Self::beats_at).
    pub fn seconds_at(&self, beats: f64) -> f64 {
        self.as_curve().inverse(beats)
    }

    /// Beats against seconds, as a curve.
    fn as_curve(&self) -> PiecewiseLinear {
        let mut beats = 0.0;
        let mut knots = Vec::with_capacity(self.points.len());
        for (i, point) in self.points.iter().enumerate() {
            knots.push((point.seconds, beats));
            if let Some(next) = self.points.get(i + 1) {
                beats += (next.seconds - point.seconds) * point.bpm / 60.0;
            }
        }
        let last = self.points.last().expect("never empty").bpm / 60.0;
        let first = self.points[0].bpm / 60.0;
        PiecewiseLinear::new(knots, first, last)
    }

    fn problem(&self) -> Option<&'static str> {
        if self.points.is_empty() {
            return Some("the clip tempo has no points");
        }
        if self.points[0].seconds != 0.0 {
            return Some("the clip tempo doesn't start at the start of the clip");
        }
        for pair in self.points.windows(2) {
            if pair[1].seconds <= pair[0].seconds || !pair[1].seconds.is_finite() {
                return Some("the clip tempo changes are not in order");
            }
        }
        for point in &self.points {
            if !(MIN_CLIP_BPM..=MAX_CLIP_BPM).contains(&point.bpm) {
                return Some("a clip tempo is out of range");
            }
        }
        None
    }
}

impl Default for TempoCurve {
    fn default() -> Self {
        Self::constant(120.0)
    }
}

/// A pin on a clip: this moment of the clip plays on this beat.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct WarpMarker {
    /// The moment in the clip, in source seconds.
    pub seconds: f64,
    /// Where it plays, in beats from where the clip's own beat 0 plays.
    /// Unwarped, a moment at beat `b` of the clip tempo plays on beat `b`.
    pub beat: f64,
}

/// The shortest a loop may be, in beats of the clip tempo. A loop is played as
/// one piece after another, each opened ahead of the playhead, so a loop much
/// shorter than this could not be kept up with.
pub const MIN_LOOP_BEATS: f64 = 1.0;

/// The shortest a loop may be on the timeline, in seconds of playback. A beat
/// is much shorter than that at a high clip tempo, and the hub opens a stream
/// for each repeat about a second ahead (and holds only a few), so a faster
/// loop is not played: the section plays once instead.
pub const MIN_LOOP_SECONDS: f64 = 0.25;

/// The part of a clip that repeats when the clip loops, in beats of the clip
/// tempo from the start of the clip (see [`TempoCurve::beats_at`]), so it
/// stays on the beat grid of the clip whatever the markers do.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct LoopRegion {
    pub start: f64,
    pub end: f64,
}

impl LoopRegion {
    /// How many beats the region lasts.
    pub fn beats(&self) -> f64 {
        self.end - self.start
    }
}

/// The grid a groove is taken on, in beats: sixteenth notes.
pub const GROOVE_SLOT: f64 = 0.25;
/// How many slots a groove lasts before it repeats: one bar of four beats.
pub const GROOVE_SLOTS: usize = 16;

/// A timing feel: how far the beats of a clip sit from the straight grid, slot
/// by slot, over a pattern that repeats. It is the part of a warp one clip can
/// lend another (CONTEXT.md, groove), because it says nothing about the
/// algorithm that plays the clip.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Groove {
    /// The distance between slots, in beats.
    pub slot: f64,
    /// How early (negative) or late each slot sits, in beats, for the slots of
    /// one repeat of the pattern.
    pub offsets: Vec<f64>,
}

impl Groove {
    /// The groove of `beats`: each is put on its nearest slot, and a slot's
    /// offset is the mean of how far the beats on it sit from it. Slots
    /// nothing sits on are straight. `None` if there are no beats, or the
    /// shape is not one.
    pub fn from_beats(
        beats: impl IntoIterator<Item = f64>,
        slot: f64,
        slots: usize,
    ) -> Option<Self> {
        if !(slot > 0.0 && slot.is_finite()) || slots == 0 {
            return None;
        }
        let mut sums = vec![0.0; slots];
        let mut counts = vec![0u32; slots];
        for beat in beats.into_iter().filter(|b| b.is_finite()) {
            let index = (beat / slot).round();
            let i = (index as i64).rem_euclid(slots as i64) as usize;
            sums[i] += beat - index * slot;
            counts[i] += 1;
        }
        if counts.iter().all(|&c| c == 0) {
            return None;
        }
        let offsets = sums
            .iter()
            .zip(&counts)
            .map(|(&sum, &count)| {
                if count == 0 {
                    0.0
                } else {
                    sum / f64::from(count)
                }
            })
            .collect();
        Some(Self { slot, offsets })
    }

    /// Whether every slot is straight.
    pub fn is_flat(&self) -> bool {
        self.offsets.iter().all(|o| o.abs() < 1e-9)
    }

    /// The offset of the slot `beat` is on, or 0 if it is not on a slot.
    pub fn offset_at(&self, beat: f64) -> f64 {
        if self.offsets.is_empty() || self.slot <= 0.0 || self.slot.is_nan() {
            return 0.0;
        }
        let index = (beat / self.slot).round();
        if (beat - index * self.slot).abs() > 1e-6 {
            return 0.0;
        }
        self.offsets[(index as i64).rem_euclid(self.offsets.len() as i64) as usize]
    }

    fn problem(&self) -> Option<&'static str> {
        if !(self.slot > 0.0 && self.slot.is_finite()) || self.offsets.is_empty() {
            return Some("the groove has no slots");
        }
        if self
            .offsets
            .iter()
            .any(|o| !o.is_finite() || o.abs() > self.slot * 4.0)
        {
            return Some("the groove has an offset that is out of range");
        }
        None
    }
}

/// Warp quantise: the warp markers pulled toward a grid, by an amount, without
/// touching where the markers are stored. The stored markers stay as the user
/// put them; [`ClipWarp::effective_markers`] is what plays.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WarpQuantise {
    /// The grid, in beats of the clip: a quarter is 1, a sixteenth 0.25.
    pub grid: f64,
    /// How far each marker is pulled onto its grid line: 0 not at all, 1 all
    /// the way.
    pub amount: f64,
    /// A feel to put on the grid lines: where a line is a slot of the groove,
    /// the marker goes to the line plus the slot's offset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub groove: Option<Groove>,
    /// How much of the groove is used, 0 to 1.
    #[serde(default = "full")]
    pub groove_amount: f64,
    /// The warp the groove follows: its [`groove`](ClipWarp::groove) is copied
    /// into `groove` whenever it changes (the project does that). Cleared if
    /// that warp goes away; the last groove stays.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub groove_from: Option<WarpId>,
}

fn full() -> f64 {
    1.0
}

/// The grids warp quantise offers, in beats: bars down to thirty-second notes.
pub const QUANTISE_GRIDS: [f64; 6] = [4.0, 1.0, 0.5, 0.25, 0.125, 0.0625];

impl Default for WarpQuantise {
    fn default() -> Self {
        Self {
            grid: 0.25,
            amount: 1.0,
            groove: None,
            groove_amount: 1.0,
            groove_from: None,
        }
    }
}

impl WarpQuantise {
    /// Where a marker at `beat` is moved to.
    pub fn apply(&self, beat: f64) -> f64 {
        let line = (beat / self.grid).round() * self.grid;
        let target = match &self.groove {
            Some(groove) => line + groove.offset_at(line) * self.groove_amount,
            None => line,
        };
        beat + (target - beat) * self.amount
    }

    fn problem(&self) -> Option<&'static str> {
        if !(self.grid > 0.0 && self.grid.is_finite()) {
            return Some("the quantise grid is not a length");
        }
        if !(0.0..=1.0).contains(&self.amount) || !(0.0..=1.0).contains(&self.groove_amount) {
            return Some("a quantise amount is outside 0 to 1");
        }
        self.groove.as_ref().and_then(Groove::problem)
    }
}

fn yes() -> bool {
    true
}

/// How an audio clip is warped. The clip's audio is not in here: this is the
/// part of a clip that sections can share. The rest of a section (where it
/// starts, which part of the file it shows, gain, fades) is its own.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ClipWarp {
    #[serde(default)]
    pub tempo: TempoCurve,
    /// Sorted by time, and by beat. Each pins a moment of the clip to a beat.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub markers: Vec<WarpMarker>,
    #[serde(default)]
    pub mode: WarpMode,
    /// Whether the clip's beats are carried onto the timeline's tempo (so the
    /// clip follows tempo changes). Off, the clip plays at its natural speed.
    #[serde(default = "yes")]
    pub tempo_conversion: bool,
    /// Whether the warp markers apply.
    #[serde(default = "yes")]
    pub use_markers: bool,
    /// The part of the clip that repeats, if the clip loops. A section of a
    /// looping clip plays its window up to the end of the region and then
    /// the region over and over, for as long as the section lasts (see
    /// [`AudioClip::extent`](crate::AudioClip::extent)).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loop_region: Option<LoopRegion>,
    /// Warp quantise, if on. It moves the markers that play, not the ones that
    /// are stored (see [`effective_markers`](Self::effective_markers)), so it
    /// can be turned off or changed and the original timing is back.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quantise: Option<WarpQuantise>,
}

impl ClipWarp {
    /// A clip at one tempo, with no markers, following the timeline's tempo.
    pub fn at_tempo(bpm: f64) -> Self {
        Self {
            tempo: TempoCurve::constant(bpm),
            markers: Vec::new(),
            mode: WarpMode::Repitch,
            tempo_conversion: true,
            use_markers: true,
            loop_region: None,
            quantise: None,
        }
    }

    /// The warp of a clip recorded in Noodle from `start` for `seconds` (of
    /// which the first `lead` seconds are not played; see
    /// [`TempoCurve::from_tempo_map`]): its
    /// tempo is the timeline's there, so its beats are exact. Tempo
    /// conversion is off, so the take sounds as recorded until it is turned
    /// on to follow later tempo changes.
    pub fn recorded(map: &TempoMap, start: Tick, lead: f64, seconds: f64) -> Self {
        Self {
            tempo: TempoCurve::from_tempo_map(map, start, lead, seconds),
            tempo_conversion: false,
            ..Self::default()
        }
    }

    /// Whether the clip plays at its natural speed all along, so that it
    /// needs no warping. A looping clip is never left alone: the engine lays
    /// its repeats out.
    pub fn is_identity(&self) -> bool {
        !self.tempo_conversion
            && (!self.use_markers || self.markers.is_empty())
            && self.loop_region.is_none()
    }

    /// The loop region in source seconds, if the clip loops.
    pub fn loop_seconds(&self) -> Option<(f64, f64)> {
        let region = self.loop_region?;
        Some((
            self.tempo.seconds_at(region.start),
            self.tempo.seconds_at(region.end),
        ))
    }

    /// The markers as they play: the stored ones, moved by warp quantise if it
    /// is on. A marker that would land on or before the one before it, or that
    /// would squeeze or stretch the clip beyond the limits, is left out, so
    /// whatever markers are stored, these always make a warp that plays.
    pub fn effective_markers(&self) -> Cow<'_, [WarpMarker]> {
        let Some(quantise) = &self.quantise else {
            return Cow::Borrowed(&self.markers);
        };
        let mut out: Vec<WarpMarker> = Vec::with_capacity(self.markers.len());
        for marker in &self.markers {
            let beat = quantise.apply(marker.beat);
            if let Some(previous) = out.last() {
                let clip_beats =
                    self.tempo.beats_at(marker.seconds) - self.tempo.beats_at(previous.seconds);
                let speed = clip_beats / (beat - previous.beat);
                if beat <= previous.beat + 1e-9
                    || !(MIN_WARP_SPEED..=MAX_WARP_SPEED).contains(&speed)
                {
                    continue;
                }
            }
            out.push(WarpMarker {
                seconds: marker.seconds,
                beat,
            });
        }
        Cow::Owned(out)
    }

    /// Where the stored marker `index` plays, which differs from its stored
    /// beat while warp quantise is on.
    pub fn marker_beat(&self, index: usize) -> Option<f64> {
        let marker = self.markers.get(index)?;
        Some(
            self.quantise
                .as_ref()
                .map_or(marker.beat, |q| q.apply(marker.beat)),
        )
    }

    /// The feel of this clip: how far its stored markers sit from the sixteenth
    /// grid, over a bar. Pin the clip's transients with markers to capture how
    /// it swings. `None` without markers.
    pub fn groove(&self) -> Option<Groove> {
        Groove::from_beats(
            self.markers.iter().map(|m| m.beat),
            GROOVE_SLOT,
            GROOVE_SLOTS,
        )
    }

    /// This warp with warp quantise made permanent: the markers are the ones
    /// that played, and quantise is off.
    pub fn baked(&self) -> Self {
        Self {
            markers: self.effective_markers().into_owned(),
            quantise: None,
            ..self.clone()
        }
    }

    /// The beat of the clip (the beats warp markers are given in, with the
    /// markers applied if they are in use) at which the moment `seconds` of
    /// the file plays.
    pub fn beat_at(&self, seconds: f64) -> f64 {
        let beats = self.tempo.beats_at(seconds);
        self.marker_map().map_or(beats, |markers| markers.at(beats))
    }

    /// The moment of the file (in seconds) that plays at `beat` of the clip.
    /// The inverse of [`beat_at`](Self::beat_at).
    pub fn seconds_at_beat(&self, beat: f64) -> f64 {
        let beats = self
            .marker_map()
            .map_or(beat, |markers| markers.inverse(beat));
        self.tempo.seconds_at(beats)
    }

    /// The marker at `seconds` (give or take a millisecond), if there is one.
    pub fn marker_at(&self, seconds: f64) -> Option<usize> {
        self.markers
            .iter()
            .position(|m| (m.seconds - seconds).abs() < 1e-3)
    }

    /// Adds a marker, keeping them in order. A marker at (nearly) the same
    /// moment as one already there replaces it.
    pub fn set_marker(&mut self, marker: WarpMarker) {
        self.markers
            .retain(|m| (m.seconds - marker.seconds).abs() > 1e-6);
        let at = self.markers.partition_point(|m| m.seconds < marker.seconds);
        self.markers.insert(at, marker);
    }

    /// Why this can't be a warp, if it can't.
    pub fn problem(&self) -> Option<&'static str> {
        if let Some(problem) = self.tempo.problem() {
            return Some(problem);
        }
        for m in &self.markers {
            if !m.seconds.is_finite() || !m.beat.is_finite() || m.seconds < 0.0 {
                return Some("a warp marker is not at a real place");
            }
        }
        if let Some(problem) = self.quantise.as_ref().and_then(WarpQuantise::problem) {
            return Some(problem);
        }
        if let Some(region) = self.loop_region
            && (!region.start.is_finite()
                || !region.end.is_finite()
                || region.start < 0.0
                || region.beats() < MIN_LOOP_BEATS - 1e-9)
        {
            return Some("the loop is shorter than a beat or starts before the clip");
        }
        for pair in self.markers.windows(2) {
            if pair[1].seconds <= pair[0].seconds || pair[1].beat <= pair[0].beat {
                return Some("warp markers cross each other or sit on the same spot");
            }
            let beats = self.tempo.beats_at(pair[1].seconds) - self.tempo.beats_at(pair[0].seconds);
            let speed = beats / (pair[1].beat - pair[0].beat);
            if !(MIN_WARP_SPEED..=MAX_WARP_SPEED).contains(&speed) {
                return Some("warp markers squeeze or stretch the clip too far");
            }
        }
        None
    }

    /// The markers as pairs of (clip beat, warped beat), if they apply.
    fn marker_map(&self) -> Option<PiecewiseLinear> {
        if !self.use_markers || self.markers.is_empty() {
            return None;
        }
        let knots: Vec<_> = self
            .effective_markers()
            .iter()
            .map(|m| (self.tempo.beats_at(m.seconds), m.beat))
            .collect();
        Some(PiecewiseLinear::new(knots, 1.0, 1.0))
    }

    /// The curve from moments of the clip to where they play.
    pub fn curve(&self) -> WarpCurve {
        let beats = self.tempo.as_curve();
        let markers = self.marker_map();
        let (axis, tail) = if self.tempo_conversion {
            (
                WarpAxis::Beats,
                self.tempo.points.last().expect("never empty").bpm / 60.0,
            )
        } else {
            (WarpAxis::Seconds, 1.0)
        };

        // Moments where the curve can bend: where the clip tempo changes,
        // where markers are, and (playing in seconds) where the warped beat
        // reaches a tempo change.
        let mut times: Vec<f64> = vec![0.0];
        times.extend(self.tempo.points.iter().map(|p| p.seconds));
        times.extend(self.effective_markers().iter().map(|m| m.seconds));
        if axis == WarpAxis::Seconds
            && let Some(markers) = &markers
        {
            for p in &self.tempo.points[1..] {
                let at = markers.inverse(beats.at(p.seconds));
                times.push(beats.inverse(at));
            }
        }
        times.sort_by(f64::total_cmp);
        times.dedup_by(|a, b| (*a - *b).abs() < 1e-9);
        times.retain(|t| *t >= 0.0);

        let warped_beats = |t: f64| {
            let b = beats.at(t);
            markers.as_ref().map_or(b, |m| m.at(b))
        };
        let knots = times
            .iter()
            .map(|&t| {
                let out = match axis {
                    WarpAxis::Beats => warped_beats(t),
                    WarpAxis::Seconds => beats.inverse(warped_beats(t)),
                };
                (t, out)
            })
            .collect();
        let head = match axis {
            WarpAxis::Beats => self.tempo.points[0].bpm / 60.0,
            WarpAxis::Seconds => 1.0,
        };
        WarpCurve {
            axis,
            curve: PiecewiseLinear::new(knots, head, tail),
        }
    }
}

impl Default for ClipWarp {
    fn default() -> Self {
        Self::at_tempo(120.0)
    }
}

/// What a [`WarpCurve`] measures where the clip plays.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WarpAxis {
    /// Timeline beats from the clip's beat 0: the clip follows the
    /// timeline's tempo.
    Beats,
    /// Seconds from the clip's start: the clip keeps its natural speed.
    Seconds,
}

/// Where each moment of a clip plays, relative to the clip's own start (which
/// is moment 0 of the file).
#[derive(Clone, Debug, PartialEq)]
pub struct WarpCurve {
    pub axis: WarpAxis,
    curve: PiecewiseLinear,
}

impl WarpCurve {
    /// Where source second `seconds` plays: in beats or seconds, by
    /// [`axis`](Self::axis).
    pub fn at(&self, seconds: f64) -> f64 {
        self.curve.at(seconds)
    }

    /// The source second that plays at `out`.
    pub fn source_at(&self, out: f64) -> f64 {
        self.curve.inverse(out)
    }

    /// The points where the curve bends, as (source seconds, out).
    pub fn knots(&self) -> &[(f64, f64)] {
        self.curve.knots()
    }
}

/// A rising line that bends at knots, and carries on straight past both ends.
#[derive(Clone, Debug, PartialEq)]
pub struct PiecewiseLinear {
    knots: Vec<(f64, f64)>,
    head: f64,
    tail: f64,
}

impl PiecewiseLinear {
    /// `knots` rise in both coordinates. `head` and `tail` are the slopes
    /// before the first and after the last.
    pub fn new(knots: Vec<(f64, f64)>, head: f64, tail: f64) -> Self {
        debug_assert!(!knots.is_empty());
        Self { knots, head, tail }
    }

    pub fn knots(&self) -> &[(f64, f64)] {
        &self.knots
    }

    pub fn at(&self, x: f64) -> f64 {
        self.eval(x, |k| k.0, |k| k.1, self.head, self.tail)
    }

    pub fn inverse(&self, y: f64) -> f64 {
        self.eval(y, |k| k.1, |k| k.0, 1.0 / self.head, 1.0 / self.tail)
    }

    fn eval(
        &self,
        x: f64,
        from: impl Fn(&(f64, f64)) -> f64,
        to: impl Fn(&(f64, f64)) -> f64,
        head: f64,
        tail: f64,
    ) -> f64 {
        let first = &self.knots[0];
        let last = &self.knots[self.knots.len() - 1];
        if x <= from(first) {
            return to(first) + (x - from(first)) * head;
        }
        if x >= from(last) {
            return to(last) + (x - from(last)) * tail;
        }
        let i = self.knots.partition_point(|k| from(k) <= x) - 1;
        let (a, b) = (&self.knots[i], &self.knots[i + 1]);
        to(a) + (x - from(a)) / (from(b) - from(a)) * (to(b) - to(a))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-9, "{a} != {b}");
    }

    #[test]
    fn beats_of_the_clip_follow_the_markers_and_back() {
        let mut warp = ClipWarp::at_tempo(120.0);
        close(warp.beat_at(1.0), 2.0);
        close(warp.seconds_at_beat(2.0), 1.0);
        warp.set_marker(WarpMarker {
            seconds: 1.0,
            beat: 3.0,
        });
        // The marker pulls its moment to beat 3; before it, the clip keeps
        // its own speed.
        close(warp.beat_at(1.0), 3.0);
        close(warp.beat_at(0.5), 2.0);
        close(warp.seconds_at_beat(3.0), 1.0);
        close(warp.seconds_at_beat(2.0), 0.5);
        // With the markers off they do nothing.
        warp.use_markers = false;
        close(warp.beat_at(1.0), 2.0);
    }

    #[test]
    fn a_loop_shorter_than_a_beat_is_not_a_warp() {
        let mut warp = ClipWarp::at_tempo(120.0);
        warp.loop_region = Some(LoopRegion {
            start: 1.0,
            end: 1.5,
        });
        assert!(warp.problem().is_some());
        warp.loop_region = Some(LoopRegion {
            start: 1.0,
            end: 3.0,
        });
        assert_eq!(warp.problem(), None);
        assert_eq!(warp.loop_seconds(), Some((0.5, 1.5)));
        // A looping clip is never "left alone" by the engine.
        assert!(!warp.is_identity());
    }

    #[test]
    fn beats_follow_the_clip_tempo_and_back() {
        let tempo = TempoCurve::new(vec![
            TempoPoint {
                seconds: 0.0,
                bpm: 120.0,
            },
            TempoPoint {
                seconds: 2.0,
                bpm: 60.0,
            },
        ])
        .unwrap();
        // Two seconds at 120 is 4 beats, then one beat a second.
        close(tempo.beats_at(2.0), 4.0);
        close(tempo.beats_at(5.0), 7.0);
        close(tempo.seconds_at(7.0), 5.0);
        close(tempo.seconds_at(1.0), 0.5);
        // Carried on past both ends.
        close(tempo.beats_at(-1.0), -2.0);
        assert_eq!(tempo.bpm_at(1.9), 120.0);
        assert_eq!(tempo.bpm_at(2.0), 60.0);
    }

    #[test]
    fn a_recording_takes_its_tempo_from_the_timeline() {
        use crate::{SignatureChange, TempoChange, TimeSignature};
        let map = TempoMap::new(
            vec![
                TempoChange {
                    tick: Tick(0),
                    bpm: 120.0,
                },
                TempoChange {
                    tick: Tick(4 * 960),
                    bpm: 90.0,
                },
                TempoChange {
                    tick: Tick(16 * 960),
                    bpm: 60.0,
                },
            ],
            vec![SignatureChange {
                bar: 0,
                signature: TimeSignature::COMMON,
            }],
        )
        .unwrap();
        // From beat 2 for five seconds: 2 s of 120 bpm are left in the first
        // stretch, then 90 bpm. The change to 60 is further than that.
        let warp = ClipWarp::recorded(&map, Tick(2 * 960), 0.0, 5.0);
        assert_eq!(warp.problem(), None);
        let points = warp.tempo.points();
        assert_eq!(points.len(), 2);
        assert_eq!((points[0].seconds, points[0].bpm), (0.0, 120.0));
        assert!((points[1].seconds - 1.0).abs() < 1e-9);
        assert_eq!(points[1].bpm, 90.0);
        // It sounds as recorded until the conversion is turned on.
        assert!(warp.is_identity());
        // A take whose first half second is dropped for latency: the change
        // comes that much later in the file.
        let trimmed = ClipWarp::recorded(&map, Tick(2 * 960), 0.5, 5.0);
        assert!((trimmed.tempo.points()[1].seconds - 1.5).abs() < 1e-9);
        // Starting past a change takes the tempo there.
        let late = TempoCurve::from_tempo_map(&map, Tick(20 * 960), 0.0, 10.0);
        assert_eq!(late.points().len(), 1);
        assert_eq!(late.points()[0].bpm, 60.0);
    }

    #[test]
    fn a_tempo_curve_is_checked() {
        assert!(TempoCurve::new(vec![]).is_err());
        assert!(
            TempoCurve::new(vec![TempoPoint {
                seconds: 0.0,
                bpm: 0.0
            }])
            .is_err()
        );
        let p = |seconds, bpm| TempoPoint { seconds, bpm };
        assert!(TempoCurve::new(vec![p(0.0, 100.0), p(0.0, 90.0)]).is_err());
        // The first point is moved to the start.
        let curve = TempoCurve::new(vec![p(3.0, 100.0), p(4.0, 90.0)]).unwrap();
        assert_eq!(curve.points()[0].seconds, 0.0);
    }

    #[test]
    fn no_markers_and_conversion_on_maps_clip_beats_to_timeline_beats() {
        let warp = ClipWarp::at_tempo(90.0);
        let curve = warp.curve();
        assert_eq!(curve.axis, WarpAxis::Beats);
        // 90 bpm: a second and a half is 2.25 beats.
        close(curve.at(1.5), 2.25);
        close(curve.source_at(2.25), 1.5);
    }

    #[test]
    fn conversion_off_and_no_markers_is_natural_speed() {
        let mut warp = ClipWarp::at_tempo(90.0);
        warp.tempo_conversion = false;
        assert!(warp.is_identity());
        let curve = warp.curve();
        assert_eq!(curve.axis, WarpAxis::Seconds);
        close(curve.at(3.0), 3.0);
        // Markers that are switched off change nothing.
        warp.markers.push(WarpMarker {
            seconds: 1.0,
            beat: 5.0,
        });
        warp.use_markers = false;
        assert!(warp.is_identity());
        close(warp.curve().at(3.0), 3.0);
        warp.use_markers = true;
        assert!(!warp.is_identity());
    }

    #[test]
    fn markers_pin_moments_to_beats_and_the_audio_between_stretches() {
        // 120 bpm: a beat every half second.
        let mut warp = ClipWarp::at_tempo(120.0);
        // The hit at 1.0 s (beat 2) is pulled to beat 3, and the one at 2.0 s
        // (beat 4) to beat 4.5.
        warp.set_marker(WarpMarker {
            seconds: 2.0,
            beat: 4.5,
        });
        warp.set_marker(WarpMarker {
            seconds: 1.0,
            beat: 3.0,
        });
        assert_eq!(warp.problem(), None);
        let curve = warp.curve();
        close(curve.at(1.0), 3.0);
        close(curve.at(2.0), 4.5);
        // Halfway between the markers lands halfway between their beats.
        close(curve.at(1.5), 3.75);
        // Before the first marker the clip plays at its own tempo, shifted so
        // the marker lands where it was put.
        close(curve.at(0.0), 1.0);
        close(curve.at(0.5), 2.0);
        // And likewise after the last.
        close(curve.at(3.0), 6.5);
        close(curve.source_at(3.75), 1.5);
    }

    #[test]
    fn markers_may_not_cross_or_stretch_absurdly() {
        let mut warp = ClipWarp::at_tempo(120.0);
        warp.markers = vec![
            WarpMarker {
                seconds: 1.0,
                beat: 3.0,
            },
            WarpMarker {
                seconds: 2.0,
                beat: 2.5,
            },
        ];
        assert!(warp.problem().is_some());
        // A second of clip squeezed into a hundredth of a beat.
        warp.markers[1].beat = 3.01;
        assert!(warp.problem().is_some());
        warp.markers[1].beat = 5.0;
        assert_eq!(warp.problem(), None);
    }

    #[test]
    fn a_marker_replaces_one_at_the_same_moment() {
        let mut warp = ClipWarp::at_tempo(120.0);
        warp.set_marker(WarpMarker {
            seconds: 1.0,
            beat: 2.0,
        });
        warp.set_marker(WarpMarker {
            seconds: 1.0,
            beat: 2.5,
        });
        assert_eq!(warp.markers.len(), 1);
        assert_eq!(warp.markers[0].beat, 2.5);
    }

    fn marked(beats: &[(f64, f64)]) -> ClipWarp {
        let mut warp = ClipWarp::at_tempo(120.0);
        for &(seconds, beat) in beats {
            warp.set_marker(WarpMarker { seconds, beat });
        }
        warp
    }

    #[test]
    fn quantise_moves_the_markers_that_play_and_leaves_the_stored_ones() {
        let mut warp = marked(&[(0.5, 1.1), (1.0, 1.9), (1.6, 3.4)]);
        let stored = warp.markers.clone();
        warp.quantise = Some(WarpQuantise {
            grid: 0.5,
            amount: 1.0,
            ..WarpQuantise::default()
        });
        let beats: Vec<f64> = warp.effective_markers().iter().map(|m| m.beat).collect();
        assert_eq!(beats, vec![1.0, 2.0, 3.5]);
        assert_eq!(warp.markers, stored);
        // The curve follows the moved markers.
        close(warp.beat_at(0.5), 1.0);
        close(warp.beat_at(1.0), 2.0);
        // Half the amount is half the way.
        warp.quantise.as_mut().unwrap().amount = 0.5;
        close(warp.marker_beat(0).unwrap(), 1.05);
        // Off, the original timing is back.
        warp.quantise = None;
        close(warp.beat_at(0.5), 1.1);
        assert_eq!(warp.problem(), None);
    }

    #[test]
    fn quantise_never_makes_markers_that_cannot_play() {
        // The first two both land on beat 1.
        let mut warp = marked(&[(0.5, 0.9), (0.6, 1.1), (1.0, 1.9)]);
        warp.quantise = Some(WarpQuantise {
            grid: 1.0,
            ..WarpQuantise::default()
        });
        let kept = warp.effective_markers();
        assert_eq!(kept.len(), 2);
        assert_eq!((kept[0].seconds, kept[1].seconds), (0.5, 1.0));
        assert_eq!(warp.problem(), None);
        warp.curve();
    }

    #[test]
    fn baking_makes_the_quantised_markers_the_stored_ones() {
        let mut warp = marked(&[(0.5, 1.1), (1.0, 1.9)]);
        warp.quantise = Some(WarpQuantise {
            grid: 1.0,
            ..WarpQuantise::default()
        });
        let baked = warp.baked();
        assert_eq!(baked.quantise, None);
        assert_eq!(baked.markers, warp.effective_markers().into_owned());
        for t in [0.0, 0.3, 0.5, 0.8, 1.0, 2.0] {
            close(baked.beat_at(t), warp.beat_at(t));
        }
    }

    #[test]
    fn a_groove_is_the_mean_distance_of_beats_from_their_slots() {
        // Offbeat eighths late by a 32nd; the downbeats straight.
        let beats = [0.0, 0.5625, 1.0, 1.5625, 4.0, 4.5625];
        let groove = Groove::from_beats(beats, 0.25, 16).unwrap();
        close(groove.offset_at(0.5), 0.0625);
        close(groove.offset_at(1.5), 0.0625);
        // A bar later is the same slot.
        close(groove.offset_at(4.5), 0.0625);
        close(groove.offset_at(1.0), 0.0);
        // Off the grid there is no offset.
        close(groove.offset_at(0.6), 0.0);
        assert!(!groove.is_flat());
        assert!(Groove::from_beats([], 0.25, 16).is_none());
        let straight = Groove::from_beats([0.0, 1.0, 2.0], 0.25, 16).unwrap();
        assert!(straight.is_flat());
    }

    #[test]
    fn a_groove_puts_its_feel_on_the_grid() {
        let feel = marked(&[(0.0, 0.0), (0.25, 0.5625), (0.5, 1.0)]);
        let groove = feel.groove().unwrap();
        // Another clip with straight, slightly loose markers.
        let mut warp = marked(&[(0.0, 0.02), (0.25, 0.49), (0.5, 1.03)]);
        warp.quantise = Some(WarpQuantise {
            grid: 0.5,
            groove: Some(groove),
            ..WarpQuantise::default()
        });
        let beats: Vec<f64> = warp.effective_markers().iter().map(|m| m.beat).collect();
        assert_eq!(beats.len(), 3);
        close(beats[0], 0.0);
        close(beats[1], 0.5625);
        close(beats[2], 1.0);
        // Half the groove is half the offset.
        warp.quantise.as_mut().unwrap().groove_amount = 0.5;
        close(warp.marker_beat(1).unwrap(), 0.53125);
    }

    #[test]
    fn a_bad_quantise_is_a_problem() {
        let mut warp = marked(&[(0.5, 1.0)]);
        warp.quantise = Some(WarpQuantise {
            grid: 0.0,
            ..WarpQuantise::default()
        });
        assert!(warp.problem().is_some());
        warp.quantise = Some(WarpQuantise {
            amount: 1.5,
            ..WarpQuantise::default()
        });
        assert!(warp.problem().is_some());
    }

    #[test]
    fn quantise_round_trips_through_ron() {
        let mut warp = marked(&[(0.5, 1.0)]);
        warp.quantise = Some(WarpQuantise {
            groove: Groove::from_beats([0.5625], 0.25, 16),
            ..WarpQuantise::default()
        });
        let text = ron::to_string(&warp).unwrap();
        assert_eq!(ron::from_str::<ClipWarp>(&text).unwrap(), warp);
        // Files from before quantise load.
        let plain = ron::to_string(&marked(&[(0.5, 1.0)])).unwrap();
        assert_eq!(ron::from_str::<ClipWarp>(&plain).unwrap().quantise, None);
    }

    #[test]
    fn playing_in_seconds_bends_where_the_warped_beat_meets_a_tempo_change() {
        // 120 bpm for two seconds (4 beats), then 60 bpm.
        let mut warp = ClipWarp {
            tempo: TempoCurve::new(vec![
                TempoPoint {
                    seconds: 0.0,
                    bpm: 120.0,
                },
                TempoPoint {
                    seconds: 2.0,
                    bpm: 60.0,
                },
            ])
            .unwrap(),
            tempo_conversion: false,
            ..ClipWarp::default()
        };
        // Pin the moment at beat 1 (0.5 s) to beat 4, the clip's tempo change.
        warp.set_marker(WarpMarker {
            seconds: 0.5,
            beat: 4.0,
        });
        let curve = warp.curve();
        // Warped beat 4 is 2 s at the first tempo, so the pin lands there.
        close(curve.at(0.5), 2.0);
        // Past it, the beat count runs 3 beats a... check against the direct
        // composition at many points.
        let beats = warp.tempo.as_curve();
        let m = warp.marker_map().unwrap();
        for i in 0..400 {
            let t = i as f64 * 0.025;
            close(curve.at(t), beats.inverse(m.at(beats.at(t))));
        }
    }

    #[test]
    fn the_curve_matches_its_definition_everywhere() {
        let mut warp = ClipWarp {
            tempo: TempoCurve::new(vec![
                TempoPoint {
                    seconds: 0.0,
                    bpm: 100.0,
                },
                TempoPoint {
                    seconds: 1.7,
                    bpm: 140.0,
                },
                TempoPoint {
                    seconds: 3.1,
                    bpm: 80.0,
                },
            ])
            .unwrap(),
            ..ClipWarp::default()
        };
        warp.set_marker(WarpMarker {
            seconds: 0.9,
            beat: 1.0,
        });
        warp.set_marker(WarpMarker {
            seconds: 2.4,
            beat: 5.0,
        });
        warp.set_marker(WarpMarker {
            seconds: 4.0,
            beat: 9.0,
        });
        assert_eq!(warp.problem(), None);
        let beats = warp.tempo.as_curve();
        let m = warp.marker_map().unwrap();
        for conversion in [true, false] {
            warp.tempo_conversion = conversion;
            let curve = warp.curve();
            for i in 0..600 {
                let t = i as f64 * 0.01;
                let direct = if conversion {
                    m.at(beats.at(t))
                } else {
                    beats.inverse(m.at(beats.at(t)))
                };
                close(curve.at(t), direct);
                close(curve.source_at(curve.at(t)), t);
            }
        }
    }

    #[test]
    fn a_warp_survives_a_save_and_old_fields_default() {
        let warp = ClipWarp {
            markers: vec![WarpMarker {
                seconds: 1.0,
                beat: 2.5,
            }],
            ..ClipWarp::at_tempo(133.0)
        };
        let text = ron::to_string(&warp).unwrap();
        assert_eq!(ron::from_str::<ClipWarp>(&text).unwrap(), warp);
        // Only the tempo written: everything else takes its default.
        let bare: ClipWarp =
            ron::from_str("(tempo: (points: [(seconds: 0.0, bpm: 90.0)]))").unwrap();
        assert_eq!(bare, ClipWarp::at_tempo(90.0));
    }
}
