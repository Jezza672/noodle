//! Custom drawing inside a node, below its ports, for nodes that show
//! something other than parameters, such as meters and scopes.
//!
//! A node type declares what it shows with `NodeType::view`; [`view_of`]
//! resolves it, and the editor keeps it on the node's geometry. To give a view
//! a body, return its height from [`height`], draw it in [`paint`] and read its
//! telemetry in [`Bodies::update`]. The editor reserves the space, and calls
//! `paint` after drawing the rest of the node.

use std::collections::{HashMap, VecDeque};

use egui::epaint::PathShape;
use egui::{Painter, Pos2, Rect, Stroke, Vec2, pos2};
use noodle_core::group::{GROUP, GROUP_OUTPUT};
use noodle_core::{Node, NodeId, Project};
use noodle_engine::{
    ACTIVITY_FLOOR, BlockStats, CpuReading, Level, MeterReader, NodeView, ParamReading, Registry,
    ScopeView, Telemetry,
};

use crate::theme::editor as colors;
use crate::widgets::Live;

/// What `node` shows in its body. Group boundaries are structural, and the
/// engine flattens them away, so they have no node type to ask.
pub fn view_of(registry: &Registry, node: &Node) -> NodeView {
    match node.type_id.as_str() {
        GROUP | GROUP_OUTPUT => NodeView::GroupMeter,
        id => registry
            .get(id)
            .map_or(NodeView::None, |node_type| node_type.view(&node.config)),
    }
}

/// Extra space below a node's ports, in graph units. Zero for most nodes.
pub fn height(view: NodeView) -> f32 {
    match view {
        NodeView::Meter | NodeView::GroupMeter => 30.0,
        NodeView::Scope | NodeView::Spectrum | NodeView::Panner => 80.0,
        NodeView::Envelope | NodeView::Graph(_) => 56.0,
        NodeView::Mixer | NodeView::None => 0.0,
    }
}

/// Draws a node's body into `rect`, the space [`height`] reserved, in screen
/// points. Scale anything drawn by `zoom`. `values` gives the node's
/// parameters, for the bodies that draw what they do; `curve` is the config
/// text a body draws from: an Envelope node's drawn curve, a Surround Pan's
/// layout name.
#[allow(clippy::too_many_arguments)]
pub fn paint(
    painter: &Painter,
    rect: Rect,
    zoom: f32,
    node: NodeId,
    view: NodeView,
    type_id: &str,
    bodies: &Bodies,
    values: &dyn Fn(&str) -> f32,
    curve: &str,
) {
    let area = rect.shrink2(Vec2::new(10.0, 4.0) * zoom);
    match view {
        NodeView::Meter | NodeView::GroupMeter => meter(
            painter,
            area,
            zoom,
            bodies.input_meters(node),
            MeterAxis::Horizontal,
        ),
        NodeView::Scope => scope(painter, area, zoom, bodies.scopes.get(&node)),
        NodeView::Spectrum => spectrum(painter, area, zoom, bodies.spectra.get(&node)),
        NodeView::Panner => {
            let layout = noodle_engine::ChannelLayout::from_name(curve)
                .or_else(|| noodle_nodes::default_layout(type_id))
                .unwrap_or(noodle_engine::ChannelLayout::Stereo);
            crate::widgets::panner::paint(
                painter,
                area,
                zoom,
                layout,
                values("azimuth"),
                values("spread"),
                false,
            );
        }
        NodeView::Envelope => envelope(painter, area, zoom, curve),
        NodeView::Graph(kind) => {
            crate::widgets::graph::paint(painter, area, kind.into(), zoom, values);
        }
        NodeView::Mixer | NodeView::None => {}
    }
}

/// An Envelope node's drawn curve, as a line under a faint baseline.
fn envelope(painter: &Painter, area: Rect, zoom: f32, text: &str) {
    let curve = noodle_nodes::EnvelopeCurve::from_text(text);
    painter.rect_filled(area, 2.0 * zoom, colors::BODY);
    let inner = area.shrink2(Vec2::new(3.0, 4.0) * zoom);
    let line: Vec<Pos2> = (0..=64)
        .map(|i| {
            let x = i as f32 / 64.0;
            Pos2::new(
                inner.left() + x * inner.width(),
                inner.bottom() - curve.eval(x) * inner.height(),
            )
        })
        .collect();
    painter.add(PathShape::line(
        line,
        Stroke::new(1.5 * zoom, colors::METER_RMS),
    ));
}

/// What the bodies show, read from the engine once a frame.
#[derive(Default)]
pub struct Bodies {
    /// The editor's own reader, so other views still see every peak.
    reader: Option<MeterReader>,
    meters: HashMap<NodeId, Vec<MeterChannel>>,
    scopes: HashMap<NodeId, ScopeView>,
    spectra: HashMap<NodeId, SpectrumTrace>,
    /// The engine's sample rate, for the spectra.
    sample_rate: f32,
    /// What each wired parameter has been doing, by (node, port key).
    params: HashMap<(NodeId, String), ParamWindow>,
    /// [`Telemetry::activity`] as of the last update.
    activity: u64,
    /// Seconds since the first update, for the windows.
    clock: f32,
    /// Whether CPU meters are shown, so they're only read when wanted.
    show_cpu: bool,
    /// Each node's share of the audio thread's time, while `show_cpu`.
    cpu: HashMap<NodeId, CpuReading>,
    /// The nodes' loads added up, before groups are given their sums.
    cpu_total: f32,
    /// How whole blocks have been taking, over the last few seconds, while
    /// `show_cpu`.
    blocks: BlockStats,
    /// The window before the current one, shown while the current is short.
    blocks_before: BlockStats,
    /// When the current window began, on `clock`.
    blocks_since: f32,
}

/// How much the editor needs redrawing, as of the last [`Bodies::update`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Liveness {
    /// Something reported a change since the last update.
    Moving,
    /// Nothing new, but a peak, hold or trace is still falling.
    Decaying,
    /// Every body shows what it will show until something moves.
    Still,
}

/// The longest a live editor waits between repaints, in seconds: 30 a second,
/// however fast the display is.
pub const METER_FRAME: f32 = 1.0 / 30.0;

/// How often a still editor looks for new sound, in seconds.
pub const IDLE_POLL: f32 = 0.25;

/// How long a window of block statistics lasts, in seconds.
const BLOCK_WINDOW_SECONDS: f32 = 5.0;

/// Blocks a window needs before it is shown instead of the one before.
const BLOCK_WINDOW_MIN: u64 = 200;

/// How far back a wired parameter's range marks look, in seconds.
pub const PARAM_WINDOW_SECONDS: f32 = 3.0;

/// A wired parameter's recent past: the range of each frame's reading, kept
/// for [`PARAM_WINDOW_SECONDS`].
#[derive(Default)]
struct ParamWindow {
    last: f32,
    /// (time, lowest, highest), oldest first.
    history: VecDeque<(f32, f32, f32)>,
}

impl ParamWindow {
    fn push(&mut self, now: f32, reading: ParamReading) {
        self.last = reading.value;
        self.history.push_back((now, reading.min, reading.max));
        self.trim(now);
    }

