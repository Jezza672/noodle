//! The Sampler's sample, drawn: a small preview in the properties panel and,
//! on a double-click, a panel of its own (where the piano roll and the clip
//! editor open) with the region and loop markers to drag.

use std::sync::Arc;

use egui::{Align2, Color32, CursorIcon, FontId, Pos2, Rect, Sense, Stroke, Ui, vec2};
use noodle_core::{Command, Node, NodeId};
use noodle_nodes::{SAMPLE_KEY, SAMPLER_ID, Sample};

use crate::session::{Edit, Session};
use crate::theme::{self, timeline as colors};
use crate::toolbar;

/// The height the panel opens at.
pub const DEFAULT_HEIGHT: f32 = 220.0;
/// How close, in points, a press is to a marker to take it.
const GRAB: f32 = 7.0;
/// The least the end of a region is after its start, as a fraction of the
/// sample.
const MIN_REGION: f32 = 0.001;

/// Which sample the panel shows, and what is being dragged in it.
pub struct SampleView {
    node: Option<NodeId>,
    drag: Option<Marker>,
    /// The part of the sample on screen, as fractions of it.
    window: (f32, f32),
}

impl Default for SampleView {
    fn default() -> Self {
        Self {
            node: None,
            drag: None,
            window: WHOLE,
        }
    }
}

/// The window that shows the whole sample.
const WHOLE: (f32, f32) = (0.0, 1.0);
/// The least of the sample the window can show, as a fraction of it.
const MIN_WINDOW: f32 = 0.0002;

/// `window` after a wheel turn: `zoom` (1.0 for none) keeps the fraction under
/// `anchor` (0 to 1 across the view) where it is, and `pan` slides it by that
/// fraction of the view. It stays inside the sample.
fn navigated(window: (f32, f32), anchor: f32, zoom: f32, pan: f32) -> (f32, f32) {
    let width = window.1 - window.0;
    let under = window.0 + anchor * width;
    let new_width = (width / zoom).clamp(MIN_WINDOW, 1.0);
    let start = under - anchor * new_width + pan * new_width;
    let start = start.clamp(0.0, 1.0 - new_width);
    (start, start + new_width)
}

impl SampleView {
    pub fn open(&mut self, node: NodeId) {
        if self.node != Some(node) {
            self.drag = None;
            self.window = WHOLE;
        }
        self.node = Some(node);
    }

    pub fn close(&mut self) {
        self.node = None;
        self.drag = None;
        self.window = WHOLE;
    }

    /// The part of the sample on screen, as fractions of it.
    #[cfg(test)]
    pub fn window(&self) -> (f32, f32) {
        self.window
    }

    /// The Sampler node shown, if the panel is open.
    pub fn node(&self) -> Option<NodeId> {
        self.node
    }

    /// Closes the panel if its node has gone.
    pub fn retain_existing(&mut self, session: &Session) {
        if self.node.is_some_and(|id| sampler(session, id).is_none()) {
            self.close();
        }
    }
}

/// A Sampler node in the project.
fn sampler(session: &Session, id: NodeId) -> Option<&Node> {
    session
        .project()
        .graph()
        .node(id)
        .filter(|node| node.type_id == SAMPLER_ID)
}

/// The hash of the node's sample ("" if it has none).
pub fn hash_of(node: &Node) -> String {
    match node.config.get(SAMPLE_KEY) {
        Some(noodle_core::Value::Text(hash)) => hash.clone(),
        _ => String::new(),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Marker {
    Start,
    End,
    LoopStart,
    LoopEnd,
}

impl Marker {
    const ALL: [Marker; 4] = [Self::Start, Self::End, Self::LoopStart, Self::LoopEnd];

    fn key(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::End => "end",
            Self::LoopStart => "loop_start",
            Self::LoopEnd => "loop_end",
        }
    }

    fn default(self) -> f32 {
        match self {
            Self::Start | Self::LoopStart => 0.0,
            Self::End | Self::LoopEnd => 1.0,
        }
    }

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
}

/// The sample's region and loop, as fractions of the sample.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Region {
    pub start: f32,
    pub end: f32,
    /// The loop's ends, if the loop is on.
    pub looping: Option<(f32, f32)>,
}

