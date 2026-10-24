//! Graphs for the Dynamics and Parametric EQ nodes, drawn by
//! [`graph`](super::graph) like the others.
//!
//! **Dynamics** draws two plots side by side. On the left is the transfer
//! curve, input level against output level in dB, with points to drag for the
//! threshold, ratio, knee and makeup, and a dot at the level passing through
//! now. On the right is the response in time to a step: the gain reduction
//! climbing at the attack and falling at the release, each with a point to
//! drag. Small, in the node body, only the transfer curve is drawn.
//!
//! **Parametric EQ** draws the combined response, with a point for each band
//! to drag along frequency and gain.
//!
//! The curves come from the node crate (`reduction_db`, `eq_response_db`), which
//! tests them against the real DSP.

use egui::epaint::PathShape;
use egui::{Align2, Color32, FontId, Painter, Rect, Stroke, StrokeKind, vec2};
use noodle_nodes::{
    DynamicsMode, EQ_MAX_BANDS, EqBand, EqType, attack_response, eq_response_db, reduction_db,
    release_response, transfer_db,
};

use super::graph::{
    AdsrLayout, Handle, P, Values, hz_text, hz_to_x, seconds_text, to_screen, x_to_hz,
};
use crate::theme::{self, editor as colors};

const SAMPLE_RATE: f32 = 48_000.0;

/// The input levels the transfer plot covers, in dB.
const IN_RANGE: (f32, f32) = (-60.0, 0.0);
/// The output levels it covers.
const OUT_RANGE: (f32, f32) = (-60.0, 12.0);
/// The share of the full-size graph's width that the transfer plot takes, and
/// where the time plot starts.
const TRANSFER_WIDTH: f32 = 0.46;
const TIME_LEFT: f32 = 0.54;

/// The gains the EQ plot covers, in dB.
const EQ_RANGE: (f32, f32) = (-24.0, 24.0);

/// The keys of each EQ band's frequency and gain, for the handles.
const BAND_KEYS: [[&str; 2]; EQ_MAX_BANDS as usize] = [
    ["freq1", "gain1"],
    ["freq2", "gain2"],
    ["freq3", "gain3"],
    ["freq4", "gain4"],
    ["freq5", "gain5"],
    ["freq6", "gain6"],
    ["freq7", "gain7"],
    ["freq8", "gain8"],
];

struct Curve {
    mode: DynamicsMode,
    threshold: f32,
    ratio: f32,
    knee: f32,
    makeup: f32,
}

impl Curve {
    fn read(values: Values<'_>) -> Self {
        Self {
            mode: DynamicsMode::from_value(values("mode")),
            threshold: values("threshold"),
            ratio: values("ratio"),
            knee: values("knee"),
            makeup: values("makeup"),
        }
    }

    fn reduction(&self, level: f32) -> f32 {
        reduction_db(self.mode, self.threshold, self.ratio, self.knee, level)
    }

    /// The output level for an input level, with makeup.
    fn out(&self, level: f32) -> f32 {
        transfer_db(self.mode, self.threshold, self.ratio, self.knee, level) + self.makeup
    }
}

fn in_x(db: f32, full: bool) -> f32 {
    let t = ((db - IN_RANGE.0) / (IN_RANGE.1 - IN_RANGE.0)).clamp(0.0, 1.0);
    if full { t * TRANSFER_WIDTH } else { t }
}

fn x_in(x: f32) -> f32 {
    IN_RANGE.0 + (x / TRANSFER_WIDTH).clamp(0.0, 1.0) * (IN_RANGE.1 - IN_RANGE.0)
}

fn out_y(db: f32) -> f32 {
    ((db - OUT_RANGE.0) / (OUT_RANGE.1 - OUT_RANGE.0)).clamp(0.0, 1.0)
}

fn y_out(y: f32) -> f32 {
    OUT_RANGE.0 + y.clamp(0.0, 1.0) * (OUT_RANGE.1 - OUT_RANGE.0)
}

