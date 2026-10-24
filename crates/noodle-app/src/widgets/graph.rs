//! Graphs of what a node does, drawn from its parameters: an ADSR's envelope,
//! a filter's frequency response, an LFO's wave.
//!
//! [`paint`] draws one small, for the body of a node. [`GraphEditor`] is the
//! full-size version in the properties panel, and the ADSR and the filters'
//! graphs have points to drag that set the parameters they stand for. As with
//! the other widgets, the editor reports [`Edit`]s and leaves the project to
//! the caller: a drag is a run of [`Edit::Drag`]s ended by [`Edit::EndDrag`],
//! so it undoes as one step.
//!
//! The shapes come from the node crate (`AdsrShape`, `svf_response`, and so
//! on), which tests them against the real DSP, so the graph is the sound's
//! shape, not an approximation of it.
//!
//! Wired parameters are drawn at the value their wire is delivering (the
//! caller says which) and their points can't be dragged.

use egui::epaint::PathShape;
use egui::{
    Align2, Color32, CursorIcon, FontId, Id, Painter, Pos2, Rect, Sense, Shape, Stroke, StrokeKind,
    Ui, WidgetInfo, WidgetType, pos2, vec2,
};
use noodle_core::{Command, NodeId};
use noodle_nodes::{AdsrShape, lfo_wave};

use super::mixing_graph;

use crate::session::Edit;
use crate::theme::{self, editor as colors};

/// How close, in points, the pointer must be to a point to grab it.
const GRAB_DISTANCE: f32 = 12.0;
/// The radius of a point's dot.
const DOT: f32 = 5.0;
/// The height of the full-size graph.
const FULL_HEIGHT: f32 = 120.0;
/// Which sample rate a filter's response is drawn at. The filters warp
/// frequency by the real rate, so at 44.1 kHz the curve differs a little
/// from this one above about 10 kHz.
const SAMPLE_RATE: f32 = 48_000.0;

/// The lowest and highest frequency a filter graph shows, in Hz.
const HZ_RANGE: (f32, f32) = (20.0, 20_000.0);
/// The lowest and highest gain a filter graph shows, in dB.
const DB_RANGE: (f32, f32) = (-48.0, 36.0);
/// The resonance past which the ladder, treated as linear, has no finite
/// peak: it rings by itself from here, and the graph shows the peak off the
/// top.
const LADDER_RINGS: f32 = noodle_nodes::LADDER_SELF_OSCILLATES;
/// How far the plot is inset from the graph's edge, so the points at its
/// edges (a full-height attack, a zero release) are drawn whole.
const INSET: egui::Vec2 = vec2(2.0, 8.0);

/// The kinds of node that have a graph.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Adsr,
    Svf,
    Ladder,
    Lfo,
    Dynamics,
    Eq,
}

impl Kind {
    /// What the graph is called, for screen readers and tests.
    pub fn label(self) -> &'static str {
        match self {
            Self::Adsr => "Envelope graph",
            Self::Svf | Self::Ladder | Self::Eq => "Frequency response graph",
            Self::Dynamics => "Dynamics graph",
            Self::Lfo => "Wave graph",
        }
    }
}

impl From<noodle_engine::GraphKind> for Kind {
    fn from(kind: noodle_engine::GraphKind) -> Self {
        use noodle_engine::GraphKind;
        match kind {
            GraphKind::Adsr => Self::Adsr,
            GraphKind::Svf => Self::Svf,
            GraphKind::Ladder => Self::Ladder,
            GraphKind::Lfo => Self::Lfo,
            GraphKind::Dynamics => Self::Dynamics,
            GraphKind::Eq => Self::Eq,
        }
    }
}

/// A parameter's current value by its key: what the graph is drawn from.
pub type Values<'a> = &'a dyn Fn(&str) -> f32;

/// A point in a plot: both from 0 to 1, with y up.
pub(super) type P = (f32, f32);

/// How the ADSR's time axis is laid out, in seconds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct AdsrLayout {
    /// The time the plot's full width stands for.
    pub(super) span: f32,
    /// How long the held level is drawn between the decay and the release.
    pub(super) hold: f32,
    /// How many dB of gain reduction the Dynamics step response covers
    /// (unused by the ADSR).
    pub(super) depth: f32,
}

