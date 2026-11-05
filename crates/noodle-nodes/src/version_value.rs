//! The Version Value node.

use noodle_core::{VERSION_COUNT_KEY, VERSION_VALUE_ID, version_slot_key};
use noodle_engine::{
    Config, ConfigInfo, Context, Instance, Io, Layout, Node, NodeError, NodeInfo, NodeType,
    ParamInfo, Setup,
};

/// One value per mix version: it outputs the slot of the version that plays,
/// so a parameter can differ between versions while everything else is
/// shared. "Vary per version" on a parameter adds one, wired to it.
///
/// **Slots.** `versions` (config, 1 to 16) is how many slots there are, one
/// parameter each (`v1`, `v2`, ...), as the project's version list is long.
/// The project keeps the two equal: adding a version adds a slot to every
/// Version Value node, holding the value the version that plays has.
///
/// **Range.** A slot is a position from 0 to 1 along `min` to `max` (`min` to
/// `max` on a log taper), so a slider is easy to drag for any parameter: the
/// gesture sets `min`, `max` and `taper` to the parameter's own. For a
/// parameter a wire *offsets* (a filter's cutoff), the range is -1 to 1 along
/// its travel and 0.5 is no move.
///
/// **Switching.** The version comes from the transport, so switching it while
/// playing rebuilds nothing. The output glides to the new slot over about 15
/// ms, so a switch doesn't click.
pub struct VersionValue;

/// The slot `n` (from 1) is the input `n - 1`; the three range parameters
/// follow the last slot.
const GLIDE_SECONDS: f32 = 0.015;
pub const MAX_VERSION_SLOTS: i64 = noodle_core::MAX_VERSIONS as i64;

const VERSIONS: ConfigInfo = ConfigInfo::int(VERSION_COUNT_KEY, "Versions", 2);
static CONFIG: [ConfigInfo; 1] = [VERSIONS];

static INFO: NodeInfo = NodeInfo {
    id: VERSION_VALUE_ID,
    version: 1,
    name: "Version Value",
    category: "Modulation",
};

/// The keys of the range parameters.
pub const VERSION_MIN: &str = "min";
pub const VERSION_MAX: &str = "max";
pub const VERSION_TAPER: &str = "taper";

fn slots(config: &Config) -> Result<usize, NodeError> {
    let n = VERSIONS.get_int(config);
    if !(1..=MAX_VERSION_SLOTS).contains(&n) {
        return Err(NodeError::config(format!(
            "Version Value needs between 1 and {MAX_VERSION_SLOTS} versions, not {n}"
        )));
    }
    Ok(n as usize)
}

impl NodeType for VersionValue {
    fn info(&self) -> &NodeInfo {
        &INFO
    }

    fn config(&self) -> &[ConfigInfo] {
        &CONFIG
    }

    fn layout(&self, config: &Config) -> Result<Layout, NodeError> {
        let n = slots(config)?;
        let mut layout = Layout::realtime();
        for i in 0..n {
            layout = layout.param(
                version_slot_key(i),
                format!("Version {}", i + 1),
                ParamInfo::new(0.0, 1.0, 0.0).smoothing(0.0).featured(),
            );
        }
        Ok(layout
            .param(
                VERSION_MIN,
                "Min",
                ParamInfo::new(-20_000.0, 20_000.0, 0.0).smoothing(0.0),
            )
            .param(
                VERSION_MAX,
                "Max",
                ParamInfo::new(-20_000.0, 20_000.0, 1.0).smoothing(0.0),
            )
            .param(
                VERSION_TAPER,
                "Taper",
                ParamInfo::choice(["Linear", "Log"]).smoothing(0.0),
            )
            .control_output("out", "Out"))
    }

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        let slots = slots(setup.config)?;
        let rate = setup.sample_rate;
        Ok(Instance::realtime(VersionValueNode {
            slots,
            glide: 1.0 - (-1.0 / (GLIDE_SECONDS * rate)).exp(),
            position: 0.0,
            started: false,
        }))
    }
}

