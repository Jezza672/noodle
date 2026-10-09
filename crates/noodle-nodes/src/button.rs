use noodle_engine::{
    Config, Context, Instance, Lane, LaneKernel, Layout, NodeError, NodeInfo, NodeType, ParamInfo,
    PerLane, Setup,
};

/// A switch that the transport bar can press, so a UI control is a node in the
/// graph like any other. It outputs its state: 0 when off, 1 when on.
///
/// The state is a parameter, so pressing the button is an ordinary undoable
/// edit, and a lane or another node could drive it too.
pub struct Button;

pub const BUTTON_ID: &str = "noodle.input.button";

/// The `state` parameter's key, for the UI that presses the button.
pub const BUTTON_STATE: &str = "state";

const STATE: usize = 0;
const OUT: usize = 0;

static INFO: NodeInfo = NodeInfo {
    id: BUTTON_ID,
    version: 1,
    name: "Button",
    category: "Input/Output",
};

impl NodeType for Button {
    fn info(&self) -> &NodeInfo {
        &INFO
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(Layout::realtime()
            .param(BUTTON_STATE, "State", ParamInfo::choice(["Off", "On"]))
            .output("out", "Out"))
    }

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        Ok(Instance::realtime(PerLane::new(ButtonKernel, setup)))
    }
}

struct ButtonKernel;

impl LaneKernel for ButtonKernel {
    type State = ();

    fn process_lane(&mut self, _: &mut (), _: &Context, mut lane: Lane<'_, '_>) {
        let state = lane.inputs.get(STATE);
        for (o, s) in lane.outputs.get_mut(OUT).iter_mut().zip(state) {
            *o = if *s >= 0.5 { 1.0 } else { 0.0 };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_engine::Shape;
    use noodle_engine::testing::Harness;

    #[test]
    fn outputs_its_state() {
        let mut h = Harness::new(&Button, &Config::new(), &[], 48_000.0, 4).unwrap();
        h.run(4).unwrap();
        assert!(h.output(OUT).lane(0, 0).iter().all(|&x| x == 0.0));
        h.set(STATE, 1.0);
        h.run(4).unwrap();
        assert_eq!(h.output(OUT).shape(), Shape::MONO);
        assert!(h.output(OUT).lane(0, 0).iter().all(|&x| x == 1.0));
    }
}