impl AdsrLayout {
    /// Whether this layout, drawn before, still suits `fit`, the layout the
    /// values ask for now: what the values need fits (the fit has room to
    /// spare, so a little growth is still inside the plot) and nothing has
    /// shrunk to under a quarter of the axis. Keeping it stops the plot
    /// rescaling as values are edited.
    pub(super) fn keeps(&self, fit: AdsrLayout) -> bool {
        let holds = |have: f32, want: f32, grow: f32| have * grow >= want && want >= have / 4.0;
        holds(self.span, fit.span, 1.3)
            && holds(self.hold, fit.hold, 2.0)
            && holds(self.depth, fit.depth, 1.3)
    }
}

/// Fits the whole envelope, with a little sustain and some room to grow.
fn adsr_layout(values: Values<'_>) -> AdsrLayout {
    let (a, d, r) = (values("attack"), values("decay"), values("release"));
    let hold = (0.25 * (a + d + r)).max(0.02);
    AdsrLayout {
        span: (a + d + hold + r) * 1.2,
        hold,
        depth: 0.0,
    }
}

/// The time axis for the graph of `kind`.
fn layout_for(kind: Kind, values: Values<'_>) -> AdsrLayout {
    match kind {
        Kind::Dynamics => mixing_graph::dynamics_layout(values),
        _ => adsr_layout(values),
    }
}

/// The values the graphs read that are not parameters: `bands`, the EQ's band
/// count (config), and `@level` and `@reduction`, what a Dynamics node is
/// doing now (NaN when it hasn't reported). `live` reads a telemetry channel
/// of the node.
pub fn special_value(
    key: &str,
    node: &noodle_core::Node,
    live: &dyn Fn(&str) -> Option<f32>,
) -> Option<f32> {
    match key {
        "bands" => Some(
            node.config
                .get("bands")
                .and_then(noodle_core::Value::as_int)
                .unwrap_or(4) as f32,
        ),
        "@level" => Some(live("level").unwrap_or(f32::NAN)),
        "@reduction" => Some(live("reduction").unwrap_or(0.0)),
        _ => None,
    }
}

/// What a graph reads for `key` of `node`, shared by the node's body and the
/// properties panel so both draw one curve: the special values, else a wired
/// parameter's live value, else the node's own value, else `default`.
/// `live` reads the node's telemetry channel `key`.
pub fn graph_value(
    key: &str,
    node: &noodle_core::Node,
    wired: bool,
    live: &dyn Fn(&str) -> Option<f32>,
    default: f32,
) -> f32 {
    if let Some(value) = special_value(key, node, live) {
        return value;
    }
    let own = node.params.get(key).copied().unwrap_or(default);
    if wired { live(key).unwrap_or(own) } else { own }
}

fn adsr_shape(values: Values<'_>) -> AdsrShape {
    AdsrShape {
        attack: values("attack"),
        decay: values("decay"),
        sustain: values("sustain"),
        release: values("release"),
    }
}

/// Where the LFO's output `offset + depth × wave` sits on its plot, which
/// runs from -2 to 2 because both can be as large as 1.
fn lfo_y(offset: f32, depth: f32, wave: f32) -> f32 {
    ((offset + depth * wave).clamp(-2.0, 2.0) + 2.0) / 4.0
}

/// "100 Hz", "2.5 kHz".
pub(super) fn hz_text(hz: f32) -> String {
    if hz >= 1_000.0 {
        format!("{} kHz", (hz / 10.0).round() / 100.0)
    } else {
        format!("{} Hz", hz.round())
    }
}

/// "0", "250 ms", "1.5 s".
pub(super) fn seconds_text(seconds: f32) -> String {
    if seconds == 0.0 {
        "0".to_owned()
    } else if seconds < 1.0 {
        format!("{} ms", (seconds * 1_000.0).round())
    } else {
        format!("{} s", (seconds * 100.0).round() / 100.0)
    }
}

/// How far along a log axis `hz` is.
pub(super) fn hz_to_x(hz: f32) -> f32 {
    ((hz / HZ_RANGE.0).ln() / (HZ_RANGE.1 / HZ_RANGE.0).ln()).clamp(0.0, 1.0)
}

pub(super) fn x_to_hz(x: f32) -> f32 {
    HZ_RANGE.0 * (HZ_RANGE.1 / HZ_RANGE.0).powf(x.clamp(0.0, 1.0))
}

fn gain_to_y(gain: f32) -> f32 {
    let db = 20.0 * gain.max(1e-6).log10();
    ((db - DB_RANGE.0) / (DB_RANGE.1 - DB_RANGE.0)).clamp(0.0, 1.0)
}

