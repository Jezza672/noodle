//! A warped section in the engine's units: for every frame of output, where in
//! the source to read.
//!
//! [`ClipWarp::curve`](noodle_core::ClipWarp::curve) says where each moment of
//! a clip plays, in beats or seconds. The map turns that, for one section, into
//! frames at the engine's rate: output frames counted from the start of the
//! section, source frames counted from the start of the section's window onto
//! the file, which is what [`ClipStream`](noodle_io::ClipStream) positions
//! count. Between knots the map is a straight line, so playing it is a
//! multiplication per frame, and it stays exact through tempo changes in the
//! timeline because the map has a knot at each one.

use noodle_core::{ENGINE_MAX_SPEED, TICKS_PER_QUARTER, Tick, WarpAxis, WarpCurve};
use noodle_engine::TempoTable;

/// A rising line from output frames to source frames.
#[derive(Clone, Debug, PartialEq)]
pub struct WarpMap {
    /// (output frame, source frame), rising in both. Starts at (0, 0) and ends
    /// where the section's window does.
    knots: Vec<(f64, f64)>,
}

impl WarpMap {
    /// The map for a section starting at `start`, showing `window` seconds of
    /// the file from `origin` seconds, at `rate`. `source_frames` is how many
    /// frames of the engine's rate the window has, so the end lines up with
    /// what the stream plays.
    pub fn build(
        curve: &WarpCurve,
        table: &TempoTable,
        start: Tick,
        rate: f64,
        origin: f64,
        window: f64,
        source_frames: u64,
    ) -> Self {
        let end = origin + window;
        let out0 = curve.at(origin);
        // The points where the line can bend, as (output frame, source second).
        let mut points: Vec<(f64, f64)> = Vec::new();
        match curve.axis {
            WarpAxis::Seconds => {
                points.push((0.0, origin));
                for &(src, out) in curve.knots() {
                    if src > origin && src < end {
                        points.push(((out - out0) * rate, src));
                    }
                }
                points.push(((curve.at(end) - out0) * rate, end));
            }
            WarpAxis::Beats => {
                let ticks_per_beat = TICKS_PER_QUARTER as f64;
                let start = start.0 as f64;
                let end_tick = start + (curve.at(end) - out0) * ticks_per_beat;
                let mut ticks = vec![start, end_tick];
                for &(src, out) in curve.knots() {
                    if src > origin && src < end {
                        ticks.push(start + (out - out0) * ticks_per_beat);
                    }
                }
                ticks.extend(
                    table
                        .tempo_changes()
                        .filter(|t| *t > start && *t < end_tick),
                );
                ticks.sort_by(f64::total_cmp);
                let at_start = table.sample_at_exact(start);
                for tick in ticks {
                    let frame = table.sample_at_exact(tick) - at_start;
                    let src = curve.source_at(out0 + (tick - start) / ticks_per_beat);
                    points.push((frame, src));
                }
            }
        }
        let mut knots: Vec<(f64, f64)> = Vec::with_capacity(points.len());
        for (frame, src) in points {
            let src_frame = (src - origin) * rate;
            match knots.last() {
                // A knot that doesn't rise adds nothing.
                Some(&(f, s)) if frame <= f + 1e-9 || src_frame <= s + 1e-9 => {}
                _ => knots.push((frame, src_frame)),
            }
        }
        if knots.len() < 2 {
            // A window too short to bend; treat it as playing at natural speed.
            knots = vec![(0.0, 0.0), (source_frames as f64, source_frames as f64)];
        }
        // The stream's length is rounded to whole frames; end where it does.
        let last = knots.len() - 1;
        knots[last].1 = source_frames as f64;
        if knots[last].1 <= knots[last - 1].1 {
            knots[last - 1].1 = (knots[last].1 - 1.0).max(0.0);
        }
        knots[0] = (0.0, 0.0);
        // Never read faster than the disk stream can supply: where tempo
        // conversion and markers ask for more, play the part at the limit and
        // fall behind (the section then ends before the file's window does).
        let wanted: Vec<f64> = knots.iter().map(|k| k.1).collect();
        for i in 1..knots.len() {
            let allowed = (knots[i].0 - knots[i - 1].0) * ENGINE_MAX_SPEED;
            knots[i].1 = knots[i - 1].1 + (wanted[i] - wanted[i - 1]).min(allowed);
        }
        Self { knots }
    }

    /// A map through `knots`, for tests and benchmarks: (output frame, source
    /// frame), rising in both, from (0, 0).
    pub fn from_knots(knots: Vec<(f64, f64)>) -> Self {
        assert!(knots.len() >= 2 && knots[0] == (0.0, 0.0));
        assert!(knots.windows(2).all(|k| k[1].0 > k[0].0 && k[1].1 > k[0].1));
        Self { knots }
    }