/// The time axis of the step response: the level steps up for `hold` and
/// back down, over `span` seconds in all, with `depth` dB of reduction on the
/// vertical axis. `depth` comes from the most the node could reduce at any
/// ratio, so turning the ratio up always moves the curve within a fixed axis.
/// The editor keeps the layout while it still fits ([`AdsrLayout::keeps`]).
pub(super) fn dynamics_layout(values: Values<'_>) -> AdsrLayout {
    let (attack, release) = (values("attack"), values("release"));
    let hold = (4.0 * attack).max(0.02);
    let c = Curve::read(values);
    let most = match c.mode {
        DynamicsMode::Compressor | DynamicsMode::Limiter => {
            reduction_db(DynamicsMode::Limiter, c.threshold, 1.0, c.knee, IN_RANGE.1)
        }
        DynamicsMode::Expander => reduction_db(c.mode, c.threshold, 100.0, c.knee, IN_RANGE.0),
        DynamicsMode::Gate => reduction_db(c.mode, c.threshold, 1.0, c.knee, IN_RANGE.0),
    };
    AdsrLayout {
        span: (hold + 4.0 * release + attack) * 1.5,
        hold,
        depth: most.max(6.0) * 1.5,
    }
}

/// The gain reduction `t` seconds into the step response, in dB, and the
/// most it can be. A compressor or limiter is hit by a loud signal at 0 and
/// let go at `hold`; a gate or expander is opened by a loud signal at 0
/// (having been closed) and closed again at `hold`.
fn step_reduction(c: &Curve, attack: f32, release: f32, layout: AdsrLayout, t: f32) -> (f32, f32) {
    if c.mode.is_downward() {
        let closed = c.reduction(IN_RANGE.0);
        let g = if t < layout.hold {
            release_response(closed, attack, t)
        } else {
            let open = release_response(closed, attack, layout.hold);
            closed - (closed - open) * (-(t - layout.hold) / release.max(1e-6)).exp()
        };
        (g, closed)
    } else {
        let loud = c.reduction(IN_RANGE.1);
        let g = if t < layout.hold {
            attack_response(loud, attack, t)
        } else {
            release_response(
                attack_response(loud, attack, layout.hold),
                release,
                t - layout.hold,
            )
        };
        (g, loud)
    }
}

/// Where a reduction sits on the time plot: none at the top.
fn red_y(reduction: f32, layout: AdsrLayout) -> f32 {
    1.0 - (reduction / layout.depth.max(1.0)).clamp(0.0, 1.0)
}

fn time_x(t: f32, layout: AdsrLayout) -> f32 {
    TIME_LEFT + (1.0 - TIME_LEFT) * (t / layout.span).clamp(0.0, 1.0)
}

fn x_time(x: f32, layout: AdsrLayout) -> f32 {
    (x - TIME_LEFT) / (1.0 - TIME_LEFT) * layout.span
}

/// The EQ bands the values describe.
pub(super) fn eq_bands(values: Values<'_>) -> Vec<EqBand> {
    let count = (values("bands").round() as usize).clamp(1, EQ_MAX_BANDS as usize);
    (1..=count)
        .map(|i| EqBand {
            kind: EqType::from_value(values(&format!("type{i}"))),
            freq: values(&format!("freq{i}")),
            gain_db: values(&format!("gain{i}")),
            q: values(&format!("q{i}")),
        })
        .collect()
}

fn eq_y(db: f32) -> f32 {
    ((db - EQ_RANGE.0) / (EQ_RANGE.1 - EQ_RANGE.0)).clamp(0.0, 1.0)
}

fn y_eq(y: f32) -> f32 {
    EQ_RANGE.0 + y.clamp(0.0, 1.0) * (EQ_RANGE.1 - EQ_RANGE.0)
}

