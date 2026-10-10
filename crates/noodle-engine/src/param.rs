//! Parameter descriptions. A parameter is an input port with a default value
//! and a widget. While it's unconnected, the UI and automation set it; once
//! something is wired in, the incoming signal replaces it.

use std::borrow::Cow;

#[derive(Clone, Debug, PartialEq)]
pub struct ParamInfo {
    pub min: f32,
    pub max: f32,
    pub default: f32,
    pub taper: Taper,
    pub unit: Unit,
    pub kind: ParamKind,
    pub modulation: Modulation,
}

/// What a wire into a parameter's port does to the parameter's own value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Modulation {
    /// The wire's signal *is* the value. Right for ports that carry a
    /// signal or an exact value: a pitch, a gate, an automation lane, a
    /// button's state.
    Replace,
    /// The wire's signal moves the value along the slider's travel:
    /// `value = from_travel(to_travel(base) + signal)`, clamped to the
    /// range, where travel runs from 0 at the slider's left end to 1 at its
    /// right (see [`ParamInfo::position`]). A signal of 1 sweeps the whole
    /// range, 0.1 a tenth of it, and on a log taper equal signals move equal
    /// ratios (octaves) whatever the base. The unconnected value is the base,
    /// so the slider stays meaningful with a wire in.
    Offset,
}

/// How a widget spreads the range over its travel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Taper {
    Linear,
    /// Equal ratios get equal travel, as for frequencies. Needs `min > 0`.
    Log,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unit {
    None,
    Hertz,
    Decibels,
    Seconds,
    Semitones,
    Percent,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ParamKind {
    /// Changes from the UI or automation ramp over `smoothing_ms`.
    Continuous { smoothing_ms: f32 },
    /// Whole numbers from `min` to `max`, never smoothed, optionally with a
    /// label for each value. Connected signals arrive unrounded; the node
    /// rounds them.
    Stepped { labels: Vec<Cow<'static, str>> },
}

impl ParamInfo {
    pub const DEFAULT_SMOOTHING_MS: f32 = 20.0;

    /// A continuous parameter with a linear taper and no unit.
    pub fn new(min: f32, max: f32, default: f32) -> Self {
        Self {
            min,
            max,
            default,
            taper: Taper::Linear,
            unit: Unit::None,
            kind: ParamKind::Continuous {
                smoothing_ms: Self::DEFAULT_SMOOTHING_MS,
            },
            modulation: Modulation::Replace,
        }
    }

    /// A choice between named options, numbered from 0.
    pub fn choice<S: Into<Cow<'static, str>>>(labels: impl IntoIterator<Item = S>) -> Self {
        let labels: Vec<_> = labels.into_iter().map(Into::into).collect();
        Self {
            max: labels.len().saturating_sub(1) as f32,
            kind: ParamKind::Stepped { labels },
            ..Self::new(0.0, 0.0, 0.0)
        }
    }

    pub fn log(self) -> Self {
        Self {
            taper: Taper::Log,
            ..self
        }
    }

    pub fn unit(self, unit: Unit) -> Self {
        Self { unit, ..self }
    }

    /// Makes a wire into this parameter offset its value along the slider's
    /// travel instead of replacing it. See [`Modulation::Offset`]. Has no
    /// effect on a stepped parameter, which is whole numbers.
    pub fn offset(self) -> Self {
        Self {
            modulation: Modulation::Offset,
            ..self
        }
    }

    /// Whether wires add to this parameter's value. Stepped parameters never
    /// do.
    pub fn is_offset(&self) -> bool {
        self.modulation == Modulation::Offset && matches!(self.kind, ParamKind::Continuous { .. })
    }

    /// A log taper needs a positive range; anything else falls back to linear
    /// rather than producing NaNs.
    fn uses_log(&self) -> bool {
        self.taper == Taper::Log && self.min > 0.0 && self.max > self.min
    }

    /// Where `value` sits along the slider's travel, from 0 at the minimum to
    /// 1 at the maximum, clamped to that range. A non-finite value gives 0.
    pub fn position(&self, value: f32) -> f32 {
        let value = value.clamp(self.min.min(self.max), self.max.max(self.min));
        let t = if self.uses_log() {
            (value / self.min).ln() / (self.max / self.min).ln()
        } else {
            (value - self.min) / (self.max - self.min)
        };
        if t.is_finite() {
            t.clamp(0.0, 1.0)
        } else {
            0.0
        }
    }

    /// The value at `position` along the travel, clamped to `0..=1`. Does not
    /// round stepped parameters.
    pub fn value_at(&self, position: f32) -> f32 {
        let t = position.clamp(0.0, 1.0);
        if self.uses_log() {
            self.min * (self.max / self.min).powf(t)
        } else {
            self.min + t * (self.max - self.min)
        }
    }

    pub fn smoothing(self, smoothing_ms: f32) -> Self {
        Self {
            kind: ParamKind::Continuous { smoothing_ms },
            ..self
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn position_and_value_at_are_inverses() {
        for info in [
            ParamInfo::new(20.0, 20_000.0, 1_000.0).log(),
            ParamInfo::new(-60.0, 24.0, 0.0),
        ] {
            for i in 0..=10 {
                let t = i as f32 / 10.0;
                let back = info.position(info.value_at(t));
                assert!((back - t).abs() < 1e-4, "{t} came back as {back}");
            }
        }
    }

    #[test]
    fn log_travel_is_in_equal_ratios() {
        let info = ParamInfo::new(20.0, 20_000.0, 1_000.0).log();
        // Three decades: a third of the travel is a decade.
        let a = info.value_at(info.position(100.0) + 1.0 / 3.0);
        assert!((a - 1_000.0).abs() < 0.5, "{a}");
        let b = info.value_at(info.position(10_000.0) + 1.0 / 3.0);
        // Past the top it clamps.
        assert!((b - 20_000.0).abs() < 1.0, "{b}");
    }

    #[test]
    fn a_log_range_through_zero_falls_back_to_linear() {
        let info = ParamInfo::new(0.0, 100.0, 50.0).log();
        assert_eq!(info.position(25.0), 0.25);
        assert_eq!(info.value_at(0.75), 75.0);
    }

    #[test]
    fn stepped_parameters_never_offset() {
        assert!(!ParamInfo::choice(["a", "b"]).offset().is_offset());
        assert!(ParamInfo::new(0.0, 1.0, 0.0).offset().is_offset());
        assert!(!ParamInfo::new(0.0, 1.0, 0.0).is_offset());
    }
}
