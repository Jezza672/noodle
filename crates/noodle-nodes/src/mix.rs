use noodle_engine::{
    Config, ConfigInfo, Context, Instance, Lane, LaneKernel, Layout, NodeError, NodeInfo, NodeType,
    PerLane, Setup,
};

/// Sums its inputs. How many inputs it has is config rather than a parameter,
/// because it changes the node's ports.
pub struct Mix;

const INPUTS: ConfigInfo = ConfigInfo::int("inputs", "Inputs", 2);
const MAX_INPUTS: i64 = 64;
const OUT: usize = 0;

static CONFIG: [ConfigInfo; 1] = [INPUTS];

static INFO: NodeInfo = NodeInfo {
    id: "noodle.util.mix",
    version: 1,
    name: "Mix",
    category: "Utilities",
};

impl NodeType for Mix {
    fn info(&self) -> &NodeInfo {
        &INFO
    }

    fn config(&self) -> &[ConfigInfo] {
        &CONFIG
    }

    fn layout(&self, config: &Config) -> Result<Layout, NodeError> {
        let inputs = INPUTS.get_int(config);
        if !(1..=MAX_INPUTS).contains(&inputs) {
            return Err(NodeError::config(format!(
                "Mix needs between 1 and {MAX_INPUTS} inputs, not {inputs}"
            )));
        }
        let layout = (1..=inputs).fold(Layout::realtime(), |layout, i| {
            layout.input(format!("in{i}"), format!("In {i}"))
        });
        Ok(layout.output("out", "Out"))
    }

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        let kernel = MixKernel {
            inputs: setup.input_shapes.len(),
        };
        Ok(Instance::realtime(PerLane::new(kernel, setup)))
    }
}

struct MixKernel {
    inputs: usize,
}

impl LaneKernel for MixKernel {
    type State = ();

    fn process_lane(&mut self, _: &mut (), _: &Context, mut lane: Lane<'_, '_>) {
        let out = lane.outputs.get_mut(OUT);
        out.copy_from_slice(lane.inputs.get(0));
        for port in 1..self.inputs {
            for (o, x) in out.iter_mut().zip(lane.inputs.get(port)) {
                *o += x;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_engine::testing::Harness;
    use noodle_engine::{Shape, Value};

    #[test]
    fn sums_a_configured_number_of_inputs() {
        let config = Config::new().with("inputs", Value::Int(3));
        let connected = [(0, Shape::MONO), (1, Shape::MONO), (2, Shape::MONO)];
        let mut h = Harness::new(&Mix, &config, &connected, 48_000.0, 4).unwrap();
        for (port, value) in [(0, 1.0), (1, 2.0), (2, 3.0)] {
            h.input(port, 4).fill(value);
        }
        h.run(4).unwrap();
        assert_eq!(h.output(OUT).lane(0, 0), &[6.0; 4]);
    }

    #[test]
    fn defaults_to_two_inputs() {
        assert_eq!(Mix.layout(&Config::new()).unwrap().inputs.len(), 2);
    }

    #[test]
    fn rejects_zero_inputs() {
        let config = Config::new().with("inputs", Value::Int(0));
        assert!(matches!(Mix.layout(&config), Err(NodeError::Config(_))));
    }
}
