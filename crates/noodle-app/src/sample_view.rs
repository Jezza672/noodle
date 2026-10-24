//! The Sampler's sample, drawn small in the properties panel. A double-click
//! opens it in the clip editor ([`crate::clip_editor`]), the editor clips use,
//! with the same zoom, trim, loop, gain envelope, warp and playhead; this
//! module supplies what the editor needs to know about the sample.

use std::sync::Arc;

use egui::{Color32, Pos2, Rect, Sense, Stroke, Ui, vec2};
use noodle_core::{Node, NodeId};
use noodle_nodes::{SAMPLE_KEY, SAMPLER_ID, Sample};

use crate::clip_editor::pane::{self, Edge};
use crate::session::Session;
use crate::theme::{self, timeline as colors};
use crate::timeline::Loaded;
use crate::timeline::clips::Source;

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
    let peaks = pane::peaks_of(ui.ctx(), hash, sample);
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

/// What the clip editor needs to know about the sample of the Sampler `id`:
/// the audio in memory, its waveform and its transients (worked out once per
/// sample).
pub fn loaded(ctx: &egui::Context, session: &Session, id: NodeId) -> Option<Loaded> {
    let (hash, sample, _) = contents(session, id)?;
    let peaks = pane::peaks_of(ctx, &hash, &sample);
    let key = egui::Id::new(("sample analysis", &hash));
    let analysis = match ctx.data(|d| d.get_temp::<Arc<noodle_io::Analysis>>(key)) {
        Some(found) => found,
        None => {
            let limit = (f64::from(sample.sample_rate) * noodle_io::ANALYSIS_SECONDS) as usize;
            let mono: Vec<f32> = sample
                .left
                .iter()
                .zip(sample.right.iter())
                .take(limit)
                .map(|(l, r)| (l + r) * 0.5)
                .collect();
            let found = Arc::new(noodle_io::Analysis::of(&mono, sample.sample_rate));
            ctx.data_mut(|d| d.insert_temp(key, found.clone()));
            found
        }
    };
    Some(Loaded {
        path: std::path::PathBuf::new(),
        source: Source {
            sample_rate: sample.sample_rate,
            frames: Some(sample.frames() as u64),
        },
        peaks: Some(peaks),
        analysis: Some(analysis),
        memory: Some(sample),
    })
}
