//! Showing parameter values as text with their units, and reading typed
//! values back.
//!
//! Values are stored in the unit they're shown in: a [`Unit::Percent`]
//! parameter running from 0 to 100 shows 50 as "50.0 %". Frequencies and
//! times switch prefix (kHz, ms) to stay readable.

use noodle_engine::{ParamInfo, ParamKind, Unit};

use super::taper;

/// The value as the user should read it, e.g. "1.20 kHz", "-6.0 dB" or a
/// stepped parameter's label.
pub fn format_value(info: &ParamInfo, value: f32) -> String {
    if let ParamKind::Stepped { labels } = &info.kind {
        let index = taper::clamp(info, value);
        if let Some(label) = labels.get(index as usize).filter(|_| index >= 0.0) {
            return label.to_string();
        }
        return with_unit(&format!("{index}"), info.unit);
    }
    match info.unit {
        Unit::Hertz if value.abs() >= 1_000.0 => format!("{} kHz", significant(value / 1_000.0)),
        Unit::Seconds if value.abs() < 1.0 && value != 0.0 => {
            format!("{} ms", significant(value * 1_000.0))
        }
        Unit::Decibels => format!("{value:.1} dB"),
        Unit::Semitones => format!("{value:+.2} st"),
        Unit::Cents => format!("{value:.0} ct"),
        unit => with_unit(&significant(value), unit),
    }
}

/// Reads a typed value, accepting the unit and its prefixes ("2k", "1.5 kHz",
/// "250ms", "-6 dB") and, for stepped parameters, a label. The result is
/// clamped to the range; `None` if the text isn't a value.
pub fn parse_value(info: &ParamInfo, text: &str) -> Option<f32> {
    let text = text.trim();
    if let ParamKind::Stepped { labels } = &info.kind
        && let Some(index) = labels.iter().position(|l| l.eq_ignore_ascii_case(text))
    {
        return Some(index as f32);
    }

    let lower = text.to_ascii_lowercase();
    let split = lower
        .find(|c: char| !(c.is_ascii_digit() || matches!(c, '.' | '-' | '+' | 'e')))
        .unwrap_or(lower.len());
    // `e` is only part of the number when digits follow, as in "1e3".
    let (number, suffix) = match lower[..split].rfind('e') {
        Some(e) if !lower[e + 1..split].chars().any(|c| c.is_ascii_digit()) => lower.split_at(e),
        _ => lower.split_at(split),
    };
    let number: f32 = number.parse().ok()?;
    let scale = scale_for(info.unit, suffix.trim())?;
    let value = number * scale;
    value.is_finite().then(|| taper::clamp(info, value))
}

/// What a typed suffix multiplies the number by, or `None` if it doesn't
/// belong to this unit.
fn scale_for(unit: Unit, suffix: &str) -> Option<f32> {
    let scale = match (unit, suffix) {
        (_, "") => 1.0,
        (Unit::Hertz, "hz") => 1.0,
        (Unit::Hertz, "k" | "khz") => 1_000.0,
        (Unit::Decibels, "db") => 1.0,
        (Unit::Seconds, "s" | "sec") => 1.0,
        (Unit::Seconds, "ms") => 1e-3,
        (Unit::Semitones, "st" | "semi" | "semitones") => 1.0,
        (Unit::Cents, "ct" | "cents") => 1.0,
        (Unit::Percent, "%") => 1.0,
        _ => return None,
    };
    Some(scale)
}

fn with_unit(number: &str, unit: Unit) -> String {
    match unit_symbol(unit) {
        "" => number.to_owned(),
        symbol => format!("{number} {symbol}"),
    }
}

pub fn unit_symbol(unit: Unit) -> &'static str {
    match unit {
        Unit::None => "",
        Unit::Hertz => "Hz",
        Unit::Decibels => "dB",
        Unit::Seconds => "s",
        Unit::Semitones => "st",
        Unit::Cents => "ct",
        Unit::Percent => "%",
    }
}