struct VersionValueNode {
    slots: usize,
    /// The share of the way to the target the position moves each sample.
    glide: f32,
    /// Where the output is along 0 to 1.
    position: f32,
    /// Whether the position has been set yet: the first block starts at the
    /// version that plays rather than gliding to it from 0.
    started: bool,
}

/// `position` (0 to 1) along `min` to `max`.
#[inline]
fn scale(position: f32, min: f32, max: f32, log: bool) -> f32 {
    if log && min > 0.0 && max > min {
        min * (max / min).powf(position)
    } else {
        min + position * (max - min)
    }
}

impl Node for VersionValueNode {
    fn process(&mut self, ctx: &Context, io: Io<'_, '_>) {
        let Io {
            inputs, outputs, ..
        } = io;
        let slot = (ctx.transport.version as usize).min(self.slots - 1);
        let target = inputs[slot].lane(0, 0);
        let min = inputs[self.slots].lane(0, 0)[0];
        let max = inputs[self.slots + 1].lane(0, 0)[0];
        let log = inputs[self.slots + 2].lane(0, 0)[0] >= 0.5;
        let out = outputs[0].lane_mut(0, 0);
        if !self.started {
            self.position = target.first().copied().unwrap_or(0.0);
            self.started = true;
        }
        for (out, &target) in out.iter_mut().zip(target) {
            let target = if target.is_finite() { target } else { 0.0 };
            self.position += (target - self.position) * self.glide;
            *out = scale(self.position.clamp(0.0, 1.0), min, max, log);
        }
    }

    fn reset(&mut self) {
        self.started = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_core::Value;
    use noodle_engine::testing::Harness;

    fn config(n: i64) -> Config {
        Config::new().with(VERSION_COUNT_KEY, Value::Int(n))
    }

    #[test]
    fn it_outputs_the_slot_of_the_version_that_plays() {
        let mut h = Harness::new(&VersionValue, &config(3), &[], 48_000.0, 64).unwrap();
        for (slot, value) in [0.25, 0.5, 0.75].into_iter().enumerate() {
            h.set(slot, value);
        }
        h.set(3, -10.0);
        h.set(4, 10.0);
        h.run(64).unwrap();
        // Version 0 plays: -10 + 0.25 * 20.
        assert!((h.output(0).lane(0, 0)[63] + 5.0).abs() < 1e-4);
        // Switching to version 2 glides to its slot.
        h.set_version(2);
        h.run(64).unwrap();
        let first = h.output(0).lane(0, 0)[0];
        assert!(first < 5.0 && first > -5.0, "{first}");
        for _ in 0..200 {
            h.run(64).unwrap();
        }
        assert!((h.output(0).lane(0, 0)[63] - 5.0).abs() < 1e-3);
    }

    #[test]
    fn a_version_past_the_slots_plays_the_last() {
        let mut h = Harness::new(&VersionValue, &config(2), &[], 48_000.0, 64).unwrap();
        h.set(0, 0.0);
        h.set(1, 1.0);
        h.set_version(9);
        h.run(64).unwrap();
        assert!((h.output(0).lane(0, 0)[63] - 1.0).abs() < 1e-4);
    }

    #[test]
    fn a_log_taper_scales_by_ratio() {
        assert!((scale(0.5, 100.0, 10_000.0, true) - 1000.0).abs() < 0.1);
        assert!((scale(0.5, 100.0, 10_000.0, false) - 5050.0).abs() < 0.1);
        // A range a log can't cover is linear.
        assert!((scale(0.5, -1.0, 1.0, true)).abs() < 1e-6);
    }

    #[test]
    fn too_many_or_no_slots_is_refused() {
        assert!(VersionValue.layout(&config(0)).is_err());
        assert!(VersionValue.layout(&config(17)).is_err());
        let layout = VersionValue.layout(&config(4)).unwrap();
        // Four slots, three range parameters.
        assert_eq!(layout.inputs.len(), 7);
    }
}