/// Whether a band type has a gain to set.
fn has_gain(kind: EqType) -> bool {
    matches!(kind, EqType::Bell | EqType::LowShelf | EqType::HighShelf)
}

pub(super) fn eq_handles(values: Values<'_>) -> Vec<Handle> {
    let bands = eq_bands(values);
    bands
        .iter()
        .enumerate()
        .map(|(i, band)| {
            let others_and_this = eq_response_db(&bands, band.freq, SAMPLE_RATE);
            let y = if has_gain(band.kind) {
                eq_y(band.gain_db)
            } else {
                eq_y(others_and_this)
            };
            Handle {
                at: (hz_to_x(band.freq), y),
                keys: if has_gain(band.kind) {
                    &BAND_KEYS[i][..]
                } else {
                    &BAND_KEYS[i][..1]
                },
                name: "Band",
            }
        })
        .collect()
}

pub(super) fn eq_dragged(which: usize, at: P, values: Values<'_>) -> Vec<(&'static str, f32)> {
    let bands = eq_bands(values);
    let Some(band) = bands.get(which) else {
        return Vec::new();
    };
    let [freq, gain] = BAND_KEYS[which];
    let mut out = vec![(freq, x_to_hz(at.0))];
    if has_gain(band.kind) {
        out.push((gain, y_eq(at.1)));
    }
    out
}

/// The input level the ratio point sits at: halfway along the sloped part
/// of the curve (past the knee, towards the end of the plot), where the ratio
/// shows most. The makeup point has the right-hand end of the curve to itself.
fn ratio_level(c: &Curve) -> f32 {
    if c.mode.is_downward() {
        let edge = c.threshold - c.knee / 2.0;
        let span = (edge - IN_RANGE.0).max(3.0);
        (IN_RANGE.0 + span / 2.0).max(IN_RANGE.0)
    } else {
        let edge = c.threshold + c.knee / 2.0;
        let span = (IN_RANGE.1 - edge).max(3.0);
        (IN_RANGE.1 - span / 2.0).min(IN_RANGE.1)
    }
}

/// The ratio that makes the curve pass through `out_level` at `level`
/// (including makeup), found by bisection because the knee bends the curve.
fn ratio_through(c: &Curve, level: f32, out_level: f32) -> f32 {
    let target = (level + c.makeup - out_level).max(0.0);
    let (mut low, mut high) = (1.0f32, 100.0f32);
    for _ in 0..40 {
        let mid = (low * high).sqrt();
        if reduction_db(c.mode, c.threshold, mid, c.knee, level) < target {
            low = mid;
        } else {
            high = mid;
        }
    }
    (low * high).sqrt()
}

pub(super) fn dynamics_handles(values: Values<'_>, layout: AdsrLayout) -> Vec<Handle> {
    let c = Curve::read(values);
    let (attack, release) = (values("attack"), values("release"));
    let t = c.threshold;
    let mut out = vec![Handle {
        at: (in_x(t, true), out_y(c.out(t))),
        keys: &["threshold"],
        name: "Threshold",
    }];
    if matches!(c.mode, DynamicsMode::Compressor | DynamicsMode::Expander) {
        let level = ratio_level(&c);
        out.push(Handle {
            at: (in_x(level, true), out_y(c.out(level))),
            keys: &["ratio"],
            name: "Ratio",
        });
    }
    let side = if c.mode.is_downward() { -1.0 } else { 1.0 };
    let knee_at = t + side * c.knee / 2.0;
    out.push(Handle {
        at: (in_x(knee_at, true), out_y(c.out(knee_at))),
        keys: &["knee"],
        name: "Knee",
    });
    out.push(Handle {
        at: (in_x(IN_RANGE.1, true), out_y(c.out(IN_RANGE.1))),
        keys: &["makeup"],
        name: "Makeup",
    });
    let on_curve = |t: f32| {
        let (g, _) = step_reduction(&c, attack, release, layout, t);
        (time_x(t, layout), red_y(g, layout))
    };
    out.push(Handle {
        at: on_curve(attack),
        keys: &["attack"],
        name: "Attack",
    });
    out.push(Handle {
        at: on_curve(layout.hold + release),
        keys: &["release"],
        name: "Release",
    });
    out
}