impl Region {
    pub fn of(node: &Node) -> Self {
        let get = |marker: Marker| {
            node.params
                .get(marker.key())
                .copied()
                .unwrap_or(marker.default())
        };
        let on = node.params.get("loop").copied().unwrap_or(0.0) >= 0.5;
        Self {
            start: get(Marker::Start),
            end: get(Marker::End),
            looping: on.then(|| (get(Marker::LoopStart), get(Marker::LoopEnd))),
        }
    }

    fn at(self, marker: Marker) -> Option<f32> {
        match marker {
            Marker::Start => Some(self.start),
            Marker::End => Some(self.end),
            Marker::LoopStart => self.looping.map(|l| l.0),
            Marker::LoopEnd => self.looping.map(|l| l.1),
        }
    }

    /// `value` for `marker`, kept where it can be: inside the sample and on
    /// the right side of its partner.
    fn clamped(self, marker: Marker, value: f32) -> f32 {
        let (lo, hi) = match marker {
            Marker::Start => (0.0, self.end - MIN_REGION),
            Marker::End => (self.start + MIN_REGION, 1.0),
            Marker::LoopStart => (0.0, self.looping.map_or(1.0, |l| l.1) - MIN_REGION),
            Marker::LoopEnd => (self.looping.map_or(0.0, |l| l.0) + MIN_REGION, 1.0),
        };
        value.clamp(lo.min(hi), hi)
    }
}

// The columns last worked out for each sample, so they are only worked out
// again when the sample or the width changes.
type Columns = Arc<(usize, Vec<(f32, f32)>)>;

/// The sample's min and max, `n` columns across (the two channels mixed).
fn columns(ui: &Ui, hash: &str, sample: &Sample, n: usize, window: (f32, f32)) -> Columns {
    let id = egui::Id::new((
        "sample columns",
        hash,
        window.0.to_bits(),
        window.1.to_bits(),
    ));
    if let Some(found) = ui.data(|d| d.get_temp::<Columns>(id))
        && found.0 == n
    {
        return found;
    }
    let frames = sample.frames();
    let (first, span) = (
        window.0 as f64 * frames as f64,
        (window.1 - window.0) as f64 * frames as f64,
    );
    let at = |i: usize| (first + span * i as f64 / n as f64) as usize;
    let cols: Vec<(f32, f32)> = (0..n)
        .map(|i| {
            let (a, b) = (at(i), at(i + 1).max(at(i) + 1));
            let mut range = (0.0f32, 0.0f32);
            for f in a..b.min(frames) {
                let mixed = (sample.left[f] + sample.right[f]) * 0.5;
                range = (range.0.min(mixed), range.1.max(mixed));
            }
            range
        })
        .collect();
    let found: Columns = Arc::new((n, cols));
    ui.data_mut(|d| d.insert_temp(id, found.clone()));
    found
}

