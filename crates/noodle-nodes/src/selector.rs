//! The Selector node.

use noodle_engine::{
    Config, ConfigInfo, Context, Instance, Lane, LaneKernel, Layout, NodeError, NodeInfo, NodeType,
    ParamInfo, PerLane, Setup, Skip,
};

/// Routes one of its inputs to its output, chosen by `which`: a deterministic
/// A/B (or N-way) selector, where [`RandomSelect`](crate::RandomSelect) picks
/// at random on a trigger.
///
/// **Inputs.** How many (`inputs`, 2 to 64) is config, because it changes the
/// ports. The audio and control inputs are `in1`…`inN`.
///
/// **Choosing.** `which` is a stepped parameter (`In 1`…`In N`), numbered from
/// 0 internally. Its value is rounded and clamped to an input, so automation
/// and wires can choose one; a wire changes the choice per sample.
///
/// **Switching.** Instant: the new input's samples start at the sample the
/// choice changes on, so switching between unrelated signals can click. There
/// is no zero-crossing option yet, and notes, samples and spectra are not
/// accepted yet.
///
/// **Shapes.** The output broadcasts the inputs' shapes, so a mono input plays
/// on every voice and channel the other inputs set. An input with nothing
/// wired is silent, so a half-patched selector still passes whichever input is
/// selected.
pub struct Selector;

/// The Selector's type ID.
pub const SELECTOR_ID: &str = "noodle.util.selector";

const INPUTS: ConfigInfo = ConfigInfo::int("inputs", "Inputs", 2);
const MIN_INPUTS: i64 = 2;
const MAX_INPUTS: i64 = 64;
const OUT: usize = 0;
const WHICH: &str = "which";

static CONFIG: [ConfigInfo; 1] = [INPUTS];

static INFO: NodeInfo = NodeInfo {
    id: SELECTOR_ID,
    version: 1,
    name: "Selector",
    category: "Utilities",
};

impl NodeType for Selector {
    fn info(&self) -> &NodeInfo {
        &INFO
    }

    fn config(&self) -> &[ConfigInfo] {
        &CONFIG
    }

    fn layout(&self, config: &Config) -> Result<Layout, NodeError> {
        let inputs = INPUTS.get_int(config);
        if !(MIN_INPUTS..=MAX_INPUTS).contains(&inputs) {
            return Err(NodeError::config(format!(
                "Selector needs between {MIN_INPUTS} and {MAX_INPUTS} inputs, not {inputs}"
            )));
        }
        let layout = (1..=inputs).fold(Layout::realtime(), |layout, i| {
            layout.input(format!("in{i}"), format!("In {i}"))
        });
        Ok(layout
            .param(
                WHICH,
                "Which",
                ParamInfo::choice((1..=inputs).map(|i| format!("In {i}"))),
            )
            .output("out", "Out")
            .memoryless())
    }

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        let inputs = INPUTS.get_int(setup.config).clamp(MIN_INPUTS, MAX_INPUTS) as usize;
        Ok(Instance::realtime(PerLane::new(
            SelectorKernel { inputs },
            setup,
        )))
    }
}

struct SelectorKernel {
    inputs: usize,
}

impl LaneKernel for SelectorKernel {
    type State = ();

    fn skip(&self) -> Skip {
        Skip::AllSilentBelow(self.inputs)
    }

    fn process_lane(&mut self, _: &mut (), _: &Context, mut lane: Lane<'_, '_>) {
        let inputs = self.inputs;
        let out = lane.outputs.get_mut(OUT);
        // An unconnected parameter is a constant, so the usual case resolves
        // the choice once per block.
        if let Some(which) = lane.inputs.constant(inputs) {
            let chosen = chosen_input(which, inputs);
            if lane.inputs.is_silent(chosen) {
                out.fill(0.0);
            } else {
                out.copy_from_slice(lane.inputs.get(chosen));
            }
            return;
        }
        let which = lane.inputs.get(inputs);
        for (i, o) in out.iter_mut().enumerate() {
            let chosen = chosen_input(which[i], inputs);
            *o = if lane.inputs.is_silent(chosen) {
                0.0
            } else {
                lane.inputs.get(chosen)[i]
            };
        }
    }
}

