//! The seconds-axis audio pane, shared by the clip editor (for a clip that
//! isn't warped) and the sample view (a Sampler's sample).
//!
//! A clip and a sample are the same thing here: a stretch of a file that is
//! lit (the part that plays), with the rest dimmed, and optionally a loop
//! inside it. The pane draws the waveform on a seconds ruler (the sample
//! curve once zoomed in far enough), zooms and pans it, and turns drags into
//! [`Gesture`]s: the lit part's edges and the loop's ends can be pulled, and
//! dragging anywhere else slips the audio under the lit part. What those
//! gestures *change* is up to the caller: a clip's offset and length, or a
//! Sampler's `start` and `end` parameters.

use std::path::Path;

use egui::{
    Align2, Color32, CursorIcon, FontId, Pos2, Rect, Response, Sense, Stroke, Ui, Vec2, vec2,
};
use noodle_io::Peaks;
use noodle_nodes::Sample;

use super::{MAX_COLUMNS, MAX_PPB, MIN_PPB, RULER, curve};
use crate::theme::{self, timeline as colors};

/// How close, in points, a press is to an edge or a loop end to take it.
const GRAB: f32 = 7.0;
/// The least, in seconds, the lit part or the loop is long.
pub(crate) const MIN_SECONDS: f64 = 0.001;

/// What the pane can pull on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Edge {
    Start,
    End,
    LoopStart,
    LoopEnd,
}

impl Edge {
    const ALL: [Edge; 4] = [Self::Start, Self::End, Self::LoopStart, Self::LoopEnd];

    fn colour(self) -> Color32 {
        match self {
            Self::Start | Self::End => theme::ACCENT,
            Self::LoopStart | Self::LoopEnd => Color32::from_rgb(109, 140, 255),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Start => "Start",
            Self::End => "End",
            Self::LoopStart => "Loop start",
            Self::LoopEnd => "Loop end",
        }
    }

    fn is_start(self) -> bool {
        matches!(self, Self::Start | Self::LoopStart)
    }