/// Draws `sample` into `rect`: the waveform with the part outside the region
/// dimmed and, if the loop is on, the loop's span tinted. Markers are drawn on
/// top when `markers` is set.
pub fn draw(
    ui: &Ui,
    rect: Rect,
    hash: &str,
    sample: &Sample,
    region: Region,
    markers: bool,
    window: (f32, f32),
) {
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 4.0, colors::BACKGROUND);
    let x =
        |fraction: f32| rect.left() + (fraction - window.0) / (window.1 - window.0) * rect.width();
    if let Some((from, to)) = region.looping {
        let band = Rect::from_x_y_ranges(x(from)..=x(to), rect.y_range());
        painter.rect_filled(band, 0.0, Marker::LoopStart.colour().gamma_multiply(0.12));
    }
    let n = (rect.width().ceil() as usize).clamp(1, 4096);
    let cols = columns(ui, hash, sample, n, window);
    let (middle, half) = (rect.center().y, rect.height() / 2.0 - 2.0);
    let stroke = Stroke::new(rect.width() / n as f32, theme::ACCENT.gamma_multiply(0.7));
    for (i, &(min, max)) in cols.1.iter().enumerate() {
        let at = x((i as f32 + 0.5) / n as f32);
        painter.line_segment(
            [
                Pos2::new(at, middle - max.clamp(-1.0, 1.0) * half - 0.5),
                Pos2::new(at, middle - min.clamp(-1.0, 1.0) * half + 0.5),
            ],
            stroke,
        );
    }
    let dim = Color32::BLACK.gamma_multiply(0.55);
    for (a, b) in [(0.0, region.start), (region.end, 1.0)] {
        if b > a {
            painter.rect_filled(Rect::from_x_y_ranges(x(a)..=x(b), rect.y_range()), 0.0, dim);
        }
    }
    if markers {
        for marker in Marker::ALL {
            if let Some(at) = region.at(marker) {
                painter.vline(x(at), rect.y_range(), Stroke::new(1.5, marker.colour()));
                painter.text(
                    Pos2::new(
                        if matches!(marker, Marker::Start | Marker::LoopStart) {
                            x(at) + 3.0
                        } else {
                            x(at) - 3.0
                        },
                        rect.top()
                            + 2.0
                            + if matches!(marker, Marker::LoopStart | Marker::LoopEnd) {
                                12.0
                            } else {
                                0.0
                            },
                    ),
                    if matches!(marker, Marker::Start | Marker::LoopStart) {
                        Align2::LEFT_TOP
                    } else {
                        Align2::RIGHT_TOP
                    },
                    marker.name(),
                    FontId::proportional(10.0),
                    marker.colour(),
                );
            }
        }
    }
}

/// What the Sampler `id` holds, for a preview or the panel: its hash, the
/// sample and the region.
pub fn contents(session: &Session, id: NodeId) -> Option<(String, Arc<Sample>, Region)> {
    let node = sampler(session, id)?;
    let hash = hash_of(node);
    let sample = session.samples().get(&hash)?;
    Some((hash, sample, Region::of(node)))
}

/// The preview in the properties panel; double-click it for the panel.
pub fn preview(ui: &mut Ui, session: &Session, id: NodeId, height: f32) {
    let Some((hash, sample, region)) = contents(session, id) else {
        return;
    };
    let (rect, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::click());
    draw(ui, rect, &hash, &sample, region, false, WHOLE);
    if response.double_clicked() {
        request_open(ui.ctx(), id);
    }
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Sample waveform")
    });
    response.on_hover_text("Double-click to open the sample view");
}

fn request_id() -> egui::Id {
    egui::Id::new("open the sample view")
}

/// Asks the app to open the sample view for `node`.
pub fn request_open(ctx: &egui::Context, node: NodeId) {
    ctx.data_mut(|d| d.insert_temp(request_id(), Some(node)));
}

/// The node a [`request_open`] asked for, once.
pub fn take_request(ctx: &egui::Context) -> Option<NodeId> {
    ctx.data_mut(|d| d.remove_temp::<Option<NodeId>>(request_id()).flatten())
}

#[cfg(test)]
pub(crate) type Geometry = Rect;

