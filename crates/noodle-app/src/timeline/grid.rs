//! The bar and beat lines the ruler and the lanes are drawn on.

use noodle_core::{TempoMap, Tick};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// The downbeat of a bar, counting from 0.
    Bar(u32),
    Beat,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Line {
    pub tick: Tick,
    pub kind: Kind,
}

/// Every beat line from `from` up to and including `to`, following the time
/// signature changes in `map`.
pub fn lines(map: &TempoMap, from: Tick, to: Tick) -> Vec<Line> {
    let mut lines = Vec::new();
    let signatures = map.signatures();
    let mut segment_start = 0i64;
    for (i, change) in signatures.iter().enumerate() {
        if segment_start > to.0 {
            break;
        }
        let per_beat = change.signature.ticks_per_beat();
        let per_bar = change.signature.ticks_per_bar();
        let beats_in_bar = i64::from(change.signature.numerator);
        // `None` for the last signature, which runs forever.
        let bars_here = signatures
            .get(i + 1)
            .map(|next| i64::from(next.bar - change.bar));
        let segment_end = bars_here.map_or(i64::MAX, |bars| segment_start + bars * per_bar);
        let first = (from.0.max(segment_start) - segment_start) / per_beat;
        let mut beat = first;
        loop {
            let tick = segment_start + beat * per_beat;
            if tick > to.0 || tick >= segment_end {
                break;
            }
            let kind = if beat % beats_in_bar == 0 {
                Kind::Bar(change.bar + (beat / beats_in_bar) as u32)
            } else {
                Kind::Beat
            };
            if tick >= from.0 {
                lines.push(Line {
                    tick: Tick(tick),
                    kind,
                });
            }
            beat += 1;
        }
        segment_start = segment_end;
    }
    lines
}

/// The beat line nearest `tick`, or `tick` itself if there is none.
pub fn snap(map: &TempoMap, tick: Tick) -> Tick {
    // The longest beat is a whole note (denominator 1): 3840 ticks.
    const REACH: i64 = 3840;
    let from = Tick((tick.0 - REACH).max(0));
    lines(map, from, Tick(tick.0 + REACH))
        .into_iter()
        .map(|line| line.tick)
        .min_by_key(|candidate| (candidate.0 - tick.0).abs())
        .unwrap_or(tick)
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_core::{SignatureChange, TempoChange, TimeSignature};

    fn map(signatures: &[(u32, u8, u8)]) -> TempoMap {
        TempoMap::new(
            vec![TempoChange {
                tick: Tick::ZERO,
                bpm: 120.0,
            }],
            signatures
                .iter()
                .map(|&(bar, numerator, denominator)| SignatureChange {
                    bar,
                    signature: TimeSignature {
                        numerator,
                        denominator,
                    },
                })
                .collect(),
        )
        .unwrap()
    }

    fn describe(lines: &[Line]) -> Vec<(i64, Option<u32>)> {
        lines
            .iter()
            .map(|l| match l.kind {
                Kind::Bar(n) => (l.tick.0, Some(n)),
                Kind::Beat => (l.tick.0, None),
            })
            .collect()
    }

    #[test]
    fn four_four_has_a_bar_line_every_four_beats() {
        let lines = lines(&map(&[(0, 4, 4)]), Tick(0), Tick(960 * 5));
        assert_eq!(
            describe(&lines),
            vec![
                (0, Some(0)),
                (960, None),
                (1920, None),
                (2880, None),
                (3840, Some(1)),
                (4800, None),
            ]
        );
    }

    #[test]
    fn lines_start_at_the_first_one_in_view() {
        let lines = lines(&map(&[(0, 4, 4)]), Tick(1000), Tick(2900));
        assert_eq!(describe(&lines), vec![(1920, None), (2880, None)]);
    }

    #[test]
    fn a_signature_change_restarts_the_count_of_beats() {
        // Two bars of 4/4 (7680 ticks), then 3/4.
        let lines = lines(&map(&[(0, 4, 4), (2, 3, 4)]), Tick(6000), Tick(7680 + 2900));
        assert_eq!(
            describe(&lines),
            vec![
                (7680 - 960, None),
                (7680, Some(2)),
                (7680 + 960, None),
                (7680 + 1920, None),
                (7680 + 2880, Some(3)),
            ]
        );
    }

    #[test]
    fn eighth_note_beats_are_half_a_quarter() {
        let lines = lines(&map(&[(0, 6, 8)]), Tick(0), Tick(1500));
        assert_eq!(
            describe(&lines),
            vec![(0, Some(0)), (480, None), (960, None), (1440, None)]
        );
    }

    #[test]
    fn snapping_goes_to_the_nearest_beat() {
        let map = map(&[(0, 4, 4)]);
        assert_eq!(snap(&map, Tick(1300)), Tick(960));
        assert_eq!(snap(&map, Tick(1500)), Tick(1920));
        assert_eq!(snap(&map, Tick(0)), Tick(0));
        assert_eq!(snap(&map, Tick(-50)), Tick(0));
    }
}