    fn trim(&mut self, now: f32) {
        while self
            .history
            .front()
            .is_some_and(|&(time, ..)| now - time > PARAM_WINDOW_SECONDS)
        {
            self.history.pop_front();
        }
    }

    fn live(&self) -> Option<Live> {
        let (min, max) = self.history.iter().fold(
            (f32::INFINITY, f32::NEG_INFINITY),
            |(lo, hi), &(_, a, b)| (lo.min(a), hi.max(b)),
        );
        (min <= max).then_some(Live {
            value: self.last,
            min,
            max,
        })
    }
}

impl Bodies {
    /// Reads every meter and scope in `project` from `telemetry`, each by
    /// the view its node type declares in `registry`. `dt` is the time since
    /// the last update, in seconds, for the peaks' decay. Says whether the
    /// bodies need drawing again.
    pub fn update(
        &mut self,
        telemetry: &Telemetry,
        project: &Project,
        registry: &Registry,
        dt: f32,
    ) -> Liveness {
        let graph = project.graph();
        let activity = telemetry.activity();
        let moving = activity != self.activity || self.show_cpu;
        self.activity = activity;
        self.cpu.clear();
        self.cpu_total = 0.0;
        if self.show_cpu {
            self.cpu.extend(telemetry.read_all_cpu());
            self.cpu_total = self.cpu.values().map(|reading| reading.load).sum();
            // A group is flattened away and never runs itself, so it shows
            // what the nodes inside it (at any depth) take.
            let mut groups: HashMap<NodeId, CpuReading> = HashMap::new();
            for (&id, reading) in &self.cpu {
                let mut parent = graph.node(id).and_then(|node| node.parent);
                while let Some(group) = parent {
                    let sum = groups.entry(group).or_default();
                    sum.load += reading.load;
                    sum.peak += reading.peak;
                    parent = graph.node(group).and_then(|node| node.parent);
                }
            }
            self.cpu.extend(groups);
            // Block statistics run in windows: read the current one, and
            // start the next when this one is old enough.
            self.blocks = telemetry.read_blocks();
            if self.clock - self.blocks_since > BLOCK_WINDOW_SECONDS {
                self.blocks_before = self.blocks;
                self.blocks_since = self.clock;
                telemetry.reset_blocks();
            }
        }
        // A new session brings a new hub.
        if !self.reader.as_ref().is_some_and(|r| r.reads(telemetry)) {
            self.reader = None;
        }
        let reader = &*self.reader.get_or_insert_with(|| telemetry.meter_reader());
        self.meters.retain(|&id, _| graph.node(id).is_some());
        self.scopes.retain(|&id, _| graph.node(id).is_some());
        self.spectra.retain(|&id, _| graph.node(id).is_some());

        // Every wired parameter. Reading one takes its range since the last
        // read, so this is the only reader: the properties panel asks `Bodies`.
        self.clock += dt;
        let now = self.clock;
        let wired: std::collections::HashSet<(NodeId, String)> = graph
            .connections()
            .map(|c| (c.to.node, c.to.port))
            .collect();
        // A graph reports what it declares whether or not anything is wired
        // (a Dynamics node's level and reduction, for the dot on its graph).
        let mut wired = wired;
        for (id, node) in graph.nodes() {
            if let NodeView::Graph(kind) = view_of(registry, node) {
                wired.extend(kind.reports().iter().map(|&key| (id, key.to_owned())));
            }
        }
        self.params.retain(|key, _| wired.contains(key));
        for (node, key) in wired {
            if let Some(reading) = telemetry.read_param(node, &key) {
                self.params
                    .entry((node, key))
                    .or_default()
                    .push(now, reading);
            }
        }
        for window in self.params.values_mut() {
            window.trim(now);
        }

        for (id, node) in graph.nodes() {
            match view_of(registry, node) {
                // A mixer reports one level per input.
                NodeView::Meter | NodeView::Mixer => {
                    let channels = self.meters.entry(id).or_default();
                    let levels = reader.meter(id).unwrap_or_default();
                    channels.resize_with(levels.len(), MeterChannel::default);
                    for (channel, level) in channels.iter_mut().zip(levels) {
                        channel.update(level, dt);
                    }
                }
                // A group's outputs, and its output node inside, show what
                // the stage at each output port reports, one bar per channel.
                NodeView::GroupMeter => {
                    let stages: Vec<NodeId> = if node.type_id == GROUP {
                        graph
                            .group_ports(id)
                            .outputs
                            .iter()
                            .map(|p| p.stage())
                            .collect()
                    } else {
                        node.boundary_ports()
                            .into_iter()
                            .map(|(port, _)| noodle_core::group::stage_id(id, port))
                            .collect()
                    };
                    let levels: Vec<Level> = stages
                        .into_iter()
                        .flat_map(|stage| reader.meter(stage).unwrap_or_default())
                        .collect();
                    let channels = self.meters.entry(id).or_default();
                    channels.resize_with(levels.len(), MeterChannel::default);
                    for (channel, level) in channels.iter_mut().zip(levels) {
                        channel.update(level, dt);
                    }
                }
                // An Output node's scope shows what it sends to the device.
                NodeView::Scope => {
                    let view = self.scopes.entry(id).or_insert_with(scope_view);
                    if !telemetry.read_scope(id, view) {
                        *view = scope_view();
                    }
                }
                NodeView::Spectrum => {
                    let trace = self.spectra.entry(id).or_default();
                    if self.sample_rate > 0.0 {
                        trace.update(telemetry, id, self.sample_rate, dt);
                    }
                }
                NodeView::Envelope | NodeView::Panner | NodeView::Graph(_) | NodeView::None => {}
            }
        }
        if moving {
            Liveness::Moving
        } else if self.is_decaying() {
            Liveness::Decaying
        } else {
            Liveness::Still
        }
    }

    /// Whether anything shown is still falling on its own: a meter's peak or
    /// its held tick, a spectrum's curve or held peaks, or a wired
    /// parameter's range marks.
    fn is_decaying(&self) -> bool {
        self.meters
            .values()
            .flatten()
            .any(|c| c.peak > ACTIVITY_FLOOR || c.hold > ACTIVITY_FLOOR)
            || self.spectra.values().any(SpectrumTrace::is_decaying)
            || self
                .params
                .values()
                .any(|w| w.live().is_some_and(|live| live.min < live.max))
    }

    /// Tells the spectra the engine's sample rate (0 when none runs).
    pub fn set_sample_rate(&mut self, rate: f32) {
        self.sample_rate = rate;
    }

    /// What a Spectrum node last measured.
    pub fn spectrum(&self, node: NodeId) -> Option<&SpectrumTrace> {
        self.spectra.get(&node)
    }

    /// The levels a mixer node's inputs last reported, one per input.
    pub fn input_meters(&self, node: NodeId) -> &[MeterChannel] {
        self.meters.get(&node).map_or(&[], Vec::as_slice)
    }

    /// The one channel `channel` of a mixer node, as a slice for drawing.
    pub fn input_meters_of(&self, node: NodeId, channel: usize) -> &[MeterChannel] {
        self.input_meters(node)
            .get(channel..=channel)
            .unwrap_or_default()
    }