    /// How many source frames the section's window has.
    pub fn source_len(&self) -> f64 {
        self.knots[self.knots.len() - 1].1
    }

    /// How many output frames the section lasts.
    pub fn out_len(&self) -> f64 {
        self.knots[self.knots.len() - 1].0
    }

    /// The source frame, fractions included, that output frame `out` plays.
    pub fn source_at(&self, out: f64) -> f64 {
        self.eval(out, |k| k.0, |k| k.1)
    }

    /// The output frame at which source frame `src` plays.
    pub fn out_at(&self, src: f64) -> f64 {
        self.eval(src, |k| k.1, |k| k.0)
    }

    fn eval(
        &self,
        x: f64,
        from: impl Fn(&(f64, f64)) -> f64,
        to: impl Fn(&(f64, f64)) -> f64,
    ) -> f64 {
        let n = self.knots.len();
        let i = self.knots.partition_point(|k| from(k) <= x).clamp(1, n - 1) - 1;
        let (a, b) = (&self.knots[i], &self.knots[i + 1]);
        to(a) + (x - from(a)) / (from(b) - from(a)) * (to(b) - to(a))
    }

    /// How many source frames pass per output frame at output frame `out`.
    pub fn speed_at(&self, out: f64) -> f64 {
        let n = self.knots.len();
        let i = self.knots.partition_point(|k| k.0 <= out).clamp(1, n - 1) - 1;
        let (a, b) = (&self.knots[i], &self.knots[i + 1]);
        (b.1 - a.1) / (b.0 - a.0)
    }

    pub fn knots(&self) -> &[(f64, f64)] {
        &self.knots
    }
}

/// Four-point cubic (Catmull-Rom) interpolation between `y1` and `y2`, `t`
/// of the way. It passes through the samples, so a map that lands on whole
/// frames reproduces the source exactly.
pub fn cubic(y0: f32, y1: f32, y2: f32, y3: f32, t: f32) -> f32 {
    let c1 = 0.5 * (y2 - y0);
    let c2 = y0 - 2.5 * y1 + 2.0 * y2 - 0.5 * y3;
    let c3 = 0.5 * (y3 - y0) + 1.5 * (y1 - y2);
    ((c3 * t + c2) * t + c1) * t + y1
}

#[cfg(test)]
mod tests {
    use noodle_core::{ClipWarp, TempoChange, TempoMap, TimeSignature, WarpMarker};

    use super::*;

    const RATE: f64 = 48_000.0;

    fn table(bpm: f64) -> TempoTable {
        TempoTable::new(
            &TempoMap::constant(bpm, TimeSignature::COMMON).unwrap(),
            RATE as f32,
        )
    }

    fn close(a: f64, b: f64, within: f64) {
        assert!((a - b).abs() <= within, "{a} != {b}");
    }

    fn build(warp: &ClipWarp, table: &TempoTable, start: i64, origin: f64, window: f64) -> WarpMap {
        let frames = (window * RATE).round() as u64;
        WarpMap::build(
            &warp.curve(),
            table,
            Tick(start),
            RATE,
            origin,
            window,
            frames,
        )
    }

    #[test]
    fn a_clip_at_the_timeline_tempo_plays_at_natural_speed() {
        let warp = ClipWarp::at_tempo(120.0);
        let map = build(&warp, &table(120.0), 0, 0.0, 4.0);
        assert_eq!(map.knots().len(), 2);
        close(map.out_len(), 4.0 * RATE, 1e-6);
        close(map.source_at(48_000.0), 48_000.0, 1e-6);
        close(map.speed_at(10.0), 1.0, 1e-9);
    }

    #[test]
    fn a_faster_timeline_plays_the_clip_faster_and_a_longer_section_follows() {
        // A clip recorded at 100 bpm on a timeline at 150: 1.5 times as fast.
        let warp = ClipWarp::at_tempo(100.0);
        let map = build(&warp, &table(150.0), 0, 0.0, 6.0);
        close(map.speed_at(0.0), 1.5, 1e-9);
        close(map.out_len(), 4.0 * RATE, 1e-6);
        // And with the conversion off, it ignores the timeline.
        let mut off = warp;
        off.tempo_conversion = false;
        let map = build(&off, &table(150.0), 0, 0.0, 6.0);
        close(map.speed_at(0.0), 1.0, 1e-9);
        close(map.out_len(), 6.0 * RATE, 1e-6);
    }