    fn is_loop(self) -> bool {
        matches!(self, Self::LoopStart | Self::LoopEnd)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Grab {
    /// The waveform, pressed at screen x `press_x`.
    Slip {
        press_x: f32,
    },
    Edge(Edge),
}

/// Zoom, scroll and the drag in progress.
pub(crate) struct View {
    /// Points per second (per beat, in the clip editor's warped view).
    pub ppb: f32,
    pub scroll_x: f32,
    /// Fit the audio to the width on the next frame.
    pub focus: bool,
    /// The samples under the view, read when zoomed far in.
    pub(super) samples: Option<curve::Samples>,
    grab: Option<Grab>,
}

impl Default for View {
    fn default() -> Self {
        Self {
            ppb: 60.0,
            scroll_x: 0.0,
            focus: false,
            samples: None,
            grab: None,
        }
    }
}

impl View {
    /// Whether a drag is under way.
    pub fn dragging(&self) -> bool {
        self.grab.is_some()
    }

    /// Lets go of any drag, returning whether there was one.
    pub fn drop_grab(&mut self) -> bool {
        self.grab.take().is_some()
    }
}

/// Where the seconds are on the screen: second 0 is `zero` seconds into the
/// file.
#[derive(Clone, Copy)]
pub(crate) struct Axis {
    pub origin: f32,
    pub pps: f32,
}

impl Axis {
    pub fn x(self, seconds: f64) -> f32 {
        self.origin + seconds as f32 * self.pps
    }
}

/// Where the samples for the zoomed-in curve come from.
#[derive(Clone, Copy)]
pub(crate) enum CurveSource<'a> {
    /// Read from the file as needed.
    File(&'a Path),
    /// Already in memory.
    Memory(&'a Sample),
}

/// What to draw.
pub(crate) struct Spec<'a> {
    pub peaks: Option<&'a Peaks>,
    pub curve: CurveSource<'a>,
    pub rate: f64,
    pub frames: u64,
    /// The second of the file that is drawn at time 0.
    pub zero: f64,
    /// The part of the file that plays, in seconds of the file.
    pub lit: (f64, f64),
    /// The loop's ends, in seconds of the file, if it is on.
    pub looping: Option<(f64, f64)>,
    pub gain: f32,
    /// The playheads, in seconds of the file: a clip has at most one, a
    /// Sampler one for each sounding voice.
    pub playheads: &'a [f64],
}

/// What the user did in the pane.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Gesture {
    /// A drag began (on an edge or the waveform).
    Begin,
    /// The waveform is `seconds` to the right of where it was pressed.
    Slip(f32),
    /// An edge was dragged to this second of the file.
    Edge(Edge, f64),
    /// The drag ended.
    Release,
}

pub(crate) struct Out {
    pub gestures: Vec<Gesture>,
    pub wave: Rect,
    pub axis: Axis,
    pub response: Response,
}

/// Draws the pane into `body` (everything under a toolbar).
pub(crate) fn show(ui: &mut Ui, view: &mut View, body: Rect, spec: &Spec) -> Out {
    let rate = spec.rate.max(1.0);
    let file_secs = spec.frames as f64 / rate;
    let ruler = Rect::from_min_size(body.min, vec2(body.width(), RULER));
    let wave = Rect::from_min_max(Pos2::new(body.left(), ruler.bottom()), body.max);
    let area = Rect::from_min_max(ruler.min, body.max);
    let (lit_from, lit_to) = (spec.lit.0 - spec.zero, spec.lit.1 - spec.zero);

    // The file runs from -zero to (file - zero) around time 0.
    let extent = (-spec.zero, file_secs - spec.zero);
    if view.focus {
        view.focus = false;
        let span = (extent.1 - extent.0).max(0.1) as f32;
        view.ppb = ((area.width() - 24.0) / span).clamp(MIN_PPB, MAX_PPB);
        view.scroll_x = extent.0 as f32 * view.ppb - 12.0;
    }
    if ui.rect_contains_pointer(area) {
        // The lit part moving changes the extent while it is dragged;
        // scrolling follows time 0 instead, so the view doesn't slide
        // under the pointer.
        navigate(
            ui,
            view,
            area,
            (extent.0.min(lit_from), extent.1.max(lit_to)),
        );
    }
    let axis = Axis {
        origin: area.left() - view.scroll_x,
        pps: view.ppb,
    };

    let painter = ui.painter_at(body);
    painter.rect_filled(wave, 0.0, colors::BACKGROUND);
    // Zoomed in to a few frames a screen, the samples themselves are drawn.
    let file_seconds = |x: f32| f64::from((x - axis.origin) / axis.pps) + spec.zero;
    let frames = |x: f32| (file_seconds(x) * rate).clamp(0.0, spec.frames as f64) as u64;
    let (first, last) = (frames(wave.left()), frames(wave.right()) + 1);
    let memory;
    let samples = if (last - first) as f32 <= 2.0 * wave.width() {
        match spec.curve {
            CurveSource::File(path) => {
                curve::window(&mut view.samples, path, first, last, spec.frames)
            }
            CurveSource::Memory(sample) => {
                memory = curve::Samples::in_memory(sample, first, last);
                Some(&memory)
            }
        }
    } else {
        None
    };
    if let Some((from, to)) = spec.looping {
        let band = Rect::from_x_y_ranges(
            axis.x(from - spec.zero).max(wave.left())..=axis.x(to - spec.zero).min(wave.right()),
            wave.y_range(),
        );
        if band.width() > 0.0 {
            painter.rect_filled(band, 0.0, Edge::LoopStart.colour().gamma_multiply(0.12));
        }
    }
    draw_waveform(
        &painter,
        wave,
        axis,
        spec.peaks,
        samples.map(|s| (s, (first, last))),
        rate,
        spec.zero,
        spec.gain,
    );
    // The audio outside the lit part is dimmed.
    let dim = Color32::BLACK.gamma_multiply(0.45);
    let (from, to) = (axis.x(lit_from), axis.x(lit_to));
    for (a, b) in [(wave.left(), from), (to, wave.right())] {
        let (a, b) = (a.max(wave.left()), b.min(wave.right()));
        if b > a {
            painter.rect_filled(Rect::from_x_y_ranges(a..=b, wave.y_range()), 0.0, dim);
        }
    }
    draw_ruler(&painter, ruler, wave, axis);
    let edges: Vec<(Edge, f64)> = Edge::ALL
        .into_iter()
        .filter_map(|edge| {
            let at = match edge {
                Edge::Start => Some(spec.lit.0),
                Edge::End => Some(spec.lit.1),
                Edge::LoopStart => spec.looping.map(|l| l.0),
                Edge::LoopEnd => spec.looping.map(|l| l.1),
            }?;
            Some((edge, at))
        })
        .collect();
    for &(edge, at) in &edges {
        let x = axis.x(at - spec.zero);
        if !(wave.left()..=wave.right()).contains(&x) {
            continue;
        }
        painter.vline(x, wave.y_range(), Stroke::new(1.5, edge.colour()));
        painter.text(
            Pos2::new(
                if edge.is_start() { x + 3.0 } else { x - 3.0 },
                wave.top() + 2.0 + if edge.is_loop() { 12.0 } else { 0.0 },
            ),
            if edge.is_start() {
                Align2::LEFT_TOP
            } else {
                Align2::RIGHT_TOP
            },
            edge.name(),
            FontId::proportional(10.0),
            edge.colour(),
        );
    }
    for &at in spec.playheads {
        let x = axis.x(at - spec.zero);
        if (wave.left()..=wave.right()).contains(&x) {
            painter.vline(x, area.y_range(), Stroke::new(1.0, colors::PLAYHEAD));
        }
    }

    let response = ui.interact(wave, ui.id().with("audio pane"), Sense::click_and_drag());
    let nearest = |x: f32| {
        edges
            .iter()
            .map(|&(edge, at)| (edge, (axis.x(at - spec.zero) - x).abs()))
            .filter(|&(_, d)| d <= GRAB)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(edge, _)| edge)
    };
    if let Some(pos) = response.hover_pos()
        && (view.grab.is_some() || nearest(pos.x).is_some())
        && !matches!(view.grab, Some(Grab::Slip { .. }))
    {
        ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
    } else if response.hovered() || view.grab.is_some() {
        ui.ctx().set_cursor_icon(CursorIcon::Grab);
    }
    let mut gestures = Vec::new();
    if response.drag_started()
        && let Some(press) = ui.input(|i| i.pointer.press_origin())
    {
        view.grab = Some(match nearest(press.x) {
            Some(edge) => Grab::Edge(edge),
            None => Grab::Slip { press_x: press.x },
        });
        gestures.push(Gesture::Begin);
    }
    if response.dragged()
        && let (Some(grab), Some(pos)) = (view.grab, response.interact_pointer_pos())
    {
        gestures.push(match grab {
            Grab::Slip { press_x } => Gesture::Slip((pos.x - press_x) / axis.pps),
            Grab::Edge(edge) => Gesture::Edge(edge, file_seconds(pos.x).clamp(0.0, file_secs)),
        });
    }
    if response.drag_stopped() && view.grab.take().is_some() {
        gestures.push(Gesture::Release);
    }
    if spec.peaks.is_none() {
        painter.text(
            wave.center(),
            Align2::CENTER_CENTER,
            "Reading the waveform…",
            FontId::proportional(12.0),
            colors::TEXT_WEAK,
        );
    }
    Out {
        gestures,
        wave,
        axis,
        response,
    }
}

