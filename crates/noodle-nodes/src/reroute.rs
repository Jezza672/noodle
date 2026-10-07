use noodle_engine::{
    Config, Context, Instance, Lane, LaneKernel, Layout, NodeError, NodeInfo, NodeType, PerLane,
    Setup,
};

/// A point a wire passes through, so it can be routed around other nodes, or
/// fan out from one place. Passes its input through unchanged.
pub struct Reroute;

pub const REROUTE_ID: &str = "noodle.util.reroute";

const IN: usize = 0;
const OUT: usize = 0;

static INFO: NodeInfo = NodeInfo {
    id: REROUTE_ID,
    version: 1,
    name: "Reroute",
    category: "Layout",
};

impl NodeType for Reroute {
    fn info(&self) -> &NodeInfo {
        &INFO
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(Layout::realtime().input("in", "In").output("out", "Out"))
    }

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        Ok(Instance::realtime(PerLane::new(RerouteKernel, setup)))
    }
}

struct RerouteKernel;

impl LaneKernel for RerouteKernel {
    type State = ();

    fn process_lane(&mut self, _: &mut (), _: &Context, mut lane: Lane<'_, '_>) {
        let input = lane.inputs.get(IN);
        lane.outputs.get_mut(OUT).copy_from_slice(input);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_engine::Shape;
    use noodle_engine::testing::Harness;

    #[test]
    fn passes_its_input_through() {
        let mut h = Harness::new(
            &Reroute,
            &Config::new(),
            &[(IN, Shape::STEREO)],
            48_000.0,
            4,
        )
        .unwrap();
        h.input(IN, 4)
            .lane_mut(0, 0)
            .copy_from_slice(&[0.1, -0.2, 0.3, -0.4]);
        h.input(IN, 4)
            .lane_mut(0, 1)
            .copy_from_slice(&[1.0, 2.0, 3.0, 4.0]);
        h.run(4).unwrap();

        let out = h.output(OUT);
        assert_eq!(out.shape(), Shape::STEREO);
        assert_eq!(out.lane(0, 0), &[0.1, -0.2, 0.3, -0.4]);
        assert_eq!(out.lane(0, 1), &[1.0, 2.0, 3.0, 4.0]);
    }
}
