//! Custom drawing inside a node, below its ports, for nodes that show
//! something other than parameters, such as meters and scopes.
//!
//! To give a node type a body: return its height from [`height`], and draw it
//! in [`paint`]. The editor reserves the space, and calls `paint` after
//! drawing the rest of the node. Anything a body shows from the engine is read
//! once a frame into [`Bodies`], by [`Bodies::update`].

use std::collections::{HashMap, VecDeque};

use egui::epaint::PathShape;
use egui::{Painter, Pos2, Rect, Stroke, Vec2};
use noodle_core::spare;
use noodle_core::{NodeId, Project};
use noodle_engine::OUTPUT_ID;
use noodle_engine::{Level, MeterReader, ParamReading, ScopeView, Telemetry};
use noodle_nodes::{METERED, SCOPE_ID};

use crate::theme::editor as colors;
use crate::widgets::Live;

/// Extra space below a node's ports, in graph units. Zero for most nodes.
pub fn height(type_id: &str) -> f32 {
    match type_id {
        id if METERED.contains(&id) => 30.0,
        SCOPE_ID | OUTPUT_ID => 80.0,
        _ => 0.0,
    }
}

/// Draws a node's body into `rect`, the space [`height`] reserved, in screen
/// points. Scale anything drawn by `zoom`.
pub fn paint(
    painter: &Painter,
    rect: Rect,
    zoom: f32,
    node: NodeId,
    type_id: &str,
    bodies: &Bodies,
) {
    let area = rect.shrink2(Vec2::new(10.0, 4.0) * zoom);
    match type_id {
        id if METERED.contains(&id) => meter(
            painter,
            area,
            zoom,
            bodies.input_meters(node),
            MeterAxis::Horizontal,
        ),
        SCOPE_ID | OUTPUT_ID => scope(painter, area, zoom, bodies.scopes.get(&node)),
        _ => {}
    }
}

/// What the bodies show, read from the engine once a frame.
#[derive(Default)]
pub struct Bodies {
    /// The editor's own reader, so other views still see every peak.
    reader: Option<MeterReader>,
    meters: HashMap<NodeId, Vec<MeterChannel>>,
    scopes: HashMap<NodeId, ScopeView>,
    /// What each wired parameter has been doing, by (node, port key).
    params: HashMap<(NodeId, String), ParamWindow>,
    /// Seconds since the first update, for the windows.
    clock: f32,
}

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
    /// Reads every meter and scope in `project` from `telemetry`. `dt` is the
    /// time since the last update, in seconds, for the peaks' decay.
    pub fn update(&mut self, telemetry: &Telemetry, project: &Project, dt: f32) {
        let graph = project.graph();
        // A new session brings a new hub.
        if !self.reader.as_ref().is_some_and(|r| r.reads(telemetry)) {
            self.reader = None;
        }
        let reader = &*self.reader.get_or_insert_with(|| telemetry.meter_reader());
        self.meters.retain(|&id, _| graph.node(id).is_some());
        self.scopes.retain(|&id, _| graph.node(id).is_some());

        // Every wired parameter. Reading one takes its range since the last
        // read, so this is the only reader: the properties panel asks `Bodies`.
        self.clock += dt;
        let now = self.clock;
        let wired: std::collections::HashSet<(NodeId, String)> = graph
            .connections()
            .map(|c| (c.to.node, c.to.port))
            .collect();
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
            match node.type_id.as_str() {
                kind if METERED.contains(&kind) => {
                    let channels = self.meters.entry(id).or_default();
                    let levels = reader.meter(id).unwrap_or_default();
                    channels.resize_with(levels.len(), MeterChannel::default);
                    for (channel, level) in channels.iter_mut().zip(levels) {
                        channel.update(level, dt);
                    }
                }
                // A mixer reports one level per input.
                spare::MIXER => {
                    let channels = self.meters.entry(id).or_default();
                    let levels = reader.meter(id).unwrap_or_default();
                    channels.resize_with(levels.len(), MeterChannel::default);
                    for (channel, level) in channels.iter_mut().zip(levels) {
                        channel.update(level, dt);
                    }
                }
                // An Output node's scope shows what it sends to the device.
                SCOPE_ID | OUTPUT_ID => {
                    let view = self.scopes.entry(id).or_insert_with(scope_view);
                    if !telemetry.read_scope(id, view) {
                        *view = scope_view();
                    }
                }
                _ => {}
            }
        }
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

    /// Whether anything is shown that changes while audio plays.
    pub fn is_live(&self) -> bool {
        !self.meters.is_empty() || !self.scopes.is_empty() || !self.params.is_empty()
    }
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
    use noodle_core::{Command, Node};
    use noodle_nodes::METER_ID;

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
        bodies.update(&telemetry, &project, 0.016);
        assert_eq!(bodies.param_live(gain, "gain"), None);

        tap.write(&[-12.0, 6.0, 0.0]);
        bodies.update(&telemetry, &project, 0.016);
        let live = bodies.param_live(gain, "gain").unwrap();
        assert_eq!((live.min, live.max, live.value), (-12.0, 6.0, 0.0));
        assert!(bodies.is_live());

        // Cutting the wire forgets it.
        Command::Disconnect {
            input: noodle_core::Endpoint::new(gain, "gain"),
        }
        .apply(&mut project)
        .unwrap();
        bodies.update(&telemetry, &project, 0.016);
        assert_eq!(bodies.param_live(gain, "gain"), None);
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
        bodies.update(&telemetry, &project, 1.0 / 60.0);
        assert!(bodies.is_live());
        assert_eq!(bodies.meters[&meter].len(), 2);
        assert_eq!(bodies.meters[&meter][1].peak, 0.5);
        assert_eq!(bodies.meters[&meter][1].rms, 0.25);
        assert_eq!(bodies.scopes[&scope].samples(), [0.0, 1.0, 2.0]);
        assert_eq!(bodies.scopes[&output].samples(), [0.0, 10.0, 1.0, 11.0]);
        assert_eq!(height(OUTPUT_ID), height(SCOPE_ID));

        Command::RemoveNode { id: meter }
            .apply(&mut project)
            .unwrap();
        bodies.update(&telemetry, &project, 1.0 / 60.0);
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
            assert!(height(kind) > 0.0, "{kind} reserves room for a meter");
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
        bodies.update(&telemetry, &project, 1.0 / 60.0);
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
        bodies.update(&telemetry, &project, 1.0 / 60.0);
        assert_eq!(bodies.input_meters(mix).len(), 2);
        assert_eq!(bodies.input_meters(mix)[0].peak, 0.0);
        assert_eq!(bodies.input_meters(mix)[1].peak, 0.5);
        assert_eq!(bodies.input_meters_of(mix, 1)[0].rms, 0.25);
        assert!(bodies.input_meters_of(mix, 5).is_empty());
    }
}