    pub fn scope_view(&self, node: NodeId) -> Option<&ScopeView> {
        self.scopes.get(&node)
    }

    /// What the wired parameter `key` of `node` has been doing lately, if it
    /// has reported.
    pub fn param_live(&self, node: NodeId, key: &str) -> Option<Live> {
        self.params.get(&(node, key.to_owned()))?.live()
    }

    /// Turns the per-node CPU meters on or off.
    pub fn set_show_cpu(&mut self, on: bool) {
        self.show_cpu = on;
    }

    pub fn show_cpu(&self) -> bool {
        self.show_cpu
    }

    /// How much of the block time `node` is taking, if CPU meters are on and
    /// it has reported.
    pub fn cpu(&self, node: NodeId) -> Option<CpuReading> {
        self.cpu.get(&node).copied()
    }

    /// The whole graph's share of the audio thread's time, if CPU meters are
    /// on: the nodes' loads added up (without the device and the engine's own
    /// overhead).
    pub fn cpu_total(&self) -> Option<f32> {
        self.show_cpu.then_some(self.cpu_total)
    }

    /// How whole blocks have been taking lately (worst case, deadline
    /// misses), if CPU meters are on and enough blocks have run.
    pub fn block_stats(&self) -> Option<BlockStats> {
        if !self.show_cpu {
            return None;
        }
        let stats = if self.blocks.blocks >= BLOCK_WINDOW_MIN {
            self.blocks
        } else {
            self.blocks_before
        };
        (stats.blocks > 0).then_some(stats)
    }
}

/// Columns across a spectrum display.
const SPECTRUM_COLUMNS: usize = 128;
/// How fast a spectrum's curve falls, in dB per second; it rises at once.
const SPECTRUM_FALL_DB_PER_SECOND: f32 = 60.0;
/// How fast a held peak falls once its hold has run out.
const SPECTRUM_PEAK_FALL_DB_PER_SECOND: f32 = 15.0;
/// How long a held peak stays, in seconds.
const SPECTRUM_PEAK_HOLD_SECONDS: f32 = 1.5;
/// The shortest time between two analyses, in seconds.
const SPECTRUM_PERIOD: f32 = 1.0 / 30.0 - 0.002;
/// Spectrogram columns per second, and how many it keeps.
const SPECTROGRAM_RATE: f32 = 30.0;
const SPECTROGRAM_COLUMNS: usize = 120;

/// What one Spectrum node shows: the analysed input as a curve over a log
/// frequency axis with held peaks, and a spectrogram of how it has changed.
pub struct SpectrumTrace {
    analyser: noodle_nodes::SpectrumAnalyser,
    view: ScopeView,
    mono: Vec<f32>,
    /// dB per display column, falling smoothly after a rise.
    curve: Vec<f32>,
    peaks: Vec<f32>,
    peak_age: Vec<f32>,
    /// Recent curves, oldest first, one per spectrogram column.
    history: VecDeque<Vec<f32>>,
    since_column: f32,
    /// Time since the last analysis, which runs at most [`SPECTRUM_RATE`]
    /// times a second whatever the frame rate.
    since_analysis: f32,
    fresh: Vec<f32>,
    sample_rate: f32,
}

impl Default for SpectrumTrace {
    fn default() -> Self {
        let floor = noodle_nodes::SPECTRUM_FLOOR_DB;
        Self {
            analyser: noodle_nodes::SpectrumAnalyser::new(),
            view: ScopeView::tail(noodle_nodes::SPECTRUM_WINDOW),
            mono: Vec::new(),
            curve: vec![floor; SPECTRUM_COLUMNS],
            peaks: vec![floor; SPECTRUM_COLUMNS],
            peak_age: vec![0.0; SPECTRUM_COLUMNS],
            history: VecDeque::new(),
            since_column: 0.0,
            // Due at once, so a new trace shows its first frame.
            since_analysis: 1.0,
            fresh: vec![floor; SPECTRUM_COLUMNS],
            sample_rate: 48_000.0,
        }
    }
}

impl SpectrumTrace {
    fn update(&mut self, telemetry: &Telemetry, node: NodeId, sample_rate: f32, dt: f32) {
        let floor = noodle_nodes::SPECTRUM_FLOOR_DB;
        self.sample_rate = sample_rate;
        self.since_analysis += dt;
        if self.since_analysis < SPECTRUM_PERIOD {
            return;
        }
        let dt = std::mem::take(&mut self.since_analysis);
        self.mono.clear();
        if telemetry.read_scope(node, &mut self.view) {
            let channels = self.view.channels().max(1);
            self.mono.extend(
                self.view
                    .samples()
                    .chunks_exact(channels)
                    .map(|frame| frame.iter().sum::<f32>() / channels as f32),
            );
        }
        self.fresh.fill(floor);
        if !self.mono.is_empty() {
            let bins = self.analyser.analyse(&self.mono);
            noodle_nodes::columns_from_bins(bins, sample_rate, &mut self.fresh);
        }
        let fall = SPECTRUM_FALL_DB_PER_SECOND * dt;
        for (k, &now) in self.fresh.iter().enumerate() {
            self.curve[k] = now.max(self.curve[k] - fall).max(floor);
            if self.curve[k] >= self.peaks[k] {
                self.peaks[k] = self.curve[k];
                self.peak_age[k] = 0.0;
            } else {
                self.peak_age[k] += dt;
                if self.peak_age[k] > SPECTRUM_PEAK_HOLD_SECONDS {
                    self.peaks[k] =
                        (self.peaks[k] - SPECTRUM_PEAK_FALL_DB_PER_SECOND * dt).max(self.curve[k]);
                }
            }
        }
        self.since_column += dt;
        if self.since_column >= 1.0 / SPECTROGRAM_RATE {
            self.since_column = 0.0;
            let mut column = if self.history.len() == SPECTROGRAM_COLUMNS {
                self.history.pop_front().unwrap_or_default()
            } else {
                Vec::new()
            };
            column.clear();
            column.extend_from_slice(&self.curve);
            self.history.push_back(column);
        }
    }

    /// Whether the curve or its held peaks are still above the floor, so
    /// they will move on their own.
    fn is_decaying(&self) -> bool {
        let floor = noodle_nodes::SPECTRUM_FLOOR_DB + 0.01;
        self.curve.iter().chain(&self.peaks).any(|&db| db > floor)
    }

    /// dB at each display column of the curve, quietest first by frequency.
    pub fn curve(&self) -> &[f32] {
        &self.curve
    }

    /// The held peaks, as `curve`.
    pub fn peaks(&self) -> &[f32] {
        &self.peaks
    }

    pub fn sample_rate(&self) -> f32 {
        self.sample_rate
    }
}

/// Where `db` sits between the floor and 0 dB, from 0 to 1.
fn spectrum_fraction(db: f32) -> f32 {
    let floor = noodle_nodes::SPECTRUM_FLOOR_DB;
    ((db - floor) / -floor).clamp(0.0, 1.0)
}