fn y_to_db(y: f32) -> f32 {
    DB_RANGE.0 + y * (DB_RANGE.1 - DB_RANGE.0)
}

/// The gain, linear, of the low-pass response of a filter at its cutoff.
fn gain_at_cutoff(kind: Kind, cutoff: f32, resonance: f32) -> f32 {
    match kind {
        Kind::Ladder => noodle_nodes::ladder_response(cutoff, resonance, cutoff, SAMPLE_RATE),
        _ => noodle_nodes::svf_response(cutoff, resonance, cutoff, SAMPLE_RATE)[0],
    }
}

/// The resonance that puts the response at the cutoff at `db`, or the most
/// the filter has when `db` is out of reach.
fn resonance_for(kind: Kind, cutoff: f32, db: f32) -> f32 {
    let top = if kind == Kind::Ladder {
        LADDER_RINGS
    } else {
        1.0
    };
    let db_at = |r: f32| 20.0 * gain_at_cutoff(kind, cutoff, r).max(1e-6).log10();
    if db >= DB_RANGE.1 || db >= db_at(top) {
        return 1.0;
    }
    if db <= db_at(0.0) {
        return 0.0;
    }
    // The response at the cutoff rises with resonance up to `top`.
    let (mut low, mut high) = (0.0f32, top);
    for _ in 0..24 {
        let middle = 0.5 * (low + high);
        if db_at(middle) < db {
            low = middle;
        } else {
            high = middle;
        }
    }
    0.5 * (low + high)
}

/// A point on a graph that sets parameters when dragged.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Handle {
    pub(super) at: P,
    /// The parameters it sets: the first moves with x, the second with y. It
    /// moves along whichever of them are free to change.
    pub(super) keys: &'static [&'static str],
    /// What to call it.
    pub(super) name: &'static str,
}

fn handles(kind: Kind, values: Values<'_>, layout: AdsrLayout) -> Vec<Handle> {
    match kind {
        Kind::Adsr => {
            let (a, d, s, r) = (
                values("attack"),
                values("decay"),
                values("sustain").clamp(0.0, 1.0),
                values("release"),
            );
            let x = |t: f32| t / layout.span;
            vec![
                Handle {
                    at: (x(a), 1.0),
                    keys: &["attack"],
                    name: "Attack",
                },
                Handle {
                    at: (x(a + d), s),
                    keys: &["decay", "sustain"],
                    name: "Decay and sustain",
                },
                Handle {
                    at: (x(a + d + layout.hold + r), 0.0),
                    keys: &["release"],
                    name: "Release",
                },
            ]
        }
        Kind::Svf | Kind::Ladder => {
            let cutoff = values("cutoff");
            let gain = gain_at_cutoff(kind, cutoff, values("resonance"));
            vec![Handle {
                at: (hz_to_x(cutoff), gain_to_y(gain)),
                keys: &["cutoff", "resonance"],
                name: "Cutoff and resonance",
            }]
        }
        Kind::Lfo => Vec::new(),
        Kind::Dynamics => mixing_graph::dynamics_handles(values, layout),
        Kind::Eq => mixing_graph::eq_handles(values),
    }
}

/// What dragging handle `which` to plot position `at` sets. `range` gives
/// each parameter's lowest and highest value. Only the values that change
/// are returned.
fn dragged(
    kind: Kind,
    which: usize,
    at: P,
    layout: AdsrLayout,
    values: Values<'_>,
    range: &dyn Fn(&str) -> (f32, f32),
    editable: &dyn Fn(&str) -> bool,
) -> Vec<(&'static str, f32)> {
    let clamped = |key: &'static str, value: f32| {
        let (low, high) = range(key);
        (key, value.clamp(low, high))
    };
    let out = match (kind, which) {
        (Kind::Adsr, 0) => vec![clamped("attack", at.0 * layout.span)],
        (Kind::Adsr, 1) => {
            let attack = values("attack");
            vec![
                clamped("decay", at.0 * layout.span - attack),
                clamped("sustain", at.1),
            ]
        }
        (Kind::Adsr, 2) => {
            let before = values("attack") + values("decay") + layout.hold;
            vec![clamped("release", at.0 * layout.span - before)]
        }
        (Kind::Svf | Kind::Ladder, 0) => {
            // A wired cutoff stays where it is, and the height is judged there.
            let cutoff = if editable("cutoff") {
                clamped("cutoff", x_to_hz(at.0)).1
            } else {
                values("cutoff")
            };
            let resonance = resonance_for(kind, cutoff, y_to_db(at.1.clamp(0.0, 1.0)));
            vec![("cutoff", cutoff), clamped("resonance", resonance)]
        }
        (Kind::Dynamics, _) => mixing_graph::dynamics_dragged(which, at, layout, values)
            .into_iter()
            .map(|(key, value)| clamped(key, value))
            .collect(),
        (Kind::Eq, _) => mixing_graph::eq_dragged(which, at, values)
            .into_iter()
            .map(|(key, value)| clamped(key, value))
            .collect(),
        _ => Vec::new(),
    };
    out.into_iter()
        .filter(|&(key, value)| editable(key) && value != values(key))
        .collect()
}

