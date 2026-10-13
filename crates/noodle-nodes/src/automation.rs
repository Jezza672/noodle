//! The Automation node.

use noodle_engine::{
    Config, Context, Instance, Lane, LaneKernel, Layout, NodeError, NodeInfo, NodeType, ParamInfo,
    PerLane, Ports, Setup,
};

/// A control signal you draw on the timeline: an automation track.
///
/// It outputs `value`, a number from 0 to 1, and an automation lane on
/// `value` (made by the timeline's "+ Add automation" button, which adds
/// this node and the lane together) moves it along the timeline, with points
/// you click in and drag. Wire `out` into any input: a parameter (where it
/// sweeps the slider's travel, or replaces the value, as the parameter
/// does for any wire), a gate, a mixer's level. Several wires may leave it.
///
/// Without a lane it is a plain 0 to 1 knob, which makes it a handy macro
/// control for one move that several parameters follow.
///
/// This is how an automation track reaches things that have no lane of their
/// own: a lane drives the one parameter it is made for, and this node turns
/// it into a signal any number of inputs can use.
pub struct Automation;

pub const AUTOMATION_NODE_ID: &str = "noodle.mod.automation";

/// The key of the parameter the lane automates.
pub const AUTOMATION_VALUE: &str = "value";

#[derive(Ports)]
struct AutomationPorts {
    #[param("value", "Value", ParamInfo::new(0.0, 1.0, 0.0))]
    value: (),
    #[output("out", "Out")]
    out: (),
}

const VALUE: usize = AutomationPorts::VALUE;
const OUT: usize = AutomationPorts::OUT;

static INFO: NodeInfo = NodeInfo {
    id: AUTOMATION_NODE_ID,
    version: 1,
    name: "Automation Track",
    category: "Modulation",
};

impl NodeType for Automation {
    fn info(&self) -> &NodeInfo {
        &INFO
    }

    /// It holds nothing between blocks.
    fn stateless(&self) -> bool {
        true
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(AutomationPorts::layout())
    }

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        Ok(Instance::realtime(PerLane::new(AutomationKernel, setup)))
    }
}

struct AutomationKernel;

impl LaneKernel for AutomationKernel {
    type State = ();

    fn process_lane(&mut self, _: &mut (), _: &Context, mut lane: Lane<'_, '_>) {
        let value = lane.inputs.get(VALUE);
        let out = lane.outputs.get_mut(OUT);
        for (o, &v) in out.iter_mut().zip(value) {
            *o = if v.is_finite() {
                v.clamp(0.0, 1.0)
            } else {
                0.0
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_engine::Shape;
    use noodle_engine::testing::Harness;

    #[test]
    fn passes_its_value_through_held_to_0_to_1() {
        let mut h = Harness::new(&Automation, &Config::new(), &[], 48_000.0, 64).unwrap();
        for (set, expect) in [(0.3, 0.3), (-4.0, 0.0), (9.0, 1.0), (f32::NAN, 0.0)] {
            h.set(VALUE, set);
            h.run(64).unwrap();
            assert!(h.output(OUT).lane(0, 0).iter().all(|&x| x == expect));
        }
    }

    #[test]
    fn follows_a_wired_value_sample_by_sample() {
        let mut h = Harness::new(
            &Automation,
            &Config::new(),
            &[(VALUE, Shape::MONO)],
            48_000.0,
            4,
        )
        .unwrap();
        h.input(VALUE, 4)
            .lane_mut(0, 0)
            .copy_from_slice(&[0.0, 0.25, 0.5, 2.0]);
        h.run(4).unwrap();
        assert_eq!(h.output(OUT).lane(0, 0), &[0.0, 0.25, 0.5, 1.0]);
    }
}
