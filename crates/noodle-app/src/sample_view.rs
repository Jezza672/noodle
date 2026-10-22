//! The Sampler's sample, drawn: a small preview in the properties panel and,
//! on a double-click, a panel of its own (where the piano roll and the clip
//! editor open). The panel is the clip editor's audio pane
//! ([`crate::clip_editor::pane`]), so a sample zooms, pans, slips, trims and
//! loops the way a clip does; here the gestures set the Sampler's `start`,
//! `end` and loop parameters.

use std::sync::Arc;

use egui::{Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, Ui, vec2};
use noodle_core::{Command, Node, NodeId};
use noodle_nodes::{SAMPLE_KEY, SAMPLER_ID, Sample};

use crate::clip_editor::pane::{self, CurveSource, Edge, Gesture, MIN_SECONDS};
use crate::session::{Edit, Session};
use crate::theme::{self, timeline as colors};
use crate::toolbar;

/// The height the panel opens at.
pub const DEFAULT_HEIGHT: f32 = 220.0;

/// Which sample the panel shows, and how it is zoomed.
#[derive(Default)]
pub struct SampleView {
    node: Option<NodeId>,
    view: pane::View,
    /// The Sampler's region when a slip began.
    slip: Option<Region>,
}

impl SampleView {
    pub fn open(&mut self, node: NodeId) {
        if self.node != Some(node) {
            self.view.drop_grab();
            self.slip = None;
            self.view.focus = true;
        }
        self.node = Some(node);
    }

    pub fn close(&mut self) {
        self.node = None;
        self.view.drop_grab();
        self.slip = None;
    }

    /// Points per second.
    #[cfg(test)]
    pub fn zoom(&self) -> f32 {
        self.view.ppb
    }

    #[cfg(test)]
    pub fn scroll(&self) -> f32 {
        self.view.scroll_x
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

fn key(edge: Edge) -> &'static str {
    match edge {
        Edge::Start => "start",
        Edge::End => "end",
        Edge::LoopStart => "loop_start",
        Edge::LoopEnd => "loop_end",
    }
}

fn default_of(edge: Edge) -> f32 {
    match edge {
        Edge::Start | Edge::LoopStart => 0.0,
        Edge::End | Edge::LoopEnd => 1.0,
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
        let get = |edge: Edge| {
            node.params
                .get(key(edge))
                .copied()
                .unwrap_or(default_of(edge))
        };
        let on = node.params.get("loop").copied().unwrap_or(0.0) >= 0.5;
        Self {
            start: get(Edge::Start),
            end: get(Edge::End),
            looping: on.then(|| (get(Edge::LoopStart), get(Edge::LoopEnd))),
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

/// The sample drawn small, whole and still: the waveform with the part
/// outside the region dimmed.
pub fn draw_preview(ui: &Ui, rect: Rect, hash: &str, sample: &Sample, region: Region) {
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 4.0, colors::BACKGROUND);
    let peaks = pane::peaks_of(ui, hash, sample);
    let n = (rect.width().ceil() as usize).clamp(1, 4096);
    let (middle, half) = (rect.center().y, rect.height() / 2.0 - 2.0);
    let stroke = Stroke::new(rect.width() / n as f32, theme::ACCENT.gamma_multiply(0.7));
    for (i, peak) in peaks.columns(None, 0, peaks.frames(), n).iter().enumerate() {
        let at = rect.left() + (i as f32 + 0.5) * rect.width() / n as f32;
        painter.line_segment(
            [
                Pos2::new(at, middle - peak.max.clamp(-1.0, 1.0) * half - 0.5),
                Pos2::new(at, middle - peak.min.clamp(-1.0, 1.0) * half + 0.5),
            ],
            stroke,
        );
    }
    let dim = Color32::BLACK.gamma_multiply(0.55);
    let x = |fraction: f32| rect.left() + fraction * rect.width();
    for (a, b) in [(0.0, region.start), (region.end, 1.0)] {
        if b > a {
            painter.rect_filled(Rect::from_x_y_ranges(x(a)..=x(b), rect.y_range()), 0.0, dim);
        }
    }
}

/// The preview in the properties panel; double-click it for the panel.
pub fn preview(ui: &mut Ui, session: &Session, id: NodeId, height: f32) {
    let Some((hash, sample, region)) = contents(session, id) else {
        return;
    };
    let (rect, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::click());
    draw_preview(ui, rect, &hash, &sample, region);
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
pub(crate) type Geometry = (Rect, pane::Axis);

/// `value` (a fraction) for `edge`, kept inside the sample and on the right
/// side of its partner.
fn clamped(region: Region, edge: Edge, value: f32, min: f32) -> f32 {
    let (lo, hi) = match edge {
        Edge::Start => (0.0, region.end - min),
        Edge::End => (region.start + min, 1.0),
        Edge::LoopStart => (0.0, region.looping.map_or(1.0, |l| l.1) - min),
        Edge::LoopEnd => (region.looping.map_or(0.0, |l| l.0) + min, 1.0),
    };
    value.clamp(lo.min(hi), hi)
}

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
        "Drag the start and end to pick the part that plays · drag the audio to slide it under them · Ctrl+scroll zooms, scroll pans · loop markers show when Loop is on",
        |_| {},
    );
    if closed {
        if view.view.drop_grab() {
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
    let frames = sample.frames() as u64;
    let rate = f64::from(sample.sample_rate.max(1));
    let total = frames as f64 / rate;
    let secs = |f: f32| f64::from(f) * total;
    let peaks = pane::peaks_of(ui, &hash, &sample);
    let out = pane::show(
        ui,
        &mut view.view,
        body,
        &pane::Spec {
            peaks: Some(&peaks),
            curve: CurveSource::Memory(&sample),
            rate,
            frames,
            zero: 0.0,
            lit: (secs(region.start), secs(region.end)),
            looping: region.looping.map(|(a, b)| (secs(a), secs(b))),
            gain: 1.0,
            playhead: None,
        },
    );
    #[cfg(test)]
    ui.data_mut(|d| {
        d.insert_temp::<Geometry>(egui::Id::new("sample view body"), (out.wave, out.axis))
    });
    let min = (MIN_SECONDS / total.max(MIN_SECONDS)) as f32;
    let set = |edge: Edge, value: f32| {
        Edit::Drag(Command::SetParam {
            node: id,
            key: key(edge).to_owned(),
            value: Some(value),
        })
    };
    for gesture in out.gestures {
        match gesture {
            Gesture::Begin => view.slip = Some(region),
            Gesture::Edge(edge, at) => {
                let value = clamped(region, edge, (at / total.max(1e-9)) as f32, min);
                edits.push(set(edge, value));
            }
            Gesture::Slip(seconds) => {
                // The audio follows the pointer, so the region moves the
                // other way (loop ends with it), stopping at the sample's ends.
                if let Some(original) = view.slip {
                    let by = -seconds / total.max(1e-9) as f32;
                    let by = by.clamp(-original.start, 1.0 - original.end);
                    edits.push(set(Edge::Start, original.start + by));
                    edits.push(set(Edge::End, original.end + by));
                }
            }
            Gesture::Release => {
                view.slip = None;
                edits.push(Edit::EndDrag);
            }
        }
    }
    edits
}