/// The spectrum as a filled curve on a log frequency axis, with faint lines
/// at 100 Hz, 1 kHz and 10 kHz and the held peaks as a thin line.
pub fn spectrum(painter: &Painter, area: Rect, zoom: f32, trace: Option<&SpectrumTrace>) {
    painter.rect_filled(area, 2.0 * zoom, colors::BODY);
    let rate = trace.map_or(48_000.0, SpectrumTrace::sample_rate);
    let (low, high) = (
        noodle_nodes::SPECTRUM_LOW_HZ,
        noodle_nodes::SPECTRUM_HIGH_HZ.min(rate * 0.5),
    );
    for hz in [100.0, 1000.0, 10_000.0] {
        if hz < high {
            let x = area.left() + area.width() * (hz / low).ln() / (high / low).ln();
            painter.vline(x, area.y_range(), Stroke::new(zoom, colors::SCOPE_AXIS));
        }
    }
    let Some(trace) = trace else {
        return;
    };
    let n = trace.curve().len();
    let point = |k: usize, db: f32| {
        Pos2::new(
            area.left() + area.width() * k as f32 / (n - 1) as f32,
            area.bottom() - area.height() * spectrum_fraction(db),
        )
    };
    let peaks: Vec<Pos2> = trace
        .peaks()
        .iter()
        .enumerate()
        .map(|(k, &db)| point(k, db))
        .collect();
    painter.add(PathShape::line(
        peaks,
        Stroke::new(zoom.max(1.0), colors::TEXT_WEAK),
    ));
    // One quad per column: a curve with dips isn't a convex polygon.
    let curve: Vec<Pos2> = trace
        .curve()
        .iter()
        .enumerate()
        .map(|(k, &db)| point(k, db))
        .collect();
    let mut mesh = egui::Mesh::default();
    let tint = colors::SCOPE_TRACE.gamma_multiply(0.35);
    for pair in curve.windows(2) {
        let base = mesh.vertices.len() as u32;
        for p in [
            pos2(pair[0].x, area.bottom()),
            pair[0],
            pair[1],
            pos2(pair[1].x, area.bottom()),
        ] {
            mesh.colored_vertex(p, tint);
        }
        mesh.add_triangle(base, base + 1, base + 2);
        mesh.add_triangle(base, base + 2, base + 3);
    }
    painter.add(egui::Shape::mesh(mesh));
    painter.add(PathShape::line(
        curve,
        Stroke::new(zoom.max(1.0), colors::SCOPE_TRACE),
    ));
}

/// A colour for a level, dark at the floor through blue and orange to
/// near white at 0 dB.
fn heat(fraction: f32) -> egui::Color32 {
    const STOPS: [(f32, [f32; 3]); 5] = [
        (0.0, [12.0, 12.0, 24.0]),
        (0.35, [40.0, 50.0, 140.0]),
        (0.6, [150.0, 60.0, 150.0]),
        (0.8, [240.0, 140.0, 40.0]),
        (1.0, [255.0, 245.0, 200.0]),
    ];
    let f = fraction.clamp(0.0, 1.0);
    let i = STOPS.iter().rposition(|s| s.0 <= f).unwrap_or(0).min(3);
    let (a, b) = (STOPS[i], STOPS[i + 1]);
    let t = (f - a.0) / (b.0 - a.0);
    let channel = |c: usize| (a.1[c] + (b.1[c] - a.1[c]) * t) as u8;
    egui::Color32::from_rgb(channel(0), channel(1), channel(2))
}

/// The spectrogram: time runs left to right, frequency up the side.
pub fn spectrogram(painter: &Painter, area: Rect, trace: Option<&SpectrumTrace>) {
    painter.rect_filled(area, 2.0, colors::BODY);
    let Some(trace) = trace else {
        return;
    };
    let mut mesh = egui::Mesh::default();
    let cell_w = area.width() / SPECTROGRAM_COLUMNS as f32;
    let cell_h = area.height() / SPECTRUM_COLUMNS as f32;
    let first = SPECTROGRAM_COLUMNS - trace.history.len();
    for (t, column) in trace.history.iter().enumerate() {
        let x = area.left() + (first + t) as f32 * cell_w;
        for (k, &db) in column.iter().enumerate() {
            let y = area.bottom() - (k + 1) as f32 * cell_h;
            mesh.add_colored_rect(
                Rect::from_min_size(Pos2::new(x, y), Vec2::new(cell_w + 0.5, cell_h + 0.5)),
                heat(spectrum_fraction(db)),
            );
        }
    }
    painter.add(egui::Shape::mesh(mesh));
}

/// The bottom of a meter's scale, in dB.
const METER_FLOOR_DB: f32 = -60.0;
/// The top of a meter's scale, in dB, leaving room to show overs.
const METER_CEILING_DB: f32 = 6.0;
/// How fast the peak falls, in dB per second.
const PEAK_FALL_DB_PER_SECOND: f32 = 20.0;
/// How long the highest recent peak stays marked, in seconds.
const PEAK_HOLD_SECONDS: f32 = 1.5;

/// One channel of a meter as shown, all as linear amplitudes.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MeterChannel {
    rms: f32,
    /// The peak, falling steadily after each rise.
    peak: f32,
    /// The highest recent peak, kept for [`PEAK_HOLD_SECONDS`].
    hold: f32,
    hold_age: f32,
}

impl MeterChannel {
    fn update(&mut self, level: Level, dt: f32) {
        self.rms = level.rms;
        let fall = 10f32.powf(-PEAK_FALL_DB_PER_SECOND * dt / 20.0);
        self.peak = level.peak.max(self.peak * fall);
        if level.peak >= self.hold {
            self.hold = level.peak;
            self.hold_age = 0.0;
        } else {
            // Once the hold has run out, the marker follows the falling peak
            // until a new peak reaches it.
            self.hold_age += dt;
            if self.hold_age > PEAK_HOLD_SECONDS {
                self.hold = self.peak;
            }
        }
    }
}

/// Where `amplitude` sits on a meter's scale, from 0 to 1.
fn meter_fraction(amplitude: f32) -> f32 {
    let db = 20.0 * amplitude.max(1e-9).log10();
    ((db - METER_FLOOR_DB) / (METER_CEILING_DB - METER_FLOOR_DB)).clamp(0.0, 1.0)
}

/// The gap between a meter's bars and their thickness, for `count` bars in
/// `thickness`. Gaps shrink with many channels, so every bar keeps some.
fn meter_bars(thickness: f32, count: usize, zoom: f32) -> (f32, f32) {
    let count = count.max(1) as f32;
    let gap = (2.0 * zoom).min(thickness / (3.0 * count + 1.0));
    (gap, (thickness - gap * (count + 1.0)) / count)
}

/// Which way a meter's bars grow.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MeterAxis {
    /// Bars stacked top to bottom, growing to the right: on a node.
    Horizontal,
    /// Bars side by side, growing up: in the mixer.
    Vertical,
}

