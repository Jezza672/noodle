//! Automation lanes: a parameter's value over the timeline. The compiler
//! turns each lane into a source wired into the parameter's port, so a lane
//! modulates what a wire does.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::{Endpoint, Tick};

/// Identifies a lane within a project. Like node IDs, lane IDs aren't reused
/// within a session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LaneId(pub u64);

impl fmt::Display for LaneId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "lane #{}", self.0)
    }
}

/// How the value gets from one point to the next.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub enum Curve {
    /// A straight line. The same as `Tension(0.0)`.
    #[default]
    Linear,
    /// A bent line, by a tension from -1 to 1: negative bends it into an
    /// exponential (slow start, fast end, down to -1), zero is straight, and
    /// positive leans it into an S-curve (up to 1). The segment still starts
    /// and ends exactly on its points and never doubles back.
    Tension(f32),
    /// Stays at the point's value until the next point, then jumps.
    Hold,
}

impl Curve {
    /// How far along the value is, from 0 to 1, when the segment is `along`
    /// (0 to 1) of the way through its time. `Hold` stays at 0 until the end.
    pub fn shape(self, along: f64) -> f64 {
        let x = along.clamp(0.0, 1.0);
        match self {
            Curve::Hold => 0.0,
            Curve::Linear => x,
            Curve::Tension(t) => {
                let t = f64::from(t.clamp(-1.0, 1.0));
                if t == 0.0 {
                    x
                } else if t < 0.0 {
                    // Exponential: steeper the nearer to -1.
                    let k = -t * EXPONENTIAL_STEEPNESS;
                    (k * x).exp_m1() / k.exp_m1()
                } else {
                    // A blend from the line to a smoothstep.
                    x + t * (x * x * (3.0 - 2.0 * x) - x)
                }
            }
        }
    }

    /// The tension, with `Linear` as zero, or `None` for `Hold`.
    pub fn tension(self) -> Option<f32> {
        match self {
            Curve::Linear => Some(0.0),
            Curve::Tension(t) => Some(t),
            Curve::Hold => None,
        }
    }

    /// A curve of this tension: `Linear` at zero.
    pub fn with_tension(tension: f32) -> Self {
        if tension == 0.0 {
            Curve::Linear
        } else {
            Curve::Tension(tension.clamp(-1.0, 1.0))
        }
    }
}

/// The exponent at tension -1: how much faster the end is than the start.
const EXPONENTIAL_STEEPNESS: f64 = 6.0;

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct AutomationPoint {
    pub tick: Tick,
    pub value: f32,
    /// How the value moves on to the next point.
    #[serde(default)]
    pub curve: Curve,
}

/// A parameter's value over time, as points in order. Before the first point
/// it holds the first value, and after the last it holds the last.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AutomationLane {
    /// The input port it drives, by the port's key.
    pub target: Endpoint,
    pub points: Vec<AutomationPoint>,
}

impl AutomationLane {
    pub fn new(target: Endpoint, points: Vec<AutomationPoint>) -> Self {
        Self { target, points }
    }

    /// The value at `tick`, a fraction of a tick included. `None` if the lane
    /// has no points, and so doesn't drive anything.
    pub fn value_at(&self, tick: f64) -> Option<f32> {
        let after = self.points.partition_point(|p| p.tick.0 as f64 <= tick);
        let Some(before) = after.checked_sub(1).map(|i| &self.points[i]) else {
            return self.points.first().map(|p| p.value);
        };
        let Some(next) = self.points.get(after) else {
            return Some(before.value);
        };
        Some(match before.curve {
            Curve::Hold => before.value,
            curve => {
                let span = (next.tick.0 - before.tick.0) as f64;
                let along = curve.shape((tick - before.tick.0 as f64) / span);
                before.value + (next.value - before.value) * along as f32
            }
        })
    }