/// Draws the panel for the open Sampler and returns what the user did.
pub fn show(ui: &mut Ui, view: &mut SampleView, session: &Session) -> Vec<Edit> {
    let mut edits = Vec::new();
    view.retain_existing(session);
    let Some(id) = view.node else {
        return edits;
    };
    let rect = ui.available_rect_before_wrap();
    ui.allocate_rect(rect, Sense::hover());
    let toolbar_rect = Rect::from_min_size(rect.min, vec2(rect.width(), toolbar::HEIGHT));
    let title = match contents(session, id) {
        Some((_, sample, _)) => format!(
            "Sample · {:.2} s at {} Hz",
            sample.frames() as f32 / sample.sample_rate.max(1) as f32,
            sample.sample_rate
        ),
        None => "Sample".to_owned(),
    };
    let closed = toolbar::show(
        ui,
        toolbar_rect,
        &title,
        "Drag the start and end to pick the part that plays · Ctrl+scroll zooms, scroll pans · loop markers show when Loop is on",
        |_| {},
    );
    if closed {
        if view.drag.take().is_some() {
            edits.push(Edit::EndDrag);
        }
        view.close();
        return edits;
    }
    let body = Rect::from_min_max(Pos2::new(rect.left(), toolbar_rect.bottom()), rect.max)
        .shrink2(vec2(8.0, 6.0));
    let Some((hash, sample, region)) = contents(session, id) else {
        ui.painter().text(
            body.center(),
            Align2::CENTER_CENTER,
            "This Sampler has no sample loaded",
            FontId::proportional(13.0),
            colors::TEXT_WEAK,
        );
        return edits;
    };
    if ui.rect_contains_pointer(body) {
        let (scroll, zoom, pointer) =
            ui.input(|i| (i.smooth_scroll_delta, i.zoom_delta(), i.pointer.hover_pos()));
        let anchor = pointer.map_or(0.5, |p| {
            ((p.x - body.left()) / body.width()).clamp(0.0, 1.0)
        });
        // Ctrl+wheel and pinch zoom around the pointer; the wheel pans.
        let pan = -(scroll.x + scroll.y) / body.width();
        view.window = navigated(view.window, anchor, zoom, pan);
    }
    let window = view.window;
    draw(ui, body, &hash, &sample, region, true, window);
    #[cfg(test)]
    ui.data_mut(|d| d.insert_temp::<Geometry>(egui::Id::new("sample view body"), body));

    let response = ui.interact(body, ui.id().with("sample view"), Sense::click_and_drag());
    let fraction = |pos: Pos2| {
        let across = ((pos.x - body.left()) / body.width()).clamp(0.0, 1.0);
        window.0 + across * (window.1 - window.0)
    };
    let x_of = |value: f32| body.left() + (value - window.0) / (window.1 - window.0) * body.width();
    let nearest = |pos: Pos2| {
        Marker::ALL
            .into_iter()
            .filter_map(|m| region.at(m).map(|at| (m, (x_of(at) - pos.x).abs())))
            .filter(|&(_, d)| d <= GRAB)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(m, _)| m)
    };
    if let Some(pos) = response.hover_pos()
        && (nearest(pos).is_some() || view.drag.is_some())
    {
        ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
    }
    if response.drag_started()
        && let Some(press) = ui.input(|i| i.pointer.press_origin())
    {
        view.drag = nearest(press);
    }
    if response.dragged()
        && let (Some(marker), Some(pos)) = (view.drag, response.interact_pointer_pos())
    {
        let value = region.clamped(marker, fraction(pos));
        edits.push(Edit::Drag(Command::SetParam {
            node: id,
            key: marker.key().to_owned(),
            value: Some(value),
        }));
    }
    if response.drag_stopped() && view.drag.take().is_some() {
        edits.push(Edit::EndDrag);
    }
    edits
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zooming_keeps_the_fraction_under_the_pointer_and_stays_inside_the_sample() {
        let (a, b) = navigated(WHOLE, 0.25, 4.0, 0.0);
        assert!((b - a - 0.25).abs() < 1e-6);
        assert!((a + 0.25 * (b - a) - 0.25).abs() < 1e-6);
        // Zooming out past the whole sample stops at the whole sample.
        assert_eq!(navigated((a, b), 0.5, 0.01, 0.0), WHOLE);
        // Panning stops at the ends.
        assert_eq!(navigated((a, b), 0.5, 1.0, -10.0).0, 0.0);
        assert_eq!(navigated((a, b), 0.5, 1.0, 10.0).1, 1.0);
        // It can't zoom in without end.
        let (a, b) = navigated(WHOLE, 0.5, 1e9, 0.0);
        assert!(b - a >= MIN_WINDOW * 0.99);
    }

    #[test]
    fn markers_stay_inside_the_sample_and_on_their_own_side() {
        let region = Region {
            start: 0.2,
            end: 0.8,
            looping: Some((0.3, 0.6)),
        };
        assert_eq!(region.clamped(Marker::Start, -1.0), 0.0);
        assert_eq!(region.clamped(Marker::Start, 0.9), 0.8 - MIN_REGION);
        assert_eq!(region.clamped(Marker::End, 0.0), 0.2 + MIN_REGION);
        assert_eq!(region.clamped(Marker::End, 2.0), 1.0);
        assert_eq!(region.clamped(Marker::LoopStart, 0.7), 0.6 - MIN_REGION);
        assert_eq!(region.clamped(Marker::LoopEnd, 0.1), 0.3 + MIN_REGION);
    }
}
