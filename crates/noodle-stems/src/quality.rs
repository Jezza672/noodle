//! How hard a separator works: the same model, run with more overlap and on
//! shifted copies of the audio, which costs time and smooths the seams.

/// A separation quality.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Quality {
    /// A quarter overlap between segments, one pass.
    #[default]
    Fast,
    /// Half overlap.
    Better,
    /// Half overlap, and each segment run on time-shifted copies that are
    /// averaged, as Demucs' `--shifts` does. Several times slower.
    Best,
}

impl Quality {
    pub const ALL: [Quality; 3] = [Quality::Fast, Quality::Better, Quality::Best];

    pub fn label(self) -> &'static str {
        match self {
            Quality::Fast => "Fast",
            Quality::Better => "Better",
            Quality::Best => "Best",
        }
    }

    /// How much neighbouring segments overlap, as a fraction of a segment.
    pub fn overlap(self) -> f32 {
        match self {
            Quality::Fast => 0.25,
            Quality::Better | Quality::Best => 0.5,
        }
    }

    /// How many shifted copies of each segment are separated and averaged.
    pub fn shifts(self) -> usize {
        match self {
            Quality::Fast | Quality::Better => 1,
            Quality::Best => 3,
        }
    }

    /// How much longer than Fast a split takes, roughly, for the dialog to say.
    pub fn cost(self) -> f32 {
        let fast = 1.0 / (1.0 - Quality::Fast.overlap());
        self.shifts() as f32 / (1.0 - self.overlap()) / fast
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_step_up_costs_more() {
        assert!(Quality::Better.cost() > Quality::Fast.cost());
        assert!(Quality::Best.cost() > Quality::Better.cost());
        assert_eq!(Quality::Fast.cost(), 1.0);
    }
}