/// Draws the graph small, for the body of a node: no handles, just the shape.
pub fn paint(painter: &Painter, area: Rect, kind: Kind, zoom: f32, values: Values<'_>) {
    painter.rect_filled(area, 2.0 * zoom, colors::BODY);
    let plot = area.shrink(3.0 * zoom);
    let layout = layout_for(kind, values);
    plot_contents(painter, plot, kind, values, layout, zoom.max(0.5), false);
}

/// Where a point of the plot is, in `rect`.
pub(super) fn to_screen(rect: Rect, (x, y): P) -> Pos2 {
    pos2(
        rect.left() + x * rect.width(),
        rect.bottom() - y * rect.height(),
    )
}

/// The shape itself, without handles. `full` adds the grid and labels.
fn plot_contents(
    painter: &Painter,
    rect: Rect,
    kind: Kind,
    values: Values<'_>,
    layout: AdsrLayout,
    scale: f32,
    full: bool,
) {
    let painter = painter.with_clip_rect(rect.expand(1.0));
    let line = Stroke::new(if full { 2.0 } else { 1.5 * scale }, theme::ACCENT);
    let grid = Stroke::new(1.0, colors::GRID_MAJOR);
    let weak = colors::TEXT_WEAK;
    let label = |at: Pos2, align: Align2, text: &str| {
        painter.text(at, align, text, FontId::proportional(10.0), weak);
    };
    let hline = |y: f32, stroke: Stroke| {
        painter.line_segment(
            [to_screen(rect, (0.0, y)), to_screen(rect, (1.0, y))],
            stroke,
        );
    };
    let vline = |x: f32, stroke: Stroke| {
        painter.line_segment(
            [to_screen(rect, (x, 0.0)), to_screen(rect, (x, 1.0))],
            stroke,
        );
    };
    let polyline = |points: &[P], stroke: Stroke| {
        let points = points.iter().map(|&p| to_screen(rect, p)).collect();
        painter.add(PathShape::line(points, stroke));
    };

    match kind {
        Kind::Adsr => {
            let shape = adsr_shape(values);
            let gate = shape.attack + shape.decay + layout.hold;
            let points: Vec<P> = (0..=200)
                .map(|i| {
                    let t = layout.span * i as f32 / 200.0;
                    (i as f32 / 200.0, shape.level(t, gate).clamp(0.0, 1.0))
                })
                .collect();
            if full {
                hline(0.0, grid);
                hline(1.0, grid);
                // Where the key is let go.
                vline(gate / layout.span, grid);
                label(
                    to_screen(rect, (gate / layout.span, 1.0)) + vec2(3.0, 1.0),
                    Align2::LEFT_TOP,
                    "key up",
                );
            }
            let fill = Color32::from_rgba_unmultiplied(200, 241, 105, 28);
            // Filled in strips, because the area under the curve isn't convex.
            for pair in points.windows(2) {
                let [(x0, y0), (x1, y1)] = [pair[0], pair[1]];
                painter.add(Shape::convex_polygon(
                    [(x0, 0.0), (x0, y0), (x1, y1), (x1, 0.0)]
                        .map(|p| to_screen(rect, p))
                        .to_vec(),
                    fill,
                    Stroke::NONE,
                ));
            }
            polyline(&points, line);
        }
        Kind::Svf | Kind::Ladder => {
            let (cutoff, resonance) = (values("cutoff"), values("resonance"));
            let curve = |pick: &dyn Fn(f32) -> f32| -> Vec<P> {
                (0..=160)
                    .map(|i| {
                        let x = i as f32 / 160.0;
                        (x, gain_to_y(pick(x_to_hz(x))))
                    })
                    .collect()
            };
            if full {
                let faint = Stroke::new(1.0, colors::GRID);
                for hz in [
                    30.0, 40.0, 50.0, 60.0, 70.0, 80.0, 90.0, 200.0, 300.0, 400.0, 600.0, 700.0,
                    800.0, 900.0, 2_000.0, 3_000.0, 4_000.0, 6_000.0, 7_000.0, 8_000.0, 9_000.0,
                ] {
                    vline(hz_to_x(hz), faint);
                }
                for hz in [100.0, 500.0, 1_000.0, 5_000.0, 10_000.0] {
                    let x = hz_to_x(hz);
                    vline(x, grid);
                    label(
                        to_screen(rect, (x, 0.0)) + vec2(2.0, -1.0),
                        Align2::LEFT_BOTTOM,
                        &hz_text(hz),
                    );
                }
                for db in [-24.0f32, 0.0, 24.0] {
                    let y = (db - DB_RANGE.0) / (DB_RANGE.1 - DB_RANGE.0);
                    hline(y, grid);
                    label(
                        to_screen(rect, (0.0, y)) + vec2(2.0, -1.0),
                        Align2::LEFT_BOTTOM,
                        &format!("{db:+} dB").replace("+0", "0"),
                    );
                }
                // Where the cutoff is.
                let x = hz_to_x(cutoff);
                painter.line_segment(
                    [to_screen(rect, (x, 0.0)), to_screen(rect, (x, 1.0))],
                    Stroke::new(1.0, theme::ACCENT.gamma_multiply(0.5)),
                );
                let right = x < 0.7;
                painter.text(
                    to_screen(rect, (x, 1.0)) + vec2(if right { 3.0 } else { -3.0 }, 2.0),
                    if right {
                        Align2::LEFT_TOP
                    } else {
                        Align2::RIGHT_TOP
                    },
                    hz_text(cutoff),
                    FontId::proportional(10.0),
                    theme::ACCENT,
                );
            }
            if kind == Kind::Svf {
                let response =
                    |hz: f32| noodle_nodes::svf_response(cutoff, resonance, hz, SAMPLE_RATE);
                polyline(
                    &curve(&|hz| response(hz)[1]),
                    Stroke::new(line.width * 0.6, colors::WIRE),
                );
                polyline(
                    &curve(&|hz| response(hz)[2]),
                    Stroke::new(line.width * 0.6, colors::EVENT_WIRE),
                );
                if full {
                    let at = to_screen(rect, (1.0, 1.0)) + vec2(-4.0, 3.0);
                    for (i, (text, color)) in [
                        ("High", colors::EVENT_WIRE),
                        ("Band", colors::WIRE),
                        ("Low", theme::ACCENT),
                    ]
                    .into_iter()
                    .enumerate()
                    {
                        painter.text(
                            at + vec2(-34.0 * i as f32, 0.0),
                            Align2::RIGHT_TOP,
                            text,
                            FontId::proportional(10.0),
                            color,
                        );
                    }
                }
                polyline(&curve(&|hz| response(hz)[0]), line);
            } else {
                polyline(
                    &curve(&|hz| noodle_nodes::ladder_response(cutoff, resonance, hz, SAMPLE_RATE)),
                    line,
                );
            }
        }
        Kind::Dynamics => {
            mixing_graph::paint_dynamics(&painter, rect, values, layout, scale, full);
            return;
        }
        Kind::Eq => {
            mixing_graph::paint_eq(&painter, rect, values, scale, full);
        }
        Kind::Lfo => {
            // Two cycles, from the wave's phase.
            const CYCLES: f32 = 2.0;
            let (depth, offset) = (values("depth"), values("offset"));
            let (shape, phase) = (values("shape"), values("phase"));
            // The output is `offset + depth × wave`, which reaches 2 either way.
            let y = |wave: f32| lfo_y(offset, depth, wave);
            if full {
                for (value, text) in [(1.0f32, "+1"), (0.0, "0"), (-1.0, "-1")] {
                    let y = (value + 1.0) / 2.0;
                    hline(y, grid);
                    let align = if y >= 1.0 {
                        Align2::LEFT_TOP
                    } else {
                        Align2::LEFT_BOTTOM
                    };
                    let nudge = if y >= 1.0 { 2.0 } else { -1.0 };
                    label(to_screen(rect, (0.0, y)) + vec2(2.0, nudge), align, text);
                }
                // One tick a cycle, labelled in seconds from the rate.
                let rate = values("rate").max(1e-3);
                for cycle in 0..=2 {
                    let x = cycle as f32 / CYCLES;
                    if cycle > 0 {
                        vline(x, grid);
                    }
                    let text = seconds_text(cycle as f32 / rate);
                    // Clear of the "-1" at the left edge.
                    let (align, nudge) = match cycle {
                        0 => (Align2::LEFT_BOTTOM, 14.0),
                        2 => (Align2::RIGHT_BOTTOM, -2.0),
                        _ => (Align2::CENTER_BOTTOM, 0.0),
                    };
                    label(to_screen(rect, (x, 0.0)) + vec2(nudge, -1.0), align, &text);
                }
            }
            let points: Vec<P> = if lfo_wave(shape, 0.0).is_some() {
                (0..=240)
                    .map(|i| {
                        let x = i as f32 / 240.0;
                        let wave = lfo_wave(shape, x * CYCLES + phase).unwrap_or(0.0);
                        (x, y(wave))
                    })
                    .collect()
            } else {
                // A random wave holds a new value each cycle, so any run of
                // values shows the idea.
                const HELD: [f32; 2] = [0.55, -0.4];
                let mut points = Vec::new();
                for (cycle, wave) in HELD.into_iter().enumerate() {
                    let (from, to) = (cycle as f32 / CYCLES, (cycle + 1) as f32 / CYCLES);
                    points.push((from, y(wave)));
                    points.push((to, y(wave)));
                }
                points
            };
            polyline(&points, line);
        }
    }
    if full {
        painter.rect_stroke(
            rect,
            0.0,
            Stroke::new(1.0, colors::NODE_OUTLINE),
            StrokeKind::Inside,
        );
    }
}