pub(super) fn dynamics_dragged(
    which: usize,
    at: P,
    layout: AdsrLayout,
    values: Values<'_>,
) -> Vec<(&'static str, f32)> {
    let c = Curve::read(values);
    let handles = dynamics_handles(values, layout);
    let Some(handle) = handles.get(which) else {
        return Vec::new();
    };
    let level = x_in(at.0);
    let out_level = y_out(at.1);
    match handle.name {
        "Threshold" => vec![("threshold", level)],
        "Ratio" => vec![("ratio", ratio_through(&c, ratio_level(&c), out_level))],
        "Knee" => vec![("knee", 2.0 * (level - c.threshold).abs())],
        "Makeup" => {
            let without = transfer_db(c.mode, c.threshold, c.ratio, c.knee, IN_RANGE.1);
            vec![("makeup", out_level - without)]
        }
        "Attack" => vec![("attack", x_time(at.0, layout).max(0.0))],
        "Release" => vec![("release", (x_time(at.0, layout) - layout.hold).max(0.0))],
        _ => Vec::new(),
    }
}

struct Pen<'a> {
    painter: &'a Painter,
    rect: Rect,
}

impl Pen<'_> {
    fn line(&self, a: P, b: P, stroke: Stroke) {
        self.painter
            .line_segment([to_screen(self.rect, a), to_screen(self.rect, b)], stroke);
    }

    fn poly(&self, points: &[P], stroke: Stroke) {
        let points = points.iter().map(|&p| to_screen(self.rect, p)).collect();
        self.painter.add(PathShape::line(points, stroke));
    }

    fn text(&self, at: P, align: Align2, text: &str, nudge: egui::Vec2) {
        self.painter.text(
            to_screen(self.rect, at) + nudge,
            align,
            text,
            FontId::proportional(10.0),
            colors::TEXT_WEAK,
        );
    }
}

/// The EQ's response, with its grid when `full`.
pub(super) fn paint_eq(painter: &Painter, rect: Rect, values: Values<'_>, scale: f32, full: bool) {
    let pen = Pen { painter, rect };
    let grid = Stroke::new(1.0, colors::GRID_MAJOR);
    if full {
        for hz in [100.0, 500.0, 1_000.0, 5_000.0, 10_000.0] {
            let x = hz_to_x(hz);
            pen.line((x, 0.0), (x, 1.0), grid);
            pen.text((x, 0.0), Align2::LEFT_BOTTOM, &hz_text(hz), vec2(2.0, -1.0));
        }
    }
    for db in [-12.0f32, 0.0, 12.0] {
        let y = eq_y(db);
        pen.line((0.0, y), (1.0, y), grid);
        if full {
            let text = format!("{db:+} dB").replace("+0", "0");
            pen.text((0.0, y), Align2::LEFT_BOTTOM, &text, vec2(2.0, -1.0));
        }
    }
    let bands = eq_bands(values);
    {
        // DEBUG (temporary): the rect each view plots into, and its curve.
        thread_local!(static LAST: std::cell::RefCell<std::collections::HashMap<bool, String>> = Default::default());
        let text = format!(
            "full={full} rect=({:.1},{:.1} {:.1}x{:.1}) clip={:?} scale={scale} bands={:?}",
            rect.left(),
            rect.top(),
            rect.width(),
            rect.height(),
            painter.clip_rect(),
            bands
        );
        LAST.with(|l| {
            let mut l = l.borrow_mut();
            if l.get(&full) != Some(&text) {
                eprintln!("[eqdebug] {text}");
                l.insert(full, text);
            }
        });
    }
    let points: Vec<P> = (0..=160)
        .map(|i| {
            let x = i as f32 / 160.0;
            (x, eq_y(eq_response_db(&bands, x_to_hz(x), SAMPLE_RATE)))
        })
        .collect();
    pen.poly(
        &points,
        Stroke::new(if full { 2.0 } else { 1.5 * scale }, theme::ACCENT),
    );
}

