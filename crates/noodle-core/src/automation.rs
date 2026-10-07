//! Automation lanes: a parameter's value over the timeline. The compiler
//! turns each lane into a source wired into the parameter's port, so a lane
//! modulates what a wire does. See "Automation" in docs/ARCHITECTURE.md.

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
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Curve {
    /// A straight line.
    #[default]
    Linear,
    /// Stays at the point's value until the next point, then jumps.
    Hold,
}

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
            Curve::Linear => {
                let span = (next.tick.0 - before.tick.0) as f64;
                let along = (tick - before.tick.0 as f64) / span;
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