/// The full-size graph in the properties panel, with its points to drag.
#[must_use = "call `show` to draw the graph"]
pub struct GraphEditor<'a> {
    node: NodeId,
    kind: Kind,
    values: Values<'a>,
    /// Each parameter's lowest and highest value.
    range: &'a dyn Fn(&str) -> (f32, f32),
    /// Whether a parameter is free to change (it has no wire into it).
    editable: &'a dyn Fn(&str) -> bool,
}

/// A drag in progress: the handle, and the time axis it started with, which
/// holds still so the point follows the pointer.
#[derive(Clone, Copy)]
struct Drag {
    handle: usize,
    layout: AdsrLayout,
}

impl<'a> GraphEditor<'a> {
    pub fn new(
        node: NodeId,
        kind: Kind,
        values: Values<'a>,
        range: &'a dyn Fn(&str) -> (f32, f32),
        editable: &'a dyn Fn(&str) -> bool,
    ) -> Self {
        Self {
            node,
            kind,
            values,
            range,
            editable,
        }
    }

    pub fn show(self, ui: &mut Ui) -> Vec<Edit> {
        let size = vec2(ui.available_width().max(60.0), FULL_HEIGHT);
        let (outer, _) = ui.allocate_exact_size(size, Sense::hover());
        let rect = outer.shrink2(INSET);
        let id = Id::new((self.node, "graph"));
        let response = ui.interact(outer, id, Sense::click_and_drag());
        let label = self.kind.label();
        response.widget_info(|| WidgetInfo::labeled(WidgetType::Other, true, label));

        let drag_id = id.with("drag");
        let from_screen = |p: Pos2| -> P {
            (
                (p.x - rect.left()) / rect.width(),
                (rect.bottom() - p.y) / rect.height(),
            )
        };
        let mut edits = Vec::new();
        let mut layout = layout_for(self.kind, self.values);
        let held = ui.data(|d| d.get_temp::<Option<Drag>>(drag_id)).flatten();
        let shown_id = id.with("layout");
        if self.kind == Kind::Dynamics
            && let Some(shown) = ui.data(|d| d.get_temp::<AdsrLayout>(shown_id))
            && shown.keeps(layout)
        {
            layout = shown;
        }
        if let Some(drag) = held {
            layout = drag.layout;
        }
        ui.data_mut(|d| d.insert_temp(shown_id, layout));
        let points = handles(self.kind, self.values, layout);
        let free = |h: &Handle| h.keys.iter().any(|key| (self.editable)(key));
        let nearest = |at: Pos2| {
            points
                .iter()
                .enumerate()
                .filter(|(_, h)| free(h))
                .map(|(i, h)| (i, at.distance(to_screen(rect, h.at))))
                .filter(|&(_, distance)| distance <= GRAB_DISTANCE)
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .map(|(i, _)| i)
        };

        if response.drag_started() {
            let origin = ui.input(|i| i.pointer.press_origin());
            let drag = origin
                .and_then(nearest)
                .map(|handle| Drag { handle, layout });
            ui.data_mut(|d| d.insert_temp(drag_id, drag));
        }
        if response.dragged()
            && let Some(drag) = ui.data(|d| d.get_temp::<Option<Drag>>(drag_id)).flatten()
            && let Some(pos) = response.interact_pointer_pos()
        {
            for (key, value) in dragged(
                self.kind,
                drag.handle,
                from_screen(pos),
                drag.layout,
                self.values,
                self.range,
                self.editable,
            ) {
                edits.push(Edit::Drag(Command::SetParam {
                    node: self.node,
                    key: key.to_owned(),
                    value: Some(value),
                }));
            }
        }
        if response.drag_stopped() {
            let took = ui.data_mut(|d| d.remove_temp::<Option<Drag>>(drag_id));
            if took.flatten().is_some() {
                edits.push(Edit::EndDrag);
            }
        }

        let hovered = held
            .map(|drag| drag.handle)
            .or_else(|| response.hover_pos().and_then(nearest));
        if let Some(i) = hovered {
            ui.ctx().set_cursor_icon(CursorIcon::Grab);
            if held.is_none() {
                let handle = &points[i];
                let text = handle
                    .keys
                    .iter()
                    .map(|key| format!("{key}: {:.3}", (self.values)(key)))
                    .collect::<Vec<_>>()
                    .join(", ");
                response
                    .clone()
                    .on_hover_text(format!("{}\n{text}", handle.name));
            }
        }

        let painter = ui.painter_at(outer);
        painter.rect_filled(rect, 4.0, theme::CANVAS);
        plot_contents(&painter, rect, self.kind, self.values, layout, 1.0, true);
        for (i, handle) in points.iter().enumerate() {
            let at = to_screen(rect, handle.at);
            let alive = free(handle);
            let radius = if hovered == Some(i) { DOT + 1.5 } else { DOT };
            let fill = if alive {
                Color32::WHITE
            } else {
                colors::TEXT_WEAK
            };
            painter.circle_filled(at, radius, fill);
            painter.circle_stroke(at, radius, Stroke::new(1.5, theme::CANVAS));
        }
        edits
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn defaults(key: &str) -> f32 {
        match key {
            "attack" => 0.1,
            "decay" => 0.2,
            "sustain" => 0.5,
            "release" => 0.4,
            "cutoff" => 1_000.0,
            "resonance" => 0.0,
            _ => 0.0,
        }
    }

    fn anywhere(key: &str) -> (f32, f32) {
        match key {
            "cutoff" => (20.0, 20_000.0),
            "sustain" | "resonance" => (0.0, 1.0),
            _ => (0.001, 10.0),
        }
    }

    #[test]
    fn dragging_each_envelope_point_sets_what_it_stands_for() {
        let layout = adsr_layout(&defaults);
        let points = handles(Kind::Adsr, &defaults, layout);
        assert_eq!(points.len(), 3);
        // Dragging a point to where it already is changes nothing.
        for (i, h) in points.iter().enumerate() {
            let same = dragged(Kind::Adsr, i, h.at, layout, &defaults, &anywhere, &|_| true);
            assert!(
                same.iter()
                    .all(|&(key, value)| (value - defaults(key)).abs() < 1e-4),
                "{i}: {same:?}"
            );
        }
        // Attack: along the time axis.
        let out = dragged(
            Kind::Adsr,
            0,
            (0.3 / layout.span, 1.0),
            layout,
            &defaults,
            &anywhere,
            &|_| true,
        );
        assert_eq!(out.len(), 1);
        assert!((out[0].1 - 0.3).abs() < 1e-5);
        // Decay and sustain: time after the attack, and height.
        let out = dragged(
            Kind::Adsr,
            1,
            (0.6 / layout.span, 0.25),
            layout,
            &defaults,
            &anywhere,
            &|_| true,
        );
        assert!(out.contains(&("sustain", 0.25)));
        assert!(
            out.iter()
                .any(|&(k, v)| k == "decay" && (v - 0.5).abs() < 1e-5)
        );
        // Release: time after the held level.
        let end = 0.1 + 0.2 + layout.hold + 1.5;
        let out = dragged(
            Kind::Adsr,
            2,
            (end / layout.span, 0.0),
            layout,
            &defaults,
            &anywhere,
            &|_| true,
        );
        assert!(
            out.iter()
                .any(|&(k, v)| k == "release" && (v - 1.5).abs() < 1e-4)
        );
    }

    #[test]
    fn a_drag_stays_within_the_parameters_range() {
        let layout = adsr_layout(&defaults);
        // Far to the right and below: past the longest time, below zero.
        let out = dragged(
            Kind::Adsr,
            1,
            (50.0, -2.0),
            layout,
            &defaults,
            &anywhere,
            &|_| true,
        );
        assert!(out.contains(&("decay", 10.0)));
        assert!(out.contains(&("sustain", 0.0)));
        let out = dragged(
            Kind::Adsr,
            0,
            (-5.0, 1.0),
            layout,
            &defaults,
            &anywhere,
            &|_| true,
        );
        assert_eq!(out, vec![("attack", 0.001)]);
    }

    #[test]
    fn dragging_the_filter_point_sets_cutoff_and_resonance() {
        for kind in [Kind::Svf, Kind::Ladder] {
            let resonant = |key: &str| {
                if key == "resonance" {
                    0.6
                } else {
                    defaults(key)
                }
            };
            let layout = adsr_layout(&resonant);
            let point = handles(kind, &resonant, layout)[0].clone();
            // Dragging to where the point is leaves the settings as they were.
            let same = dragged(kind, 0, point.at, layout, &resonant, &anywhere, &|_| true);
            for (key, value) in same {
                let before = resonant(key);
                assert!(
                    (value - before).abs() <= 1e-3 * before.abs().max(1.0),
                    "{kind:?} {key}: {value} vs {before}"
                );
            }
            // Dragging up raises the resonance, and right the cutoff.
            let up = dragged(
                kind,
                0,
                (point.at.0 + 0.1, point.at.1 + 0.1),
                layout,
                &resonant,
                &anywhere,
                &|_| true,
            );
            let get = |key: &str| up.iter().find(|(k, _)| *k == key).map(|&(_, v)| v);
            assert!(get("cutoff").unwrap() > 1_000.0);
            assert!(get("resonance").unwrap() > 0.6);
            // Dragging to the floor is no resonance.
            let down = dragged(
                kind,
                0,
                (point.at.0, 0.0),
                layout,
                &resonant,
                &anywhere,
                &|_| true,
            );
            assert!(down.contains(&("resonance", 0.0)));
        }
    }

    #[test]
    fn the_filter_point_sits_on_the_curve_at_the_cutoff() {
        let resonant = |key: &str| {
            if key == "resonance" {
                0.8
            } else {
                defaults(key)
            }
        };
        let svf = handles(Kind::Svf, &resonant, adsr_layout(&resonant))[0].clone();
        let low = noodle_nodes::svf_response(1_000.0, 0.8, 1_000.0, SAMPLE_RATE)[0];
        assert!((svf.at.1 - gain_to_y(low)).abs() < 1e-6);
        assert!((svf.at.0 - hz_to_x(1_000.0)).abs() < 1e-6);
    }

    #[test]
    fn axis_labels_read_naturally() {
        assert_eq!(hz_text(100.0), "100 Hz");
        assert_eq!(hz_text(1_000.0), "1 kHz");
        assert_eq!(hz_text(2_500.0), "2.5 kHz");
        assert_eq!(seconds_text(0.0), "0");
        assert_eq!(seconds_text(0.25), "250 ms");
        assert_eq!(seconds_text(2.0), "2 s");
    }

    #[test]
    fn an_lfo_above_one_is_not_drawn_clipped() {
        // Offset 0.5 and depth 1: the top of a sine is 1.5, not 1.
        assert!((lfo_y(0.5, 1.0, 1.0) - 0.875).abs() < 1e-6);
        assert!((lfo_y(0.0, 1.0, 0.0) - 0.5).abs() < 1e-6);
        assert!(lfo_y(0.5, 1.0, 1.0) > lfo_y(0.0, 1.0, 1.0));
        assert_eq!(lfo_y(1.0, 1.0, 1.0), 1.0);
    }

    #[test]
    fn a_wired_parameter_reads_live_and_an_unwired_one_its_own_value() {
        let mut node = noodle_core::Node::new("noodle.eq");
        node.params.insert("freq4".to_owned(), 1_800.0);
        let live = |_: &str| Some(157.0);
        assert_eq!(graph_value("freq4", &node, true, &live, 100.0), 157.0);
        assert_eq!(graph_value("freq4", &node, false, &live, 100.0), 1_800.0);
        assert_eq!(graph_value("freq1", &node, true, &|_| None, 100.0), 100.0);
    }
}