/// Bars, one per channel: the RMS level filled, the peak as a bar tip, and
/// the held peak as a tick that turns red above 0 dB.
pub fn meter(painter: &Painter, area: Rect, zoom: f32, channels: &[MeterChannel], axis: MeterAxis) {
    painter.rect_filled(area, 2.0 * zoom, colors::BODY);
    let (across, along) = match axis {
        MeterAxis::Horizontal => (area.height(), area.width()),
        MeterAxis::Vertical => (area.width(), area.height()),
    };
    let (gap, thickness) = meter_bars(across, channels.len(), zoom);
    // The position of `amplitude` along the bars.
    let at = |amplitude: f32| match axis {
        MeterAxis::Horizontal => area.left() + along * meter_fraction(amplitude),
        MeterAxis::Vertical => area.bottom() - along * meter_fraction(amplitude),
    };
    let line = Stroke::new(zoom.max(1.0), colors::TEXT_WEAK);

    for (i, channel) in channels.iter().enumerate() {
        let offset = gap + i as f32 * (thickness + gap);
        let bar = match axis {
            MeterAxis::Horizontal => Rect::from_min_size(
                Pos2::new(area.left(), area.top() + offset),
                Vec2::new(along, thickness),
            ),
            MeterAxis::Vertical => Rect::from_min_size(
                Pos2::new(area.left() + offset, area.top()),
                Vec2::new(thickness, along),
            ),
        };
        let fill = |amplitude: f32, color| {
            let end = at(amplitude);
            match axis {
                MeterAxis::Horizontal if end > bar.left() => {
                    painter.rect_filled(bar.with_max_x(end), 0.0, color);
                }
                MeterAxis::Vertical if end < bar.bottom() => {
                    painter.rect_filled(bar.with_min_y(end), 0.0, color);
                }
                _ => {}
            }
        };
        fill(channel.peak, colors::METER_PEAK);
        fill(channel.rms, colors::METER_RMS);
        if channel.hold > 0.0 {
            let color = if channel.hold >= 1.0 {
                colors::METER_OVER
            } else {
                colors::TEXT
            };
            let stroke = Stroke::new(zoom.max(1.0), color);
            match axis {
                MeterAxis::Horizontal => painter.vline(at(channel.hold), bar.y_range(), stroke),
                MeterAxis::Vertical => painter.hline(bar.x_range(), at(channel.hold), stroke),
            };
        }
    }
    // 0 dB, so overs are easy to see.
    match axis {
        MeterAxis::Horizontal => painter.vline(at(1.0), area.y_range(), line),
        MeterAxis::Vertical => painter.hline(area.x_range(), at(1.0), line),
    };
}

/// How many frames of a scope are shown: about 21 ms at 48 kHz.
const SCOPE_FRAMES: usize = 1024;

/// One strip per channel, each a min/max trace per pixel column so dense
/// waveforms stay readable, full scale ±1.
pub fn scope(painter: &Painter, area: Rect, zoom: f32, view: Option<&ScopeView>) {
    painter.rect_filled(area, 2.0 * zoom, colors::BODY);
    let Some(view) = view.filter(|v| v.channels() > 0) else {
        return;
    };
    let channels = view.channels();
    let frames = view.samples().len() / channels;
    let start = trigger(view, SCOPE_FRAMES);
    let shown = frames.saturating_sub(start).min(SCOPE_FRAMES);
    let strip_height = area.height() / channels as f32;
    let columns = area.width().max(1.0) as usize;

    for channel in 0..channels {
        let top = area.top() + channel as f32 * strip_height;
        let mid = top + strip_height / 2.0;
        painter.hline(area.x_range(), mid, Stroke::new(zoom, colors::SCOPE_AXIS));
        if shown == 0 {
            continue;
        }
        let y = |x: f32| mid - x.clamp(-1.0, 1.0) * (strip_height / 2.0 - zoom);
        let samples: Vec<f32> = view
            .channel(channel)
            .into_iter()
            .flatten()
            .skip(start)
            .take(shown)
            .collect();
        let mut points = Vec::with_capacity(columns * 2);
        for (column, (low, high)) in min_max_columns(&samples, columns, SCOPE_FRAMES)
            .into_iter()
            .enumerate()
        {
            let x = area.left() + column as f32;
            points.push(Pos2::new(x, y(low)));
            if high != low {
                points.push(Pos2::new(x, y(high)));
            }
        }
        painter.add(PathShape::line(
            points,
            Stroke::new(zoom.max(1.0), colors::SCOPE_TRACE),
        ));
    }
}

/// A view holding only as much history as the scope draws from: the window
/// shown, and the windows searched before it for a trigger.
fn scope_view() -> ScopeView {
    ScopeView::tail(SCOPE_FRAMES * (TRIGGER_SEARCH_WINDOWS + 1))
}

/// How far back, in windows, the scope looks for a crossing to trigger on.
/// Four windows of 1024 frames catch tones down to about 12 Hz at 48 kHz,
/// without showing audio much older than that.
const TRIGGER_SEARCH_WINDOWS: usize = 4;

/// The first frame to show of `window` frames, so a steady waveform holds
/// still: the latest rising zero crossing of channel 0 that leaves a full
/// window after it, or else the start of the latest window.
fn trigger(view: &ScopeView, window: usize) -> usize {
    let channels = view.channels().max(1);
    let samples = view.samples();
    let frames = samples.len() / channels;
    let latest = frames.saturating_sub(window);
    let first = latest.saturating_sub(window * TRIGGER_SEARCH_WINDOWS);
    let at = |frame: usize| samples[frame * channels];
    (first.max(1)..=latest)
        .rev()
        .find(|&frame| at(frame - 1) < 0.0 && at(frame) >= 0.0)
        .unwrap_or(latest)
}