/// Three significant figures, without going past three decimal places, so
/// 440 shows as "440", 1.2 as "1.20" and 0.5 as "0.500".
fn significant(value: f32) -> String {
    let magnitude = if value == 0.0 {
        0
    } else {
        value.abs().log10().floor() as i32
    };
    let decimals = (2 - magnitude).clamp(0, 3) as usize;
    let text = format!("{value:.decimals$}");
    // Rounding can produce "-0.000"; show it as zero.
    if text
        .trim_start_matches('-')
        .chars()
        .all(|c| c == '0' || c == '.')
    {
        text.trim_start_matches('-').to_owned()
    } else {
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hz() -> ParamInfo {
        ParamInfo::new(0.01, 20_000.0, 440.0)
            .log()
            .unit(Unit::Hertz)
    }

    #[test]
    fn formats_with_units_and_prefixes() {
        assert_eq!(format_value(&hz(), 440.0), "440 Hz");
        assert_eq!(format_value(&hz(), 1_200.0), "1.20 kHz");
        assert_eq!(format_value(&hz(), 20_000.0), "20.0 kHz");
        assert_eq!(format_value(&hz(), 0.5), "0.500 Hz");

        let gain = ParamInfo::new(-60.0, 24.0, 0.0).unit(Unit::Decibels);
        assert_eq!(format_value(&gain, -6.0), "-6.0 dB");

        let time = ParamInfo::new(0.0, 10.0, 0.0).unit(Unit::Seconds);
        assert_eq!(format_value(&time, 0.25), "250 ms");
        assert_eq!(format_value(&time, 1.5), "1.50 s");
        assert_eq!(format_value(&time, 0.0), "0.00 s");

        let pitch = ParamInfo::new(-24.0, 24.0, 0.0).unit(Unit::Semitones);
        assert_eq!(format_value(&pitch, 7.0), "+7.00 st");
        assert_eq!(format_value(&pitch, -12.0), "-12.00 st");

        let mix = ParamInfo::new(0.0, 100.0, 50.0).unit(Unit::Percent);
        assert_eq!(format_value(&mix, 50.0), "50.0 %");

        let plain = ParamInfo::new(0.0, 1.0, 0.0);
        assert_eq!(format_value(&plain, 0.5), "0.500");
        assert_eq!(format_value(&plain, -0.0001), "0.000");
    }

    #[test]
    fn stepped_parameters_show_labels_or_whole_numbers() {
        let wave = ParamInfo::choice(["Sine", "Saw"]);
        assert_eq!(format_value(&wave, 0.0), "Sine");
        assert_eq!(format_value(&wave, 0.9), "Saw");

        let mut octave = ParamInfo::choice(Vec::<&str>::new());
        octave.min = -2.0;
        octave.max = 2.0;
        assert_eq!(format_value(&octave, -1.2), "-1");
    }

    #[test]
    fn parses_numbers_with_units() {
        assert_eq!(parse_value(&hz(), "440"), Some(440.0));
        assert_eq!(parse_value(&hz(), " 2k "), Some(2_000.0));
        assert_eq!(parse_value(&hz(), "1.5 kHz"), Some(1_500.0));
        assert_eq!(parse_value(&hz(), "1e3"), Some(1_000.0));
        assert_eq!(parse_value(&hz(), "99999"), Some(20_000.0));
        assert_eq!(parse_value(&hz(), "3 dB"), None);
        assert_eq!(parse_value(&hz(), "loud"), None);
        assert_eq!(parse_value(&hz(), ""), None);

        let gain = ParamInfo::new(-60.0, 24.0, 0.0).unit(Unit::Decibels);
        assert_eq!(parse_value(&gain, "-6dB"), Some(-6.0));

        let time = ParamInfo::new(0.0, 10.0, 0.0).unit(Unit::Seconds);
        assert_eq!(parse_value(&time, "250 ms"), Some(0.25));
        assert_eq!(parse_value(&time, "2s"), Some(2.0));
    }

    #[test]
    fn parses_labels_and_rounds_steps() {
        let wave = ParamInfo::choice(["Sine", "Saw"]);
        assert_eq!(parse_value(&wave, "saw"), Some(1.0));
        assert_eq!(parse_value(&wave, "0.7"), Some(1.0));
        assert_eq!(parse_value(&wave, "Square"), None);
    }

    #[test]
    fn formatted_values_parse_back() {
        let params = [
            hz(),
            ParamInfo::new(-60.0, 24.0, 0.0).unit(Unit::Decibels),
            ParamInfo::new(0.0, 10.0, 0.0).unit(Unit::Seconds),
            ParamInfo::new(-24.0, 24.0, 0.0).unit(Unit::Semitones),
            ParamInfo::new(0.0, 100.0, 0.0).unit(Unit::Percent),
            ParamInfo::choice(["Sine", "Saw"]),
        ];
        for info in &params {
            for value in [info.min, info.default, info.max] {
                let text = format_value(info, value);
                let back = parse_value(info, &text).unwrap_or_else(|| panic!("{text:?}"));
                assert!(
                    (back - value).abs() <= 0.01 * value.abs().max(1.0),
                    "{value} showed as {text:?} and came back as {back}"
                );
            }
        }
    }
}