/// Zooms around the pointer (Ctrl+scroll, pinch) or pans (scroll, middle
/// button drag), keeping `seconds` (the extent in time on the axis) in reach.
///
/// The pane has one axis, the time along the horizontal: every row of it
/// (the ruler, the flags, the brace, the handles and the wave) is drawn in
/// the space the pane has, so there is nothing to scroll up and down. The
/// wheel's vertical component is therefore swallowed rather than turned into
/// a sideways pan.
pub(crate) fn navigate(ui: &Ui, view: &mut View, area: Rect, seconds: (f64, f64)) {
    let (scroll, zoom, pointer, middle) = ui.input(|i| {
        (
            i.smooth_scroll_delta,
            i.zoom_delta(),
            i.pointer.hover_pos(),
            i.pointer.middle_down().then(|| i.pointer.delta().x),
        )
    });
    if zoom != 1.0 {
        // Ctrl+scroll and pinch zoom; the wheel's own delta goes no further,
        // so one turn cannot zoom and pan at once.
        let anchor = pointer.map_or(area.left(), |p| p.x.max(area.left()));
        let at = (anchor - area.left() + view.scroll_x) / view.ppb;
        view.ppb = (view.ppb * zoom).clamp(MIN_PPB, MAX_PPB);
        view.scroll_x = at * view.ppb - (anchor - area.left());
    } else {
        // The wheel pans along the time axis; so does a middle-button drag.
        view.scroll_x -= scroll.x + middle.unwrap_or(0.0);
    }
    // The wheel turn is the pane's: what it did not use is swallowed too, so
    // that one turn does not scroll something else as well.
    ui.input_mut(|i| i.smooth_scroll_delta = Vec2::ZERO);
    let (lo, hi) = (
        seconds.0 as f32 * view.ppb - 12.0,
        seconds.1 as f32 * view.ppb,
    );
    view.scroll_x = view
        .scroll_x
        .clamp(lo.min(hi), (hi - area.width() * 0.5).max(lo));
}

