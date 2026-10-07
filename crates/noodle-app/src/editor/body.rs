//! Custom drawing inside a node, below its ports, for nodes that show
//! something other than parameters, such as meters and scopes.
//!
//! To give a node type a body: return its height from [`height`], and draw it
//! in [`paint`]. The editor reserves the space, and calls `paint` after
//! drawing the rest of the node. Anything a body shows from the engine is read
//! once a frame into [`Bodies`], by [`Bodies::update`].

use std::collections::HashMap;

use egui::epaint::PathShape;
use egui::{Painter, Pos2, Rect, Stroke, Vec2};
use noodle_core::{NodeId, Project};
use noodle_engine::{Level, ScopeView, Telemetry};
use noodle_nodes::{METER_ID, SCOPE_ID};

use crate::theme::editor as colors;

/// Extra space below a node's ports, in graph units. Zero for most nodes.
pub fn height(type_id: &str) -> f32 {
    match type_id {
        METER_ID => 30.0,
        SCOPE_ID => 80.0,
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
        METER_ID => meter(painter, area, zoom, bodies.meters.get(&node)),
        SCOPE_ID => scope(painter, area, zoom, bodies.scopes.get(&node)),
        _ => {}
    }
}

/// What the bodies show, read from the engine once a frame.
#[derive(Default)]
pub struct Bodies {
    meters: HashMap<NodeId, Vec<MeterChannel>>,
    scopes: HashMap<NodeId, ScopeView>,
}

impl Bodies {
    /// Reads every meter and scope in `project` from `telemetry`. `dt` is the
    /// time since the last update, in seconds, for the peaks' decay.
    pub fn update(&mut self, telemetry: &Telemetry, project: &Project, dt: f32) {
        let graph = project.graph();
        self.meters.retain(|&id, _| graph.node(id).is_some());
        self.scopes.retain(|&id, _| graph.node(id).is_some());
        for (id, node) in graph.nodes() {
            match node.type_id.as_str() {
                METER_ID => {
                    let channels = self.meters.entry(id).or_default();
                    let levels = telemetry.meter(id).unwrap_or_default();
                    channels.resize_with(levels.len(), MeterChannel::default);
                    for (channel, level) in channels.iter_mut().zip(levels) {
                        channel.update(level, dt);
                    }
                }
                SCOPE_ID => {
                    let view = self.scopes.entry(id).or_default();
                    if !telemetry.read_scope(id, view) {
                        *view = ScopeView::default();
                    }
                }
                _ => {}
            }
        }
    }

    /// Whether anything is shown that changes while audio plays.
    pub fn is_live(&self) -> bool {
        !self.meters.is_empty() || !self.scopes.is_empty()
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
struct MeterChannel {
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
            self.hold_age += dt;
            if self.hold_age > PEAK_HOLD_SECONDS {
                self.hold = self.peak;
                self.hold_age = 0.0;
            }
        }
    }
}

/// Where `amplitude` sits on a meter's scale, from 0 to 1.
fn meter_fraction(amplitude: f32) -> f32 {
    let db = 20.0 * amplitude.max(1e-9).log10();
    ((db - METER_FLOOR_DB) / (METER_CEILING_DB - METER_FLOOR_DB)).clamp(0.0, 1.0)
}

/// Horizontal bars, one per channel: the RMS level filled, the peak as a bar
/// tip, and the held peak as a tick that turns red above 0 dB.
fn meter(painter: &Painter, area: Rect, zoom: f32, channels: Option<&Vec<MeterChannel>>) {
    painter.rect_filled(area, 2.0 * zoom, colors::BODY);
    let channels = channels.map_or(&[][..], Vec::as_slice);
    let count = channels.len().max(1) as f32;
    let gap = 2.0 * zoom;
    let bar_height = (area.height() - gap * (count + 1.0)) / count;
    let x_at = |amplitude: f32| area.left() + area.width() * meter_fraction(amplitude);

    for (i, channel) in channels.iter().enumerate() {
        let top = area.top() + gap + i as f32 * (bar_height + gap);
        let bar = Rect::from_min_max(
            Pos2::new(area.left(), top),
            Pos2::new(area.right(), top + bar_height),
        );
        let fill = |amplitude: f32, color| {
            let right = x_at(amplitude);
            if right > bar.left() {
                painter.rect_filled(bar.with_max_x(right), 0.0, color);
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
            painter.vline(x_at(channel.hold), bar.y_range(), Stroke::new(zoom, color));
        }
    }
    // 0 dB, so overs are easy to see.
    painter.vline(
        x_at(1.0),
        area.y_range(),
        Stroke::new(zoom, colors::TEXT_WEAK),
    );
}

/// How many frames of a scope are shown: about 21 ms at 48 kHz.
const SCOPE_FRAMES: usize = 1024;

/// One strip per channel, each a min/max trace per pixel column so dense
/// waveforms stay readable, full scale ±1.
fn scope(painter: &Painter, area: Rect, zoom: f32, view: Option<&ScopeView>) {
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

/// The first frame to show of `window` frames, so a steady waveform holds
/// still: the latest rising zero crossing of channel 0 that leaves a full
/// window after it, or else the start of the latest window.
fn trigger(view: &ScopeView, window: usize) -> usize {
    let channels = view.channels().max(1);
    let samples = view.samples();
    let frames = samples.len() / channels;
    let latest = frames.saturating_sub(window);
    let first = latest.saturating_sub(window);
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

    #[test]
    fn meter_scale_runs_from_floor_to_ceiling() {
        assert_eq!(meter_fraction(0.0), 0.0);
        assert_eq!(meter_fraction(10.0), 1.0);
        let zero_db = -METER_FLOOR_DB / (METER_CEILING_DB - METER_FLOOR_DB);
        assert!((meter_fraction(1.0) - zero_db).abs() < 1e-6);
        assert!(meter_fraction(0.5) < meter_fraction(1.0));
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

        // Past the hold time, the hold drops to the peak.
        for _ in 0..6 {
            channel.update(Level::default(), 0.1);
        }
        assert!(channel.hold < 0.1, "{channel:?}");
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

        let mut bodies = Bodies::default();
        bodies.update(&telemetry, &project, 1.0 / 60.0);
        assert!(bodies.is_live());
        assert_eq!(bodies.meters[&meter].len(), 2);
        assert_eq!(bodies.meters[&meter][1].peak, 0.5);
        assert_eq!(bodies.meters[&meter][1].rms, 0.25);
        assert_eq!(bodies.scopes[&scope].samples(), [0.0, 1.0, 2.0]);

        Command::RemoveNode { id: meter }
            .apply(&mut project)
            .unwrap();
        bodies.update(&telemetry, &project, 1.0 / 60.0);
        assert!(!bodies.meters.contains_key(&meter));
    }
}
