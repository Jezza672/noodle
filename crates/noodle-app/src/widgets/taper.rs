//! Mapping between a parameter's value and a widget's travel, from 0 at the
//! left to 1 at the right.

use noodle_engine::{ParamInfo, ParamKind, Taper};

/// Where `value` sits along the widget's travel, clamped to `0..=1`.
pub fn to_normalized(info: &ParamInfo, value: f32) -> f32 {
    let value = clamp(info, value);
    let t = if uses_log(info) {
        (value / info.min).ln() / (info.max / info.min).ln()
    } else {
        (value - info.min) / (info.max - info.min)
    };
    if t.is_finite() {
        t.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

/// The value at position `t` along the widget's travel. Stepped parameters
/// round to the nearest step.
pub fn from_normalized(info: &ParamInfo, t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    let value = if uses_log(info) {
        info.min * (info.max / info.min).powf(t)
    } else {
        info.min + t * (info.max - info.min)
    };
    clamp(info, value)
}

/// Keeps `value` inside the parameter's range, and rounds it if the
/// parameter is stepped. NaN becomes the default.
pub fn clamp(info: &ParamInfo, value: f32) -> f32 {
    if value.is_nan() {
        return info.default;
    }
    let (low, high) = if info.min <= info.max {
        (info.min, info.max)
    } else {
        (info.max, info.min)
    };
    let value = value.clamp(low, high);
    if is_stepped(info) {
        value.round()
    } else {
        value
    }
}

pub fn is_stepped(info: &ParamInfo) -> bool {
    matches!(info.kind, ParamKind::Stepped { .. })
}

/// A log taper needs a positive range; anything else falls back to linear
/// rather than producing NaNs.
fn uses_log(info: &ParamInfo) -> bool {
    info.taper == Taper::Log && info.min > 0.0 && info.max > info.min
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frequency() -> ParamInfo {
        ParamInfo::new(20.0, 20_000.0, 1_000.0).log()
    }

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() <= 1e-4 * a.abs().max(b.abs()).max(1.0)
    }

    #[test]
    fn linear_maps_the_range_evenly() {
        let info = ParamInfo::new(-10.0, 10.0, 0.0);
        assert_eq!(to_normalized(&info, -10.0), 0.0);
        assert_eq!(to_normalized(&info, 0.0), 0.5);
        assert_eq!(to_normalized(&info, 10.0), 1.0);
        assert_eq!(from_normalized(&info, 0.25), -5.0);
    }

    #[test]
    fn log_gives_equal_ratios_equal_travel() {
        let info = frequency();
        // 20 Hz to 20 kHz is three decades, so each decade is a third.
        assert!(close(to_normalized(&info, 200.0), 1.0 / 3.0));
        assert!(close(to_normalized(&info, 2_000.0), 2.0 / 3.0));
        assert!(close(from_normalized(&info, 0.5), 632.455_5));
        assert_eq!(from_normalized(&info, 0.0), 20.0);
        assert!(close(from_normalized(&info, 1.0), 20_000.0));
    }

    #[test]
    fn round_trips() {
        for info in [frequency(), ParamInfo::new(-60.0, 24.0, 0.0)] {
            for i in 0..=20 {
                let t = i as f32 / 20.0;
                let back = to_normalized(&info, from_normalized(&info, t));
                assert!(close(back, t), "{t} came back as {back}");
            }
        }
    }

    #[test]
    fn log_with_a_non_positive_minimum_falls_back_to_linear() {
        let info = ParamInfo::new(0.0, 100.0, 50.0).log();
        assert_eq!(to_normalized(&info, 50.0), 0.5);
        assert_eq!(from_normalized(&info, 0.5), 50.0);
    }

    #[test]
    fn out_of_range_values_clamp() {
        let info = frequency();
        assert_eq!(to_normalized(&info, 1.0), 0.0);
        assert_eq!(to_normalized(&info, 1e9), 1.0);
        assert_eq!(from_normalized(&info, -3.0), 20.0);
        assert_eq!(clamp(&info, f32::NAN), 1_000.0);
    }

    #[test]
    fn stepped_values_round() {
        let info = ParamInfo::choice(["Sine", "Saw", "Square", "Noise"]);
        assert_eq!(from_normalized(&info, 0.4), 1.0);
        assert_eq!(from_normalized(&info, 0.55), 2.0);
        assert_eq!(clamp(&info, 2.6), 3.0);
        assert_eq!(to_normalized(&info, 1.0), 1.0 / 3.0);
    }

    #[test]
    fn an_empty_range_does_not_divide_by_zero() {
        let info = ParamInfo::new(5.0, 5.0, 5.0);
        assert_eq!(to_normalized(&info, 5.0), 0.0);
        assert_eq!(from_normalized(&info, 0.7), 5.0);
    }
}