/// The file's peaks across the window, one column per pixel.
#[allow(clippy::too_many_arguments)]
fn draw_waveform(
    painter: &egui::Painter,
    wave: Rect,
    axis: Axis,
    peaks: Option<&Peaks>,
    samples: Option<(&curve::Samples, (u64, u64))>,
    rate: f64,
    zero: f64,
    gain: f32,
) {
    let Some(peaks) = peaks else {
        return;
    };
    let frames = peaks.frames();
    // The file's frames, on screen.
    let (x0, x1) = (axis.x(-zero), axis.x(frames as f64 / rate - zero));
    let (v0, v1) = (x0.max(wave.left()), x1.min(wave.right()));
    if v1 - v0 < 0.5 || x1 <= x0 {
        return;
    }
    let ink = theme::ACCENT.gamma_multiply(0.55);
    if let Some((samples, frames)) = samples
        && axis.pps / rate as f32 >= curve::CURVE_AT
    {
        curve::draw(painter, wave, samples, frames, gain, ink, |f| {
            axis.x(f / rate - zero)
        });
        return;
    }
    let across =
        |x: f32| (f64::from((x - x0) / (x1 - x0)) * frames as f64).clamp(0.0, frames as f64);
    let columns = ((v1 - v0).ceil() as usize).clamp(1, MAX_COLUMNS);
    let (start, end) = (across(v0).round() as u64, across(v1).round() as u64);
    let middle = wave.center().y;
    let half = wave.height() / 2.0 - 2.0;
    let stroke = Stroke::new((v1 - v0) / columns as f32, ink);
    for (i, peak) in peaks.columns(None, start, end, columns).iter().enumerate() {
        let x = v0 + (i as f32 + 0.5) * (v1 - v0) / columns as f32;
        let (min, max) = (
            (peak.min * gain).clamp(-1.0, 1.0),
            (peak.max * gain).clamp(-1.0, 1.0),
        );
        painter.line_segment(
            [
                Pos2::new(x, middle - max * half - 0.5),
                Pos2::new(x, middle - min * half + 0.5),
            ],
            stroke,
        );
    }
}

/// A seconds ruler: a line at a round number of seconds, labelled when there
/// is room.
fn draw_ruler(painter: &egui::Painter, ruler: Rect, wave: Rect, axis: Axis) {
    painter.rect_filled(ruler, 0.0, colors::RULER);
    let every: f64 = [
        0.01, 0.02, 0.05, 0.1, 0.25, 0.5, 1.0, 2.0, 5.0, 10.0, 30.0, 60.0,
    ]
    .into_iter()
    .find(|s| *s * f64::from(axis.pps) >= 60.0)
    .unwrap_or(120.0);
    let first =
        ((f64::from(ruler.left() - axis.origin) / f64::from(axis.pps)) / every).floor() as i64;
    let last =
        ((f64::from(ruler.right() - axis.origin) / f64::from(axis.pps)) / every).ceil() as i64;
    for n in first..=last {
        let seconds = n as f64 * every;
        let x = axis.x(seconds);
        if !(ruler.left()..=ruler.right()).contains(&x) {
            continue;
        }
        painter.vline(
            x,
            ruler.bottom() - 6.0..=wave.bottom(),
            Stroke::new(1.0, colors::BEAT_LINE),
        );
        painter.text(
            Pos2::new(x + 3.0, ruler.top() + 2.0),
            Align2::LEFT_TOP,
            seconds_label(seconds, every),
            FontId::proportional(10.0),
            colors::TEXT_WEAK,
        );
    }
}

fn seconds_label(seconds: f64, every: f64) -> String {
    if every >= 1.0 {
        format!("{seconds:.0} s")
    } else if every >= 0.1 {
        format!("{seconds:.2} s")
    } else {
        format!("{:.0} ms", seconds * 1000.0)
    }
}

/// A Sampler's peaks, worked out once per sample.
pub(crate) fn peaks_of(ctx: &egui::Context, hash: &str, sample: &Sample) -> std::sync::Arc<Peaks> {
    let id = egui::Id::new(("sample peaks", hash));
    if let Some(found) = ctx.data(|d| d.get_temp::<std::sync::Arc<Peaks>>(id)) {
        return found;
    }
    let mut builder = noodle_io::PeaksBuilder::new(2, sample.sample_rate);
    let interleaved: Vec<f32> = sample
        .left
        .iter()
        .zip(sample.right.iter())
        .flat_map(|(l, r)| [*l, *r])
        .collect();
    builder.push(&interleaved);
    let found = std::sync::Arc::new(builder.finish());
    ctx.data_mut(|d| d.insert_temp(id, found.clone()));
    found
}