    /// Why this lane can't be in a project, if it can't.
    pub(crate) fn problem(&self) -> Option<&'static str> {
        if self.points.iter().any(|p| p.tick < Tick::ZERO) {
            Some("a point is before the beginning")
        } else if self.points.iter().any(|p| !p.value.is_finite()) {
            Some("a point's value is not a number")
        } else if self
            .points
            .windows(2)
            .any(|pair| pair[0].tick >= pair[1].tick)
        {
            Some("its points are not in order, or two share a tick")
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NodeId;

    fn point(tick: i64, value: f32, curve: Curve) -> AutomationPoint {
        AutomationPoint {
            tick: Tick(tick),
            value,
            curve,
        }
    }

    fn lane(points: Vec<AutomationPoint>) -> AutomationLane {
        AutomationLane::new(Endpoint::new(NodeId(1), "cutoff"), points)
    }

    #[test]
    fn holds_before_the_first_point_and_after_the_last() {
        let lane = lane(vec![
            point(100, 1.0, Curve::Linear),
            point(200, 3.0, Curve::Linear),
        ]);
        assert_eq!(lane.value_at(0.0), Some(1.0));
        assert_eq!(lane.value_at(100.0), Some(1.0));
        assert_eq!(lane.value_at(200.0), Some(3.0));
        assert_eq!(lane.value_at(5000.0), Some(3.0));
    }

    #[test]
    fn linear_points_slope_to_the_next() {
        let lane = lane(vec![
            point(0, 0.0, Curve::Linear),
            point(100, 10.0, Curve::Linear),
            point(200, 0.0, Curve::Linear),
        ]);
        assert_eq!(lane.value_at(50.0), Some(5.0));
        assert_eq!(lane.value_at(25.5), Some(2.55));
        assert_eq!(lane.value_at(150.0), Some(5.0));
    }

    #[test]
    fn hold_points_stay_put_until_the_next() {
        let lane = lane(vec![
            point(0, 1.0, Curve::Hold),
            point(100, 2.0, Curve::Linear),
            point(200, 4.0, Curve::Linear),
        ]);
        assert_eq!(lane.value_at(99.9), Some(1.0));
        assert_eq!(lane.value_at(100.0), Some(2.0));
        assert_eq!(lane.value_at(150.0), Some(3.0));
    }

    #[test]
    fn tension_curves_start_and_end_on_their_points() {
        for t in [-1.0, -0.5, 0.0, 0.3, 1.0] {
            let curve = Curve::with_tension(t);
            assert_eq!(curve.shape(0.0), 0.0, "tension {t}");
            assert_eq!(curve.shape(1.0), 1.0, "tension {t}");
        }
    }

    #[test]
    fn tension_curves_never_double_back() {
        for t in [-1.0, -0.7, -0.1, 0.1, 0.6, 1.0] {
            let curve = Curve::Tension(t);
            let mut last = 0.0;
            for i in 1..=1000 {
                let y = curve.shape(f64::from(i) / 1000.0);
                assert!(y >= last, "tension {t} fell at step {i}");
                last = y;
            }
        }
    }

    #[test]
    fn zero_tension_is_the_straight_line() {
        assert_eq!(Curve::with_tension(0.0), Curve::Linear);
        for i in 0..=10 {
            let x = f64::from(i) / 10.0;
            assert_eq!(Curve::Tension(0.0).shape(x), x);
            assert_eq!(Curve::Linear.shape(x), x);
        }
    }

    #[test]
    fn negative_tension_starts_slow_and_positive_leans_into_an_s() {
        assert!(Curve::Tension(-0.8).shape(0.5) < 0.5);
        assert!(Curve::Tension(0.8).shape(0.25) < 0.25);
        assert!(Curve::Tension(0.8).shape(0.75) > 0.75);
        assert_eq!(Curve::Tension(1.0).shape(0.5), 0.5);
    }

    #[test]
    fn a_lane_follows_its_segments_tension() {
        let lane = lane(vec![
            point(0, 0.0, Curve::Tension(-1.0)),
            point(100, 10.0, Curve::Linear),
        ]);
        let mid = lane.value_at(50.0).unwrap();
        assert!(mid < 1.0, "an exponential segment is still low at half");
        assert_eq!(lane.value_at(100.0), Some(10.0));
    }

    #[test]
    fn an_empty_lane_has_no_value() {
        assert_eq!(lane(vec![]).value_at(10.0), None);
    }

    #[test]
    fn points_must_be_in_order_and_numbers() {
        let ok = lane(vec![
            point(0, 0.0, Curve::Linear),
            point(10, 1.0, Curve::Linear),
        ]);
        assert_eq!(ok.problem(), None);
        let unordered = lane(vec![
            point(10, 0.0, Curve::Linear),
            point(0, 1.0, Curve::Linear),
        ]);
        assert!(unordered.problem().is_some());
        let twice = lane(vec![
            point(10, 0.0, Curve::Linear),
            point(10, 1.0, Curve::Linear),
        ]);
        assert!(twice.problem().is_some());
        assert!(
            lane(vec![point(0, f32::NAN, Curve::Linear)])
                .problem()
                .is_some()
        );
        assert!(
            lane(vec![point(-1, 0.0, Curve::Linear)])
                .problem()
                .is_some()
        );
    }
}
