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

/// How a warp is rendered. Only Repitch exists so far (plain resampling, so
/// pitch follows speed); the others (Grain, Beats) arrive with the engine.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum WarpMode {
    #[default]
    Repitch,
}

/// The slowest and fastest a clip may play, as a multiple of natural speed,
/// between two warp markers. Beyond these the sound is noise and the
/// engine's work per block is unbounded.
pub const MIN_WARP_SPEED: f64 = 1.0 / 16.0;
pub const MAX_WARP_SPEED: f64 = 16.0;

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
    /// there was played at, tempo changes included.
    pub fn from_tempo_map(map: &TempoMap, start: Tick, seconds: f64) -> Self {
        let began = map.seconds_at(start);
        let mut points = vec![TempoPoint {
            seconds: 0.0,
            bpm: map.bpm_at(start),
        }];
        for change in map.tempos().iter().filter(|c| c.tick > start) {
            let at = map.seconds_at(change.tick) - began;
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
        }
    }

    /// The warp of a clip recorded in Noodle from `start` for `seconds`: its
    /// tempo is the timeline's there, so its beats are exact. Tempo
    /// conversion is off, so the take sounds as recorded until it is turned
    /// on to follow later tempo changes.
    pub fn recorded(map: &TempoMap, start: Tick, seconds: f64) -> Self {
        Self {
            tempo: TempoCurve::from_tempo_map(map, start, seconds),
            tempo_conversion: false,
            ..Self::default()
        }
    }

    /// Whether the clip plays at its natural speed all along, so that it
    /// needs no warping.
    pub fn is_identity(&self) -> bool {
        !self.tempo_conversion && (!self.use_markers || self.markers.is_empty())
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
            .markers
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
        times.extend(self.markers.iter().map(|m| m.seconds));
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
        let warp = ClipWarp::recorded(&map, Tick(2 * 960), 5.0);
        assert_eq!(warp.problem(), None);
        let points = warp.tempo.points();
        assert_eq!(points.len(), 2);
        assert_eq!((points[0].seconds, points[0].bpm), (0.0, 120.0));
        assert!((points[1].seconds - 1.0).abs() < 1e-9);
        assert_eq!(points[1].bpm, 90.0);
        // It sounds as recorded until the conversion is turned on.
        assert!(warp.is_identity());
        // Starting past a change takes the tempo there.
        let late = TempoCurve::from_tempo_map(&map, Tick(20 * 960), 10.0);
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