/// Splits `samples`, spread as if `window` long, across `columns` and gives
/// each column's lowest and highest sample. Columns past the end of `samples`
/// are left out.
fn min_max_columns(samples: &[f32], columns: usize, window: usize) -> Vec<(f32, f32)> {
    let per_column = window as f32 / columns as f32;
    (0..columns)
        .map_while(|column| {
            let from = (column as f32 * per_column) as usize;
            let to = (((column + 1) as f32 * per_column) as usize).max(from + 1);
            let slice = samples.get(from..to.min(samples.len()))?;
            let first = *slice.first()?;
            Some(
                slice
                    .iter()
                    .fold((first, first), |(lo, hi), &x| (lo.min(x), hi.max(x))),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_core::spare;
    use noodle_core::{Command, Node};
    use noodle_engine::OUTPUT_ID;
    use noodle_nodes::{
        DYNAMICS_ID, ENVELOPE_ID, METER_ID, SCOPE_ID, SPECTRUM_ID, SURROUND_PAN_ID,
    };

    fn registry() -> Registry {
        crate::session::Nodes::all().registry
    }

    /// The height of the body a node of this type gets.
    fn height_of(type_id: &str) -> f32 {
        height(view_of(&registry(), &Node::new(type_id)))
    }

    #[test]
    fn a_group_shows_what_the_nodes_inside_it_take() {
        let mut project = Project::new();
        let ids: Vec<NodeId> = (0..3).map(|_| project.new_node_id()).collect();
        for &id in &ids {
            Command::AddNode {
                id,
                node: Node::new("noodle.util.gain"),
            }
            .apply(&mut project)
            .unwrap();
        }
        let (group, command) =
            noodle_core::group::group_nodes(&project, &ids[..2], || NodeId(100)).unwrap();
        command.apply(&mut project).unwrap();

        let telemetry = Telemetry::new();
        let mut writers: Vec<_> = ids.iter().map(|&id| telemetry.open_cpu(id)).collect();
        // Loads of 0.1, 0.2 and 0.4 of a block, held long enough to settle.
        for (writer, share) in writers.iter_mut().zip([0.1, 0.2, 0.4]) {
            for _ in 0..600 {
                writer.record((share * 1e6) as u64, 1e6);
            }
        }
        let mut bodies = Bodies::default();
        bodies.update(&telemetry, &project, &registry(), 0.016);
        assert_eq!(bodies.cpu(group), None, "off until switched on");
        bodies.set_show_cpu(true);
        bodies.update(&telemetry, &project, &registry(), 0.016);
        let inside = bodies.cpu(group).expect("the group reports");
        assert!((inside.load - 0.3).abs() < 0.01, "{}", inside.load);
        let total = bodies.cpu_total().unwrap();
        assert!(
            (total - 0.7).abs() < 0.01,
            "groups aren't counted twice: {total}"
        );
    }

    #[test]
    fn meter_scale_runs_from_floor_to_ceiling() {
        assert_eq!(meter_fraction(0.0), 0.0);
        assert_eq!(meter_fraction(10.0), 1.0);
        let zero_db = -METER_FLOOR_DB / (METER_CEILING_DB - METER_FLOOR_DB);
        assert!((meter_fraction(1.0) - zero_db).abs() < 1e-6);
        assert!(meter_fraction(0.5) < meter_fraction(1.0));
    }

    #[test]
    fn meter_bars_fit_any_channel_count() {
        for count in [0, 1, 2, 10, 64] {
            let (gap, bar) = meter_bars(22.0, count, 1.0);
            assert!(gap > 0.0 && bar > 0.0, "{count} channels");
            let used = gap * (count.max(1) + 1) as f32 + bar * count.max(1) as f32;
            assert!((used - 22.0).abs() < 1e-3, "{count} channels");
        }
    }

    #[test]
    fn peaks_fall_and_holds_expire() {
        let mut channel = MeterChannel::default();
        channel.update(
            Level {
                peak: 1.0,
                rms: 0.5,
            },
            0.1,
        );
        assert_eq!((channel.peak, channel.hold), (1.0, 1.0));

        // A second of silence: the peak falls 20 dB, the hold stays.
        for _ in 0..10 {
            channel.update(Level::default(), 0.1);
        }
        assert!((channel.peak - 0.1).abs() < 1e-3, "{channel:?}");
        assert_eq!(channel.hold, 1.0);
        assert_eq!(channel.rms, 0.0);

        // Past the hold time, the hold follows the falling peak, every frame.
        for _ in 0..6 {
            channel.update(Level::default(), 0.1);
        }
        assert!(channel.hold < 0.1, "{channel:?}");
        for _ in 0..3 {
            channel.update(Level::default(), 0.1);
            assert_eq!(channel.hold, channel.peak, "{channel:?}");
        }
    }

    fn sine_view(frames: usize, period: usize, phase: usize) -> ScopeView {
        let telemetry = Telemetry::new();
        let mut writer = telemetry.open_scope(NodeId(1), 1, frames);
        writer.write(frames, |f, _| {
            ((f + phase) as f32 * std::f32::consts::TAU / period as f32).sin()
        });
        let mut view = ScopeView::default();
        assert!(telemetry.read_scope(NodeId(1), &mut view));
        view
    }

    #[test]
    fn trigger_starts_on_a_rising_zero_crossing() {
        // Whatever the phase, the shown window starts where the sine rises
        // through zero, so it holds still from frame to frame.
        for phase in [0, 7, 30, 63] {
            let view = sine_view(4096, 64, phase);
            let start = trigger(&view, 1024);
            let s = view.samples();
            assert!(s[start - 1] < 0.0 && s[start] >= 0.0, "phase {phase}");
            assert!(start + 1024 <= 4096, "phase {phase}");
            assert!(start >= 4096 - 1024 - 64, "should be recent, phase {phase}");
        }
    }

    #[test]
    fn trigger_holds_low_tones_still() {
        // A period of 2000 frames is longer than the window, about 24 Hz.
        for phase in [0, 500, 1000, 1500, 1999] {
            let view = sine_view(48_000, 2000, phase);
            let start = trigger(&view, 1024);
            let s = view.samples();
            assert!(s[start - 1] < 0.0 && s[start] >= 0.0, "phase {phase}");
        }
    }

    #[test]
    fn trigger_falls_back_to_the_latest_window() {
        let telemetry = Telemetry::new();
        let mut writer = telemetry.open_scope(NodeId(1), 2, 2000);
        writer.write(2000, |_, _| 0.5);
        let mut view = ScopeView::default();
        telemetry.read_scope(NodeId(1), &mut view);
        assert_eq!(trigger(&view, 1024), 2000 - 1024);
        // Too little to fill a window.
        assert_eq!(trigger(&ScopeView::default(), 1024), 0);
    }

    #[test]
    fn columns_hold_each_slices_extremes() {
        let samples = [0.0, 1.0, -1.0, 0.5, 0.25, -0.25];
        assert_eq!(
            min_max_columns(&samples, 3, 6),
            [(0.0, 1.0), (-1.0, 0.5), (-0.25, 0.25)]
        );
        // Short of a full window: the missing columns are left out.
        assert_eq!(
            min_max_columns(&samples[..3], 3, 6),
            [(0.0, 1.0), (-1.0, -1.0)]
        );
        // More columns than samples: each column gets the sample under it.
        assert_eq!(
            min_max_columns(&[0.5, -0.5], 4, 2),
            [(0.5, 0.5), (0.5, 0.5), (-0.5, -0.5), (-0.5, -0.5)]
        );
    }

    fn reading(min: f32, max: f32, last: f32) -> ParamReading {
        ParamReading {
            value: last,
            min,
            max,
        }
    }

    #[test]
    fn a_window_marks_the_range_of_the_last_few_seconds() {
        let mut window = ParamWindow::default();
        assert_eq!(window.live(), None);
        window.push(0.0, reading(0.2, 0.8, 0.5));
        window.push(1.0, reading(0.4, 0.6, 0.5));
        let live = window.live().unwrap();
        assert_eq!((live.min, live.max, live.value), (0.2, 0.8, 0.5));

        // Once the wide swing is more than 3 s old, it drops out.
        window.push(3.5, reading(0.45, 0.55, 0.5));
        let live = window.live().unwrap();
        assert_eq!((live.min, live.max), (0.4, 0.6));
        window.push(4.5, reading(0.5, 0.5, 0.5));
        let live = window.live().unwrap();
        assert_eq!((live.min, live.max), (0.45, 0.55));
    }

    #[test]
    fn update_follows_a_wired_parameters_range() {
        let mut project = Project::new();
        let mut add = |type_id: &str| {
            let id = project.new_node_id();
            Command::AddNode {
                id,
                node: Node::new(type_id),
            }
            .apply(&mut project)
            .unwrap();
            id
        };
        let lfo = add("noodle.mod.lfo");
        let gain = add("noodle.util.gain");
        Command::Connect(noodle_core::Connection {
            from: noodle_core::Endpoint::new(lfo, "out"),
            to: noodle_core::Endpoint::new(gain, "gain"),
        })
        .apply(&mut project)
        .unwrap();

        let telemetry = Telemetry::new();
        let tap = telemetry.open_param(gain, "gain");
        let mut bodies = Bodies::default();
        // Nothing has been written yet.
        bodies.update(&telemetry, &project, &registry(), 0.016);
        assert_eq!(bodies.param_live(gain, "gain"), None);

        tap.write(&[-12.0, 6.0, 0.0]);
        let state = bodies.update(&telemetry, &project, &registry(), 0.016);
        let live = bodies.param_live(gain, "gain").unwrap();
        assert_eq!((live.min, live.max, live.value), (-12.0, 6.0, 0.0));
        assert_ne!(state, Liveness::Still);

        // Cutting the wire forgets it.
        Command::Disconnect {
            input: noodle_core::Endpoint::new(gain, "gain"),
        }
        .apply(&mut project)
        .unwrap();
        bodies.update(&telemetry, &project, &registry(), 0.016);
        assert_eq!(bodies.param_live(gain, "gain"), None);
    }

    #[test]
    fn a_spectrum_node_shows_where_its_input_has_energy_and_forgets_it_when_silent() {
        let mut project = Project::new();
        let id = project.new_node_id();
        Command::AddNode {
            id,
            node: Node::new(SPECTRUM_ID),
        }
        .apply(&mut project)
        .unwrap();
        assert!(height_of(SPECTRUM_ID) > 0.0);
        let telemetry = Telemetry::new();
        let rate = 48_000.0;
        let mut writer = telemetry.open_scope(id, 2, 16_384);
        let tone = |hz: f32| {
            move |f: usize, _c: usize| (std::f32::consts::TAU * hz * f as f32 / rate).sin()
        };
        writer.write(8192, tone(1000.0));

        let mut bodies = Bodies::default();
        // Nothing is analysed until the engine's rate is known.
        bodies.update(&telemetry, &project, &registry(), 0.02);
        assert!(
            bodies
                .spectrum(id)
                .unwrap()
                .curve()
                .iter()
                .all(|&db| db < -80.0)
        );
        bodies.set_sample_rate(rate);
        let state = bodies.update(&telemetry, &project, &registry(), 0.02);
        let trace = bodies.spectrum(id).unwrap();
        let loudest = (0..trace.curve().len())
            .max_by(|&a, &b| trace.curve()[a].total_cmp(&trace.curve()[b]))
            .unwrap();
        let hz = noodle_nodes::column_frequency(loudest, trace.curve().len(), rate);
        assert!((hz / 1000.0).log2().abs() < 1.0 / 12.0, "{hz}");
        assert!(trace.curve()[loudest] > -3.0);
        assert!(trace.peaks()[loudest] >= trace.curve()[loudest]);
        assert_ne!(state, Liveness::Still);

        // Later and quiet, the curve falls rather than jumping, and the peak
        // is held.
        writer.write(8192, |_, _| 0.0);
        writer.write(8192, |_, _| 0.0);
        bodies.update(&telemetry, &project, &registry(), 0.1);
        let trace = bodies.spectrum(id).unwrap();
        assert!(trace.curve()[loudest] < -3.0 && trace.curve()[loudest] > -20.0);
        assert!(trace.peaks()[loudest] > -3.0, "the peak is held");

        // Drawing it, with and without data, produces shapes.
        let ctx = egui::Context::default();
        let output = ctx.run_ui(egui::RawInput::default(), |ctx| {
            let painter = ctx.layer_painter(egui::LayerId::background());
            let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(200.0, 60.0));
            spectrum(&painter, rect, 1.0, bodies.spectrum(id));
            spectrum(&painter, rect, 1.0, None);
            spectrogram(&painter, rect, bodies.spectrum(id));
        });
        assert!(output.shapes.len() >= 5);
        std::mem::forget(output.textures_delta);

        Command::RemoveNode { id }.apply(&mut project).unwrap();
        bodies.update(&telemetry, &project, &registry(), 0.02);
        assert!(bodies.spectrum(id).is_none());
    }

    #[test]
    fn an_envelope_draws_its_curve_on_its_body() {
        assert!(height_of(ENVELOPE_ID) > 0.0);
        let ctx = egui::Context::default();
        let drawn = |text: &str| {
            let output = ctx.run_ui(egui::RawInput::default(), |ctx| {
                let painter = ctx.layer_painter(egui::LayerId::background());
                let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(200.0, 60.0));
                envelope(&painter, rect, 1.0, text);
            });
            let points = output
                .shapes
                .iter()
                .find_map(|clipped| match &clipped.shape {
                    egui::Shape::Path(path) => Some(path.points.clone()),
                    _ => None,
                })
                .unwrap_or_default();
            // Nothing renders these textures; dropping them unapplied panics.
            std::mem::forget(output.textures_delta);
            points
        };
        let default = drawn("");
        assert_eq!(default.len(), 65);
        // A curve drawn flat at the top is a flat line near the top.
        let flat = drawn("0 1 0;1 1 0");
        assert!(flat.iter().all(|p| (p.y - flat[0].y).abs() < 1e-3));
        assert!(flat[0].y < default[0].y, "full height is higher than 0");
        assert_ne!(default, flat);
    }

    #[test]
    fn update_reads_the_projects_meters_and_scopes() {
        let mut project = Project::new();
        let mut add = |type_id: &str| {
            let id = project.new_node_id();
            Command::AddNode {
                id,
                node: Node::new(type_id),
            }
            .apply(&mut project)
            .unwrap();
            id
        };
        let meter = add(METER_ID);
        let scope = add(SCOPE_ID);
        let output = add(OUTPUT_ID);

        let telemetry = Telemetry::new();
        let writer = telemetry.open_meter(meter, 2);
        writer.write(
            1,
            Level {
                peak: 0.5,
                rms: 0.25,
            },
        );
        let mut scope_writer = telemetry.open_scope(scope, 1, 8);
        scope_writer.write(3, |f, _| f as f32);
        // The Output node has a scope of its own.
        let mut output_writer = telemetry.open_scope(output, 2, 8);
        output_writer.write(2, |f, c| (f + 10 * c) as f32);

        let mut bodies = Bodies::default();
        let state = bodies.update(&telemetry, &project, &registry(), 1.0 / 60.0);
        assert_eq!(state, Liveness::Moving);
        assert_eq!(bodies.meters[&meter].len(), 2);
        assert_eq!(bodies.meters[&meter][1].peak, 0.5);
        assert_eq!(bodies.meters[&meter][1].rms, 0.25);
        assert_eq!(bodies.scopes[&scope].samples(), [0.0, 1.0, 2.0]);
        assert_eq!(bodies.scopes[&output].samples(), [0.0, 10.0, 1.0, 11.0]);
        assert_eq!(height_of(OUTPUT_ID), height_of(SCOPE_ID));

        Command::RemoveNode { id: meter }
            .apply(&mut project)
            .unwrap();
        bodies.update(&telemetry, &project, &registry(), 1.0 / 60.0);
        assert!(!bodies.meters.contains_key(&meter));
    }

    #[test]
    fn gain_and_voice_mix_nodes_show_their_output_level() {
        let mut project = Project::new();
        let telemetry = Telemetry::new();
        let mut writers = Vec::new();
        for kind in [noodle_nodes::GAIN_ID, noodle_nodes::VOICE_MIX_ID] {
            let id = project.new_node_id();
            Command::AddNode {
                id,
                node: Node::new(kind),
            }
            .apply(&mut project)
            .unwrap();
            assert!(height_of(kind) > 0.0, "{kind} reserves room for a meter");
            let writer = telemetry.open_meter(id, 1);
            writer.write(
                0,
                Level {
                    peak: 0.5,
                    rms: 0.25,
                },
            );
            writers.push((id, writer));
        }
        let mut bodies = Bodies::default();
        bodies.update(&telemetry, &project, &registry(), 1.0 / 60.0);
        for (id, _) in &writers {
            assert_eq!(bodies.input_meters(*id)[0].peak, 0.5);
        }
    }

    #[test]
    fn a_mixers_inputs_each_get_a_level() {
        let mut project = Project::new();
        let mix = project.new_node_id();
        Command::AddNode {
            id: mix,
            node: Node::new(spare::MIXER),
        }
        .apply(&mut project)
        .unwrap();
        let telemetry = Telemetry::new();
        let writer = telemetry.open_meter(mix, 2);
        writer.write(
            1,
            Level {
                peak: 0.5,
                rms: 0.25,
            },
        );
        let mut bodies = Bodies::default();
        bodies.update(&telemetry, &project, &registry(), 1.0 / 60.0);
        assert_eq!(bodies.input_meters(mix).len(), 2);
        assert_eq!(bodies.input_meters(mix)[0].peak, 0.0);
        assert_eq!(bodies.input_meters(mix)[1].peak, 0.5);
        assert_eq!(bodies.input_meters_of(mix, 1)[0].rms, 0.25);
        assert!(bodies.input_meters_of(mix, 5).is_empty());
    }

    /// A node type that is none of the built-ins, declaring only a view.
    struct Probe;

    static PROBE_INFO: noodle_engine::NodeInfo = noodle_engine::NodeInfo {
        id: "test.probe",
        version: 1,
        name: "Probe",
        category: "Test",
    };

    impl noodle_engine::NodeType for Probe {
        fn info(&self) -> &noodle_engine::NodeInfo {
            &PROBE_INFO
        }

        fn view(&self, _config: &noodle_engine::Config) -> NodeView {
            NodeView::Meter
        }

        fn layout(
            &self,
            _config: &noodle_engine::Config,
        ) -> Result<noodle_engine::Layout, noodle_engine::NodeError> {
            Ok(noodle_engine::Layout::realtime().input("in", "In"))
        }

        fn instantiate(
            &self,
            _setup: &noodle_engine::Setup<'_>,
        ) -> Result<noodle_engine::Instance, noodle_engine::NodeError> {
            unreachable!("the test never builds a plan")
        }
    }

    #[test]
    fn a_node_type_that_declares_a_meter_view_gets_a_meter_body_with_no_app_changes() {
        let mut registry = registry();
        registry.register(Probe);
        let mut project = Project::new();
        let id = project.new_node_id();
        Command::AddNode {
            id,
            node: Node::new("test.probe"),
        }
        .apply(&mut project)
        .unwrap();
        let view = view_of(&registry, project.graph().node(id).unwrap());
        assert_eq!(view, NodeView::Meter);
        assert_eq!(height(view), height_of(METER_ID));

        let telemetry = Telemetry::new();
        let writer = telemetry.open_meter(id, 1);
        writer.write(
            0,
            Level {
                peak: 0.5,
                rms: 0.25,
            },
        );
        let mut bodies = Bodies::default();
        bodies.update(&telemetry, &project, &registry, 1.0 / 60.0);
        assert_eq!(bodies.input_meters(id).len(), 1);
        assert_eq!(bodies.input_meters(id)[0].peak, 0.5);
    }

    #[test]
    fn views_are_declared_by_the_node_types() {
        for (type_id, view) in [
            (METER_ID, NodeView::Meter),
            (SCOPE_ID, NodeView::Scope),
            (OUTPUT_ID, NodeView::Scope),
            (SPECTRUM_ID, NodeView::Spectrum),
            (ENVELOPE_ID, NodeView::Envelope),
            (SURROUND_PAN_ID, NodeView::Panner),
            (spare::MIXER, NodeView::Mixer),
            (GROUP, NodeView::GroupMeter),
            (
                DYNAMICS_ID,
                NodeView::Graph(noodle_engine::GraphKind::Dynamics),
            ),
            ("noodle.util.gain", NodeView::Meter),
        ] {
            assert_eq!(view_of(&registry(), &Node::new(type_id)), view, "{type_id}");
        }
    }

    #[test]
    fn the_body_code_matches_no_node_type_ids() {
        let source = include_str!("body.rs");
        let code = &source[..source.find("#[cfg(test)]").unwrap()];
        let lines: Vec<&str> = code
            .lines()
            .filter(|line| !line.trim_start().starts_with("//") && line.contains("_ID"))
            .collect();
        assert!(lines.is_empty(), "{lines:?}");
    }

    #[test]
    fn a_silent_graph_goes_still_and_a_sound_decays_before_it_does() {
        let mut project = Project::new();
        let ids: Vec<NodeId> = [METER_ID, SPECTRUM_ID, OUTPUT_ID]
            .into_iter()
            .map(|type_id| {
                let id = project.new_node_id();
                Command::AddNode {
                    id,
                    node: Node::new(type_id),
                }
                .apply(&mut project)
                .unwrap();
                id
            })
            .collect();
        let telemetry = Telemetry::new();
        let meter = telemetry.open_meter(ids[0], 1);
        let mut spectrum = telemetry.open_scope(ids[1], 1, noodle_nodes::SPECTRUM_WINDOW);
        let mut output = telemetry.open_scope(ids[2], 2, 64);
        let registry = registry();
        let mut bodies = Bodies::default();
        bodies.set_sample_rate(48_000.0);
        let dt = 1.0 / 30.0;
        let mut silent_block = || {
            meter.write(0, Level::default());
            spectrum.write(256, |_, _| 0.0);
            output.write(64, |_, _| 0.0);
        };

        // Silent from the start: settled after the first look.
        silent_block();
        bodies.update(&telemetry, &project, &registry, dt);
        for _ in 0..5 {
            silent_block();
            assert_eq!(
                bodies.update(&telemetry, &project, &registry, dt),
                Liveness::Still
            );
        }

        // A sound moves the bodies, then they fall back to still.
        meter.write(
            0,
            Level {
                peak: 0.8,
                rms: 0.5,
            },
        );
        assert_eq!(
            bodies.update(&telemetry, &project, &registry, dt),
            Liveness::Moving
        );
        meter.write(0, Level::default());
        let mut states = Vec::new();
        for _ in 0..600 {
            silent_block();
            states.push(bodies.update(&telemetry, &project, &registry, dt));
        }
        assert!(states.contains(&Liveness::Decaying));
        assert_eq!(states.last(), Some(&Liveness::Still), "{states:?}");
    }
}
