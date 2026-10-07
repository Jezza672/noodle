use noodle_engine::{
    Config, Context, Instance, Io, Layout, Node, NodeError, NodeInfo, NodeType, Setup,
};

/// White noise, spread evenly between -1 and 1. It's seeded from the node, so
/// it's the same on every render, and can be cached, but it's different for
/// every noise node.
pub struct WhiteNoise;

const OUT: usize = 0;

static INFO: NodeInfo = NodeInfo {
    id: "noodle.noise.white",
    version: 1,
    name: "White Noise",
    category: "Generators",
};

impl NodeType for WhiteNoise {
    fn info(&self) -> &NodeInfo {
        &INFO
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(Layout::realtime().output("out", "Out"))
    }

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        Ok(Instance::realtime(WhiteNoiseNode(Rng::new(setup.seed))))
    }
}

struct WhiteNoiseNode(Rng);

impl Node for WhiteNoiseNode {
    fn process(&mut self, _ctx: &Context, io: Io<'_, '_>) {
        for sample in io.outputs[OUT].lane_mut(0, 0) {
            *sample = self.0.next();
        }
    }
}

/// xorshift64*: fast, and plenty random for audio.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        // xorshift gets stuck at zero.
        Self(seed.max(1))
    }

    /// Uniform in [-1, 1).
    fn next(&mut self) -> f32 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        let bits = x.wrapping_mul(0x2545_f491_4f6c_dd1d) >> 40;
        bits as f32 / (1 << 23) as f32 - 1.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_engine::testing::Harness;

    fn noise(seed: u64, frames: usize) -> Vec<f32> {
        let mut h =
            Harness::with_seed(&WhiteNoise, &Config::new(), &[], 48_000.0, frames, seed).unwrap();
        h.run(frames).unwrap();
        h.output(OUT).lane(0, 0).to_vec()
    }

    #[test]
    fn repeats_for_a_seed_and_differs_between_seeds() {
        assert_eq!(noise(1, 64), noise(1, 64));
        assert_ne!(noise(1, 64), noise(2, 64));
    }

    #[test]
    fn is_evenly_spread_between_minus_one_and_one() {
        let samples = noise(7, 48_000);
        assert!(samples.iter().all(|x| (-1.0..1.0).contains(x)));
        let mean = samples.iter().sum::<f32>() / samples.len() as f32;
        let variance =
            samples.iter().map(|x| (x - mean).powi(2)).sum::<f32>() / samples.len() as f32;
        assert!(mean.abs() < 0.02, "mean {mean}");
        // A uniform distribution on [-1, 1) has variance 1/3.
        assert!((variance - 1.0 / 3.0).abs() < 0.02, "variance {variance}");
    }
}