/// The input a `which` value chooses: rounded, clamped to `0..inputs`, and
/// input 1 for a value that isn't a number.
fn chosen_input(which: f32, inputs: usize) -> usize {
    if which.is_finite() {
        which.round().clamp(0.0, (inputs - 1) as f32) as usize
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_engine::testing::Harness;
    use noodle_engine::{InputKind, Memory, ParamKind, Shape, Value};

    const FRAMES: usize = 8;

    const MONO: Shape = Shape::MONO;

    fn config(inputs: i64) -> Config {
        Config::new().with("inputs", Value::Int(inputs))
    }

    /// A selector with `inputs` audio inputs, all wired mono, and an
    /// unconnected `which` (so `set` can choose).
    fn wired(inputs: i64) -> Harness {
        let connected: Vec<_> = (0..inputs as usize).map(|i| (i, MONO)).collect();
        Harness::new(&Selector, &config(inputs), &connected, 48_000.0, FRAMES).unwrap()
    }

    /// Fills each input with its 1-based number, so the output says which was
    /// chosen.
    fn numbered(h: &mut Harness, inputs: usize) {
        for i in 0..inputs {
            h.input(i, FRAMES).fill((i + 1) as f32);
        }
    }

    #[test]
    fn selects_the_input_which_names() {
        let mut h = wired(3);
        numbered(&mut h, 3);
        for (which, want) in [(0.0, 1.0), (1.0, 2.0), (2.0, 3.0)] {
            h.set(3, which);
            h.run(FRAMES).unwrap();
            assert_eq!(h.output(OUT).lane(0, 0), &[want; FRAMES]);
        }
    }

    #[test]
    fn which_rounds_and_clamps_to_an_input() {
        let mut h = wired(3);
        numbered(&mut h, 3);
        for (which, want) in [
            (-5.0, 1.0),
            (0.4, 1.0),
            (0.5, 2.0),
            (1.49, 2.0),
            (1.5, 3.0),
            (99.0, 3.0),
            (f32::NAN, 1.0),
        ] {
            h.set(3, which);
            h.run(FRAMES).unwrap();
            assert_eq!(h.output(OUT).lane(0, 0), &[want; FRAMES], "which {which}");
        }
    }

    #[test]
    fn a_wired_which_selects_each_sample() {
        let connected = [(0, MONO), (1, MONO), (2, MONO)];
        let mut h = Harness::new(&Selector, &config(2), &connected, 48_000.0, FRAMES).unwrap();
        h.input(0, 4).fill(1.0);
        h.input(1, 4).fill(2.0);
        // The last value rounds to input 2 and the last clamps to it.
        h.input(2, 4)
            .lane_mut(0, 0)
            .copy_from_slice(&[0.0, 1.0, 1.4, 5.0]);
        h.run(4).unwrap();
        assert_eq!(h.output(OUT).lane(0, 0), &[1.0, 2.0, 2.0, 2.0]);
    }

    #[test]
    fn a_mono_input_broadcasts_across_the_output_shape() {
        let poly = Shape::new(3, 2);
        let connected = [(0, poly), (1, MONO)];
        let mut h = Harness::new(&Selector, &config(2), &connected, 48_000.0, FRAMES).unwrap();
        let mut input = h.input(0, FRAMES);
        for voice in 0..3 {
            for channel in 0..2 {
                input
                    .lane_mut(voice, channel)
                    .fill((voice * 2 + channel + 1) as f32);
            }
        }
        h.input(1, FRAMES).fill(-1.0);

        h.set(2, 0.0);
        h.run(FRAMES).unwrap();
        assert_eq!(h.output(OUT).shape(), poly);
        assert_eq!(h.output(OUT).lane(2, 1), &[6.0; FRAMES]);

        // A mono input plays on every voice and channel.
        h.set(2, 1.0);
        h.run(FRAMES).unwrap();
        assert_eq!(h.output(OUT).shape(), poly);
        assert_eq!(h.output(OUT).lane(2, 1), &[-1.0; FRAMES]);
    }

    #[test]
    fn an_unconnected_selected_input_is_silent() {
        // Input 2 is configured but nothing is wired to it.
        let connected = [(0, MONO), (2, MONO)];
        let mut h = Harness::new(&Selector, &config(3), &connected, 48_000.0, FRAMES).unwrap();
        h.input(0, FRAMES).fill(1.0);
        h.input(2, FRAMES).fill(3.0);

        h.set(3, 1.0);
        h.run(FRAMES).unwrap();
        assert!(h.output(OUT).lane(0, 0).iter().all(|&x| x == 0.0));

        // The other inputs still pass while selected.
        h.set(3, 2.0);
        h.run(FRAMES).unwrap();
        assert_eq!(h.output(OUT).lane(0, 0), &[3.0; FRAMES]);
        h.set(3, 0.0);
        h.run(FRAMES).unwrap();
        assert_eq!(h.output(OUT).lane(0, 0), &[1.0; FRAMES]);
    }

    #[test]
    fn the_default_is_two_named_inputs() {
        let layout = Selector.layout(&Config::new()).unwrap();
        assert_eq!(layout.inputs.len(), 3, "in1, in2 and which");
        assert_eq!(layout.outputs.len(), 1);
        assert_eq!(layout.memory, Memory::None);
        let which = layout.inputs.iter().find(|port| port.key == WHICH).unwrap();
        let InputKind::Param(info) = &which.kind else {
            panic!("which should be a parameter");
        };
        assert_eq!(info.max, 1.0);
        let ParamKind::Stepped { labels } = &info.kind else {
            panic!("which should be stepped");
        };
        assert_eq!(labels.as_slice(), ["In 1", "In 2"]);
    }

    #[test]
    fn rejects_a_bad_input_count() {
        assert!(matches!(
            Selector.layout(&config(1)),
            Err(NodeError::Config(_))
        ));
        assert!(matches!(
            Selector.layout(&config(65)),
            Err(NodeError::Config(_))
        ));
    }
}
