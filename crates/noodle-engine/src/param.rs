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

    pub fn smoothing(self, smoothing_ms: f32) -> Self {
        Self {
            kind: ParamKind::Continuous { smoothing_ms },
            ..self
        }
    }
}