    #[test]
    fn the_map_never_reads_faster_than_the_engine_limit() {
        // A 2 bpm clip on a 120 bpm timeline asks for 60 times natural speed.
        let warp = ClipWarp::at_tempo(2.0);
        let map = build(&warp, &table(120.0), 0, 0.0, 4.0);
        close(map.out_len(), 4.0 * RATE / 60.0, 1e-6);
        for (a, b) in map.knots().iter().zip(map.knots().iter().skip(1)) {
            assert!((b.1 - a.1) / (b.0 - a.0) <= noodle_core::ENGINE_MAX_SPEED + 1e-9);
        }
        close(map.speed_at(10.0), noodle_core::ENGINE_MAX_SPEED, 1e-9);
        assert!(map.source_len() < 4.0 * RATE);
    }

    #[test]
    fn markers_bend_the_map_at_their_frames() {
        // 120 bpm on a 120 timeline. The hit at 1 s (beat 2) stays on beat 2;
        // the one at 2 s (beat 4) is pulled back to beat 5.
        let mut warp = ClipWarp::at_tempo(120.0);
        warp.set_marker(WarpMarker {
            seconds: 1.0,
            beat: 2.0,
        });
        warp.set_marker(WarpMarker {
            seconds: 2.0,
            beat: 5.0,
        });
        let map = build(&warp, &table(120.0), 0, 0.0, 4.0);
        close(map.out_at(RATE), RATE, 1e-6);
        close(map.out_at(2.0 * RATE), 2.5 * RATE, 1e-6);
        close(map.source_at(2.5 * RATE), 2.0 * RATE, 1e-6);
        // Between them the clip crawls: a second of audio takes 1.5.
        close(map.speed_at(1.2 * RATE), 2.0 / 3.0, 1e-9);
        // Before and after, it plays at the clip's own tempo.
        close(map.speed_at(0.5 * RATE), 1.0, 1e-9);
        close(map.speed_at(3.0 * RATE), 1.0, 1e-9);
        close(map.out_len(), 4.5 * RATE, 1e-6);
    }

    #[test]
    fn a_tempo_change_in_the_timeline_bends_the_map() {
        let changes = vec![
            TempoChange {
                tick: Tick(0),
                bpm: 120.0,
            },
            TempoChange {
                tick: Tick(4 * 960),
                bpm: 60.0,
            },
        ];
        let sigs = vec![noodle_core::SignatureChange {
            bar: 0,
            signature: TimeSignature::COMMON,
        }];
        let table = TempoTable::new(&TempoMap::new(changes, sigs).unwrap(), RATE as f32);
        let warp = ClipWarp::at_tempo(120.0);
        let map = build(&warp, &table, 0, 0.0, 6.0);
        // Four beats at 120 (2 s), then a clip beat takes a full second.
        close(map.speed_at(0.0), 1.0, 1e-9);
        close(map.speed_at(2.5 * RATE), 0.5, 1e-9);
        // 2 s at the first tempo, then the 4 s left of the clip (8 beats at
        // 120) take 8 s at 60 bpm.
        close(map.out_len(), 2.0 * RATE + 8.0 * RATE, 1e-3);
    }

    #[test]
    fn a_section_starting_part_way_through_the_clip_plays_that_part() {
        let mut warp = ClipWarp::at_tempo(120.0);
        warp.set_marker(WarpMarker {
            seconds: 2.0,
            beat: 5.0,
        });
        // Showing the clip from 1 s: the first half-second plays at natural
        // speed (to the marker at 2 s that sits one second on from it).
        let full = build(&warp, &table(120.0), 0, 0.0, 4.0);
        let part = build(&warp, &table(120.0), 0, 1.0, 3.0);
        close(part.out_len(), full.out_len() - full.out_at(RATE), 2.0);
        close(
            part.out_at(RATE),
            full.out_at(2.0 * RATE) - full.out_at(RATE),
            2.0,
        );
    }

    #[test]
    fn the_map_inverts_cleanly() {
        let mut warp = ClipWarp::at_tempo(97.0);
        warp.set_marker(WarpMarker {
            seconds: 0.7,
            beat: 1.0,
        });
        warp.set_marker(WarpMarker {
            seconds: 2.9,
            beat: 6.0,
        });
        let map = build(&warp, &table(133.0), 960, 0.0, 5.0);
        for i in 0..200 {
            let out = i as f64 * map.out_len() / 200.0;
            close(map.out_at(map.source_at(out)), out, 1e-6);
        }
    }

    #[test]
    fn cubic_interpolation_passes_through_the_samples() {
        for (a, b, c, d) in [(0.0, 1.0, 0.5, -1.0), (0.3, -0.2, 0.9, 0.1)] {
            assert_eq!(cubic(a, b, c, d, 0.0), b);
            assert!((cubic(a, b, c, d, 1.0) - c).abs() < 1e-6);
        }
        // A straight line stays straight.
        assert!((cubic(0.0, 1.0, 2.0, 3.0, 0.25) - 1.25).abs() < 1e-6);
    }
}