/// The transfer curve, and the step response beside it when `full`.
pub(super) fn paint_dynamics(
    painter: &Painter,
    rect: Rect,
    values: Values<'_>,
    layout: AdsrLayout,
    scale: f32,
    full: bool,
) {
    let pen = Pen { painter, rect };
    let c = Curve::read(values);
    let line = Stroke::new(if full { 2.0 } else { 1.5 * scale }, theme::ACCENT);
    let grid = Stroke::new(1.0, colors::GRID_MAJOR);
    let weak = Stroke::new(1.0, colors::TEXT_WEAK.gamma_multiply(0.5));

    // Transfer: the diagonal of no change, the curve, and the threshold.
    let right = if full { TRANSFER_WIDTH } else { 1.0 };
    for db in [-40.0f32, -20.0, 0.0] {
        let x = in_x(db, full);
        pen.line((x, 0.0), (x, 1.0), grid);
        if full {
            pen.text(
                (x, 0.0),
                Align2::LEFT_BOTTOM,
                &format!("{db} dB").replace("-0", "0"),
                vec2(2.0, -1.0),
            );
        }
    }
    pen.line((0.0, out_y(IN_RANGE.0)), (right, out_y(IN_RANGE.1)), weak);
    let xs = |i: usize| i as f32 / 120.0;
    let curve: Vec<P> = (0..=120)
        .map(|i| {
            let level = IN_RANGE.0 + xs(i) * (IN_RANGE.1 - IN_RANGE.0);
            (xs(i) * right, out_y(c.out(level)))
        })
        .collect();
    pen.poly(&curve, line);
    pen.line(
        (in_x(c.threshold, full), 0.0),
        (in_x(c.threshold, full), 1.0),
        Stroke::new(1.0, theme::ACCENT.gamma_multiply(0.4)),
    );
    // The level passing through now.
    let level = values("@level");
    if level.is_finite() {
        let reduction = values("@reduction");
        let level = level.clamp(IN_RANGE.0, IN_RANGE.1);
        let dot = to_screen(
            rect,
            (in_x(level, full), out_y(level - reduction + c.makeup)),
        );
        painter.circle_filled(dot, 3.5 * if full { 1.0 } else { scale }, Color32::WHITE);
    }

    if !full {
        return;
    }
    // Step response.
    let (attack, release) = (values("attack"), values("release"));
    let steps = 160;
    let points: Vec<P> = (0..=steps)
        .map(|i| {
            let t = layout.span * i as f32 / steps as f32;
            let (g, _) = step_reduction(&c, attack, release, layout, t);
            (time_x(t, layout), red_y(g, layout))
        })
        .collect();
    pen.line((TIME_LEFT, 1.0), (1.0, 1.0), grid);
    pen.line((TIME_LEFT, 0.0), (TIME_LEFT, 1.0), grid);
    pen.line(
        (time_x(layout.hold, layout), 0.0),
        (time_x(layout.hold, layout), 1.0),
        grid,
    );
    pen.text(
        (time_x(layout.hold, layout), 0.0),
        Align2::LEFT_BOTTOM,
        &seconds_text(layout.hold),
        vec2(2.0, -1.0),
    );
    pen.text(
        (TIME_LEFT, 1.0),
        Align2::LEFT_TOP,
        "gain reduction",
        vec2(3.0, 2.0),
    );
    pen.poly(&points, line);
    painter.rect_stroke(
        rect,
        0.0,
        Stroke::new(1.0, colors::NODE_OUTLINE),
        StrokeKind::Inside,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values(key: &str) -> f32 {
        match key {
            "mode" => 0.0,
            "threshold" => -24.0,
            "ratio" => 4.0,
            "knee" => 0.0,
            "makeup" => 0.0,
            "attack" => 0.01,
            "release" => 0.2,
            "bands" => 2.0,
            "type1" => 0.0,
            "freq1" => 200.0,
            "gain1" => 6.0,
            "q1" => 1.0,
            "type2" => 3.0,
            "freq2" => 80.0,
            "q2" => 0.7,
            _ => 0.0,
        }
    }

    fn range(key: &str) -> (f32, f32) {
        match key {
            "threshold" => (-80.0, 0.0),
            _ => (0.0, 100.0),
        }
    }

    #[test]
    fn dragging_a_dynamics_point_to_where_it_is_changes_nothing() {
        let layout = dynamics_layout(&values);
        let handles = dynamics_handles(&values, layout);
        let names: Vec<_> = handles.iter().map(|h| h.name).collect();
        assert_eq!(
            names,
            ["Threshold", "Ratio", "Knee", "Makeup", "Attack", "Release"]
        );
        let _ = range;
        for (i, h) in handles.iter().enumerate() {
            for (key, value) in dynamics_dragged(i, h.at, layout, &values) {
                let before = values(key);
                // The knee at 0 dB sits on the threshold, so any x along the
                // curve there is the same knee of 0.
                assert!(
                    (value - before).abs() <= 0.02 * before.abs().max(1.0),
                    "{key}: {value} vs {before}"
                );
            }
        }
    }

    #[test]
    fn the_ratio_point_sets_the_ratio_the_curve_shows() {
        let layout = dynamics_layout(&values);
        let handles = dynamics_handles(&values, layout);
        let at = handles[1].at;
        // Dragged lower, the curve is flatter: a higher ratio.
        let lower = dynamics_dragged(1, (at.0, at.1 - 0.15), layout, &values);
        assert!(lower[0].1 > 4.0, "{lower:?}");
        // Dragged to the diagonal: no compression.
        let diagonal = dynamics_dragged(
            1,
            (at.0, out_y(ratio_level(&Curve::read(&values)))),
            layout,
            &values,
        );
        assert!((diagonal[0].1 - 1.0).abs() < 1e-3, "{diagonal:?}");
    }

    #[test]
    fn the_makeup_point_is_at_the_right_of_the_curve_and_the_ratio_point_is_not() {
        for mode in [0.0, 3.0] {
            let v = |key: &str| if key == "mode" { mode } else { values(key) };
            let handles = dynamics_handles(&v, dynamics_layout(&v));
            let at = |name: &str| handles.iter().find(|h| h.name == name).unwrap().at;
            assert!((at("Makeup").0 - TRANSFER_WIDTH).abs() < 1e-6);
            assert!((at("Ratio").0 - at("Makeup").0).abs() > 0.05);
        }
    }

    #[test]
    fn the_ratio_point_follows_the_pointer_through_a_knee() {
        let v = |key: &str| match key {
            "knee" => 12.0,
            "makeup" => 3.0,
            _ => values(key),
        };
        let layout = dynamics_layout(&v);
        let c = Curve::read(&v);
        let level = ratio_level(&c);
        for ratio in [2.0f32, 8.0, 20.0] {
            let w = |key: &str| if key == "ratio" { ratio } else { v(key) };
            let at = dynamics_handles(&w, layout)[1].at;
            let got = dynamics_dragged(1, at, layout, &v)[0].1;
            assert!(
                (got / ratio - 1.0).abs() < 0.02,
                "{ratio}: {got} at {level}"
            );
        }
    }

    #[test]
    fn the_ratio_never_changes_the_layout_and_only_deepens_the_reduction() {
        for threshold in [-12.0f32, -18.0, -24.0, -40.0] {
            let mut last: Option<(AdsrLayout, f32)> = None;
            for step in 0..=40 {
                let ratio = 100f32.powf(step as f32 / 40.0);
                let v = |key: &str| match key {
                    "threshold" => threshold,
                    "ratio" => ratio,
                    _ => values(key),
                };
                let layout = dynamics_layout(&v);
                let y = red_y(Curve::read(&v).reduction(IN_RANGE.1), layout);
                if let Some((before, y_before)) = last {
                    assert_eq!(before, layout, "threshold {threshold}, ratio {ratio}");
                    assert!(
                        y <= y_before + 1e-6,
                        "{threshold} {ratio}: {y} > {y_before}"
                    );
                }
                last = Some((layout, y));
            }
        }
    }

    #[test]
    fn a_layout_is_kept_through_edits_that_stay_on_the_plot() {
        let mut layout = dynamics_layout(&values);
        let first = layout;
        // Attack and release wander, never off the plot nor to under a quarter.
        for (attack, release) in [(0.013, 0.2), (0.008, 0.24), (0.01, 0.17), (0.011, 0.25)] {
            let v = |key: &str| match key {
                "attack" => attack,
                "release" => release,
                _ => values(key),
            };
            let fit = dynamics_layout(&v);
            if layout.keeps(fit) {
                continue;
            }
            layout = fit;
        }
        assert_eq!(layout, first);
        // Past the edge of the plot, it refits.
        let long = |key: &str| if key == "release" { 3.0 } else { values(key) };
        assert!(!layout.keeps(dynamics_layout(&long)));
    }

    #[test]
    fn the_time_points_set_attack_and_release() {
        let layout = dynamics_layout(&values);
        let handles = dynamics_handles(&values, layout);
        let attack = dynamics_dragged(4, (time_x(0.05, layout), 0.5), layout, &values);
        assert!((attack[0].1 - 0.05).abs() < 1e-5);
        let release =
            dynamics_dragged(5, (time_x(layout.hold + 0.5, layout), 0.5), layout, &values);
        assert!((release[0].1 - 0.5).abs() < 1e-5);
        assert_eq!(handles.len(), 6);
    }

    #[test]
    fn a_limiter_and_a_gate_have_no_ratio_point() {
        for mode in [1.0, 2.0] {
            let v = |key: &str| if key == "mode" { mode } else { values(key) };
            let names: Vec<_> = dynamics_handles(&v, dynamics_layout(&v))
                .iter()
                .map(|h| h.name)
                .collect();
            assert!(!names.contains(&"Ratio"), "{names:?}");
        }
    }

    #[test]
    fn the_step_response_climbs_to_the_curves_reduction() {
        let c = Curve::read(&values);
        let layout = dynamics_layout(&values);
        let (end_of_attack, most) = step_reduction(&c, 0.01, 0.2, layout, layout.hold - 1e-6);
        assert!((most - 18.0).abs() < 1e-3, "24 dB over 4:1 is 18");
        assert!(end_of_attack > 0.95 * most);
        let (later, _) = step_reduction(&c, 0.01, 0.2, layout, layout.span);
        assert!(later < 0.3 * most);
    }

    #[test]
    fn eq_points_drag_in_frequency_and_gain_and_cuts_only_in_frequency() {
        let handles = eq_handles(&values);
        assert_eq!(handles.len(), 2);
        let bell = eq_dragged(0, handles[0].at, &values);
        assert!(
            bell.iter()
                .any(|&(k, v)| k == "freq1" && (v - 200.0).abs() < 0.5)
        );
        assert!(
            bell.iter()
                .any(|&(k, v)| k == "gain1" && (v - 6.0).abs() < 0.05)
        );
        // The low cut has no gain.
        let cut = eq_dragged(1, handles[1].at, &values);
        assert_eq!(cut.len(), 1);
        assert_eq!(cut[0].0, "freq2");
    }
}
