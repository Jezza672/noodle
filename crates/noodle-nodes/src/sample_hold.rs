//! The Sample & Hold node.

use noodle_engine::{
    Config, Context, Instance, Lane, LaneKernel, Layout, NodeError, NodeInfo, NodeType, ParamInfo,
    PerLane, Ports, Setup,
};

/// Captures its input at an instant and holds that value until the next one.
/// The instant is a rising edge through 0.5 on `trigger`: a clock, a key's
/// gate, a comparator, an LFO's square wave.
///
/// Wire a noise source into `in` and a clock into `trigger` for steps of
/// random values; wire an LFO into `in` for a staircase that follows it.
///
/// In `Sample` mode the output holds between triggers. In `Track` mode (a
/// track and hold) it follows the input while the trigger is high and holds
/// the last value once it falls. The output starts at 0.
///
/// Every voice holds its own value, so a polyphonic trigger gives a
/// polyphonic hold.
pub struct SampleHold;

pub const SAMPLE_HOLD_ID: &str = "noodle.mod.samplehold";

#[derive(Ports)]
struct SampleHoldPorts {
    #[input("in", "In")]
    input: (),
    #[input("trigger", "Trigger")]
    trigger: (),
    #[param("mode", "Mode", ParamInfo::choice(["Sample", "Track"]))]
    mode: (),
    #[output("out", "Out")]
    out: (),
}

const IN: usize = SampleHoldPorts::INPUT;
const TRIGGER: usize = SampleHoldPorts::TRIGGER;
const MODE: usize = SampleHoldPorts::MODE;
const OUT: usize = SampleHoldPorts::OUT;

static INFO: NodeInfo = NodeInfo {
    id: SAMPLE_HOLD_ID,
    version: 1,
    name: "Sample & Hold",
    category: "Modulation",
};

impl NodeType for SampleHold {
    fn info(&self) -> &NodeInfo {
        &INFO
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(SampleHoldPorts::layout())
    }

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        Ok(Instance::realtime(PerLane::new(HoldKernel, setup)))
    }
}

struct HoldKernel;

#[derive(Default)]
struct HoldState {
    held: f32,
    gate: bool,
}

impl LaneKernel for HoldKernel {
    type State = HoldState;

    fn process_lane(&mut self, state: &mut HoldState, _: &Context, mut lane: Lane<'_, '_>) {
        let (input, trigger) = (lane.inputs.get(IN), lane.inputs.get(TRIGGER));
        let mode = lane.inputs.get(MODE);
        let out = lane.outputs.get_mut(OUT);
        for i in 0..out.len() {
            let high = trigger[i] >= 0.5;
            let track = mode[i] >= 0.5;
            if (high && !state.gate) || (track && high) {
                state.held = input[i];
            }
            state.gate = high;
            out[i] = state.held;
        }
        if !state.held.is_finite() {
            state.held = 0.0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_engine::Shape;
    use noodle_engine::testing::Harness;

    fn run(input: &[f32], trigger: &[f32], mode: f32) -> Vec<f32> {
        let frames = input.len();
        let mut h = Harness::new(
            &SampleHold,
            &Config::new(),
            &[(IN, Shape::MONO), (TRIGGER, Shape::MONO)],
            48_000.0,
            frames,
        )
        .unwrap();
        h.set(MODE, mode);
        h.input(IN, frames).lane_mut(0, 0).copy_from_slice(input);
        h.input(TRIGGER, frames)
            .lane_mut(0, 0)
            .copy_from_slice(trigger);
        h.run(frames).unwrap();
        h.output(OUT).lane(0, 0).to_vec()
    }

    #[test]
    fn captures_on_the_rising_edge_and_holds() {
        let input: Vec<f32> = (0..10).map(|i| i as f32).collect();
        let trigger = [0.0, 0.0, 1.0, 1.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0];
        let out = run(&input, &trigger, 0.0);
        assert_eq!(out, [0.0, 0.0, 2.0, 2.0, 2.0, 2.0, 6.0, 6.0, 6.0, 6.0]);
    }

    #[test]
    fn track_follows_while_high_and_holds_after() {
        let input: Vec<f32> = (0..8).map(|i| i as f32).collect();
        let trigger = [0.0, 1.0, 1.0, 1.0, 0.0, 0.0, 0.0, 0.0];
        let out = run(&input, &trigger, 1.0);
        assert_eq!(out, [0.0, 1.0, 2.0, 3.0, 3.0, 3.0, 3.0, 3.0]);
    }

    #[test]
    fn the_edge_is_found_across_blocks() {
        let mut h = Harness::new(
            &SampleHold,
            &Config::new(),
            &[(IN, Shape::MONO), (TRIGGER, Shape::MONO)],
            48_000.0,
            4,
        )
        .unwrap();
        for (value, gate) in [(1.0, 1.0), (2.0, 1.0), (3.0, 0.0), (4.0, 1.0)] {
            h.input(IN, 4).fill(value);
            h.input(TRIGGER, 4).fill(gate);
            h.run(4).unwrap();
        }
        // Blocks 1 and 2 hold a gate that never fell: one edge, at value 1.
        // The gate fell in block 3 and rose again in block 4.
        assert_eq!(h.output(OUT).lane(0, 0), &[4.0; 4]);
    }

    #[test]
    fn a_held_gate_does_not_resample() {
        let input: Vec<f32> = (0..6).map(|i| i as f32 + 1.0).collect();
        let out = run(&input, &[1.0; 6], 0.0);
        assert_eq!(out, [1.0; 6]);
    }
}
