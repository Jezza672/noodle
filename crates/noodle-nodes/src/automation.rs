//! The Automation node.

use noodle_engine::{
    Config, ConfigInfo, Context, Instance, Lane, LaneKernel, Layout, NodeError, NodeInfo, NodeType,
    ParamInfo, PerLane, Ports, Setup,
};

/// A control signal you draw on the timeline: an automation track.
///
/// It outputs `value`, a number from 0 to 1 mapped onto `min` to `max` (the
/// range of the parameter it was made for, by the Automate shortcut; the
/// default 0 to 1 passes it through), along a linear or log taper, and an
/// automation lane on `value` (made by the timeline's "+ Add automation" button, which adds
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
#[ports(memoryless)]
struct AutomationPorts {
    #[param("value", "Value", ParamInfo::new(0.0, 1.0, 0.0))]
    value: (),
    #[param("min", "Min", ParamInfo::new(-RANGE_LIMIT, RANGE_LIMIT, 0.0))]
    min: (),
    #[param("max", "Max", ParamInfo::new(-RANGE_LIMIT, RANGE_LIMIT, 1.0))]
    max: (),
    #[param("taper", "Taper", ParamInfo::choice(["Linear", "Log"]))]
    taper: (),
    /// Wire a signal here and, while the node is armed, it is written into
    /// the lane (0 to 1, like `value`). The node itself ignores it.
    #[param("record", "Record", ParamInfo::new(0.0, 1.0, 0.0))]
    record: (),
    #[control_output("out", "Out")]
    out: (),
}

/// How far `min` and `max` can reach, in either direction.
pub const RANGE_LIMIT: f32 = 1.0e6;

/// The keys of the range parameters and the taper.
pub const AUTOMATION_MIN: &str = "min";
pub const AUTOMATION_MAX: &str = "max";
pub const AUTOMATION_TAPER: &str = "taper";
/// The input an armed node records a wired signal from.
pub const AUTOMATION_RECORD: &str = "record";
/// The config key of the node's name, shown on its lane, copied from the
/// parameter the Automate shortcut was used on. Empty or missing means none.
pub const AUTOMATION_LABEL: &str = "label";

static LABEL: ConfigInfo = ConfigInfo::text(AUTOMATION_LABEL, "Label");

const VALUE: usize = AutomationPorts::VALUE;
const MIN: usize = AutomationPorts::MIN;
const MAX: usize = AutomationPorts::MAX;
const TAPER: usize = AutomationPorts::TAPER;
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

    fn config(&self) -> &[ConfigInfo] {
        std::slice::from_ref(&LABEL)
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
        let (min, max) = (lane.inputs.get(MIN), lane.inputs.get(MAX));
        let log = lane.inputs.get(TAPER);
        let out = lane.outputs.get_mut(OUT);
        for (i, (o, &v)) in out.iter_mut().zip(value).enumerate() {
            let v = if v.is_finite() {
                v.clamp(0.0, 1.0)
            } else {
                0.0
            };
            let (lo, hi) = (min[i], max[i]);
            *o = if log[i] >= 0.5 && lo > 0.0 && hi > 0.0 {
                lo * (hi / lo).powf(v)
            } else {
                lo + (hi - lo) * v
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
    fn maps_onto_its_range_on_a_linear_or_log_taper() {
        let mut h = Harness::new(&Automation, &Config::new(), &[], 48_000.0, 64).unwrap();
        h.set(MIN, 20.0);
        h.set(MAX, 20_000.0);
        h.set(VALUE, 0.0);
        h.run(64).unwrap();
        assert_eq!(h.output(OUT).lane(0, 0)[63], 20.0);
        h.set(VALUE, 1.0);
        h.run(64).unwrap();
        assert_eq!(h.output(OUT).lane(0, 0)[63], 20_000.0);
        h.set(VALUE, 0.5);
        h.run(64).unwrap();
        assert_eq!(h.output(OUT).lane(0, 0)[63], 10_010.0);
        h.set(TAPER, 1.0);
        h.run(64).unwrap();
        let mid = h.output(OUT).lane(0, 0)[63];
        assert!(
            (mid - 632.455_5).abs() < 0.01,
            "geometric middle, got {mid}"
        );
    }

    #[test]
    fn a_log_taper_over_a_range_through_zero_falls_back_to_linear() {
        let mut h = Harness::new(&Automation, &Config::new(), &[], 48_000.0, 64).unwrap();
        h.set(MIN, -60.0);
        h.set(MAX, 12.0);
        h.set(TAPER, 1.0);
        h.set(VALUE, 0.5);
        h.run(64).unwrap();
        assert_eq!(h.output(OUT).lane(0, 0)[63], -24.0);
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
