//! How times in the reference map onto times in the test.

/// A monotone map between the reference's timeline (source seconds) and the
/// test's (output seconds).
#[derive(Debug, Clone, PartialEq)]
pub enum TimeMap {
    /// The two are on the same timeline (a native render at the target tempo).
    Identity,
    /// A constant speed: the test plays `speed` times as fast as the
    /// reference, so 2 is half as long and 0.5 twice as long.
    Speed(f64),
    /// Points of (output seconds, source seconds), increasing in both.
    /// Between points the map is linear (a constant tempo); outside them it
    /// continues at the slope of the nearest segment.
    Points(Vec<(f64, f64)>),
}

impl TimeMap {
    /// The reference time that sounds at `output` seconds in the test.
    pub fn to_source(&self, output: f64) -> f64 {
        match self {
            Self::Identity => output,
            Self::Speed(s) => output * s,
            Self::Points(p) => interp(p, output, false),
        }
    }

    /// The test time at which the reference's `source` seconds sounds.
    pub fn to_output(&self, source: f64) -> f64 {
        match self {
            Self::Identity => source,
            Self::Speed(s) => source / s,
            Self::Points(p) => interp(p, source, true),
        }
    }
}

/// Linear interpolation over `points`, keyed by the second element when
/// `by_source` and by the first otherwise.
fn interp(points: &[(f64, f64)], x: f64, by_source: bool) -> f64 {
    let key = |p: &(f64, f64)| if by_source { p.1 } else { p.0 };
    let val = |p: &(f64, f64)| if by_source { p.0 } else { p.1 };
    match points.len() {
        0 => x,
        1 => x - key(&points[0]) + val(&points[0]),
        n => {
            let i = points.partition_point(|p| key(p) <= x).clamp(1, n - 1);
            let (a, b) = (&points[i - 1], &points[i]);
            let span = key(b) - key(a);
            let slope = if span > 0.0 {
                (val(b) - val(a)) / span
            } else {
                1.0
            };
            val(a) + (x - key(a)) * slope
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn points_map_both_ways_and_extrapolate() {
        // Twice as fast for 2 s of output (4 s of source), then real time.
        let map = TimeMap::Points(vec![(0.0, 0.0), (2.0, 4.0), (4.0, 6.0)]);
        assert_eq!(map.to_source(1.0), 2.0);
        assert_eq!(map.to_source(3.0), 5.0);
        assert_eq!(map.to_source(5.0), 7.0);
        assert_eq!(map.to_output(2.0), 1.0);
        assert_eq!(map.to_output(5.0), 3.0);
        assert_eq!(map.to_output(-1.0), -0.5);
    }

    #[test]
    fn speed_is_its_own_inverse_pair() {
        let m = TimeMap::Speed(2.0);
        assert_eq!(m.to_source(3.0), 6.0);
        assert_eq!(m.to_output(6.0), 3.0);
    }
}
