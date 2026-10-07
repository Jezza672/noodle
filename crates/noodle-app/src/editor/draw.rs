//! Painting the editor. Nothing here changes any state.

use egui::epaint::{CornerRadius, CubicBezierShape, PathShape, StrokeKind};
use egui::{Align2, Color32, FontId, Painter, Pos2, Rect, Shape, Stroke, Vec2};
use noodle_core::Endpoint;
use noodle_engine::{ParamInfo, ParamKind, Taper, Unit};

use super::layout::{NodeGeom, PortGeom, PortKind, Side};
use super::view::Transform;
use super::{EditorState, Frame_, Gesture, Problems, StrokeAction, body, wire};
use crate::theme::{self, editor as colors};

/// Below this zoom, text is too small to read, so it isn't drawn.
const MIN_TEXT_ZOOM: f32 = 0.4;
const GRID: f32 = 20.0;

pub fn canvas(painter: &Painter, rect: Rect, t: &Transform) {
    painter.rect_filled(rect, 0.0, theme::CANVAS);
    // Fine lines only when they're far enough apart to be useful.
    let fine = t.scale(GRID) >= 8.0;
    let step = if fine { GRID } else { GRID * 5.0 };
    let min = t.to_graph(rect.min);
    let max = t.to_graph(rect.max);
    let line = |i: i64| {
        if i % 5 == 0 {
            colors::GRID_MAJOR
        } else {
            colors::GRID
        }
    };
    let mut i = (min.x / step).floor() as i64;
    while i as f32 * step <= max.x {
        let x = t.to_screen(Pos2::new(i as f32 * step, 0.0)).x;
        let color = if fine { line(i) } else { colors::GRID };
        painter.vline(x, rect.y_range(), Stroke::new(1.0, color));
        i += 1;
    }
    let mut i = (min.y / step).floor() as i64;
    while i as f32 * step <= max.y {
        let y = t.to_screen(Pos2::new(0.0, i as f32 * step)).y;
        let color = if fine { line(i) } else { colors::GRID };
        painter.hline(rect.x_range(), y, Stroke::new(1.0, color));
        i += 1;
    }
}

pub fn frames(painter: &Painter, f: &Frame_<'_>, state: &EditorState) {
    let z = f.t.zoom;
    for &i in &f.frame_order {
        let frame = &f.scene.frames[i];
        let rect = f.t.rect_to_screen(frame.rect);
        painter.rect_filled(rect, 4.0 * z, colors::FRAME);
        if state.selected_frames.contains(&frame.id) {
            painter.rect_stroke(
                rect,
                4.0 * z,
                Stroke::new(1.5, colors::SELECTED),
                StrokeKind::Outside,
            );
        }
        let renaming = state.rename.as_ref().is_some_and(|r| r.id == frame.id);
        if z >= MIN_TEXT_ZOOM && !renaming {
            let header = f.t.rect_to_screen(frame.header());
            painter.with_clip_rect(header).text(
                header.left_center() + Vec2::new(8.0 * z, 0.0),
                Align2::LEFT_CENTER,
                &frame.label,
                FontId::proportional(14.0 * z),
                colors::TEXT,
            );
        }
        // The resize grip: two short diagonals in the corner.
        let corner = rect.max - Vec2::splat(3.0 * z);
        for length in [5.0, 9.0] {
            let d = length * z;
            painter.line_segment(
                [corner - Vec2::new(d, 0.0), corner - Vec2::new(0.0, d)],
                Stroke::new(1.0, colors::TEXT_WEAK),
            );
        }
    }
}

pub fn wires(
    painter: &Painter,
    f: &Frame_<'_>,
    state: &EditorState,
    problems: &Problems,
    detached: Option<&Endpoint>,
) {
    let z = f.t.zoom;
    for wire in &f.scene.wires {
        if detached == Some(&wire.connection.to) {
            continue; // being dragged
        }
        let problem = problems.wires.contains_key(&wire.connection.to);
        let selected = state.selected.contains(&wire.connection.from.node)
            || state.selected.contains(&wire.connection.to.node);
        let color = if problem {
            colors::PROBLEM
        } else if wire.event {
            colors::EVENT_WIRE
        } else if selected {
            colors::WIRE_SELECTED
        } else {
            colors::WIRE
        };
        let points = wire::curve(wire.from, wire.to).map(|p| f.t.to_screen(p));
        curve(painter, points, Stroke::new((2.0 * z).max(1.0), color));
        if problem && z >= MIN_TEXT_ZOOM {
            let middle = wire::flatten(wire::curve(wire.from, wire.to))[12];
            warning(painter, f.t.to_screen(middle), z, Align2::CENTER_CENTER);
        }
    }
}

pub fn nodes(painter: &Painter, f: &Frame_<'_>, state: &EditorState, problems: &Problems) {
    for &i in &f.order {
        let node = &f.scene.nodes[i];
        let problem = problems.nodes.contains_key(&node.id) || node.error.is_some();
        let outline = if problem {
            Stroke::new(1.5, colors::PROBLEM)
        } else if state.active == Some(node.id) {
            Stroke::new(1.5, colors::ACTIVE)
        } else if state.selected.contains(&node.id) {
            Stroke::new(1.5, colors::SELECTED)
        } else {
            Stroke::new(1.0, colors::NODE_OUTLINE)
        };
        if node.reroute {
            reroute(painter, f, node, outline);
        } else {
            boxed(painter, f, node, outline, problem, &state.bodies);
        }
    }
}

fn reroute(painter: &Painter, f: &Frame_<'_>, node: &NodeGeom, outline: Stroke) {
    let rect = f.t.rect_to_screen(node.rect);
    let radius = rect.height() / 2.0;
    painter.rect(rect, radius, colors::NODE, outline, StrokeKind::Outside);
    for port in &node.ports {
        socket(painter, f, port);
    }
}

fn boxed(
    painter: &Painter,
    f: &Frame_<'_>,
    node: &NodeGeom,
    outline: Stroke,
    problem: bool,
    bodies: &body::Bodies,
) {
    let z = f.t.zoom;
    let rect = f.t.rect_to_screen(node.rect);
    let radius = 4.0 * z;
    painter.rect_filled(rect, radius, colors::NODE);
    let header = f.t.rect_to_screen(node.header());
    let top = CornerRadius {
        nw: radius as u8,
        ne: radius as u8,
        sw: 0,
        se: 0,
    };
    painter.rect_filled(header, top, colors::header(&node.category));
    painter.rect_stroke(rect, radius, outline, StrokeKind::Outside);

    let text = z >= MIN_TEXT_ZOOM;
    if text {
        let clipped = painter.with_clip_rect(header.shrink(2.0 * z));
        clipped.text(
            header.left_center() + Vec2::new(8.0 * z, 0.0),
            Align2::LEFT_CENTER,
            &node.title,
            FontId::proportional(13.0 * z),
            colors::TEXT,
        );
        if problem {
            warning(
                painter,
                header.right_center() - Vec2::new(8.0 * z, 0.0),
                z,
                Align2::RIGHT_CENTER,
            );
        }
    }

    let graph = f.project.graph();
    let values = graph.node(node.id).map(|n| &n.params);
    for port in &node.ports {
        if text {
            let row = f.t.rect_to_screen(port.row);
            let connected = port.side == Side::Input
                && graph
                    .source(&Endpoint::new(node.id, port.key.clone()))
                    .is_some();
            match (&port.kind, port.side) {
                (PortKind::Param(info), Side::Input) if !connected => {
                    let value = values
                        .and_then(|v| v.get(&port.key))
                        .copied()
                        .unwrap_or(info.default);
                    param_field(painter, row, z, &port.name, info, value);
                }
                (_, side) => {
                    let (anchor, align) = match side {
                        Side::Input => (
                            row.left_center() + Vec2::new(12.0 * z, 0.0),
                            Align2::LEFT_CENTER,
                        ),
                        Side::Output => (
                            row.right_center() - Vec2::new(12.0 * z, 0.0),
                            Align2::RIGHT_CENTER,
                        ),
                    };
                    let color = if port.kind == PortKind::Unknown {
                        colors::PROBLEM
                    } else {
                        colors::TEXT
                    };
                    painter.text(
                        anchor,
                        align,
                        &port.name,
                        FontId::proportional(12.0 * z),
                        color,
                    );
                }
            }
        }
        socket(painter, f, port);
    }

    if let Some(area) = node.body {
        body::paint(
            painter,
            f.t.rect_to_screen(area),
            z,
            node.id,
            graph.node(node.id).map_or("", |n| &n.type_id),
            bodies,
        );
    }
}

/// A read-only stand-in for the parameter widget, which comes from
/// `crate::widgets`: Blender's slider, showing the name and value.
fn param_field(painter: &Painter, row: Rect, z: f32, name: &str, info: &ParamInfo, value: f32) {
    let field = row.shrink2(Vec2::new(10.0 * z, 2.0 * z));
    let radius = 3.0 * z;
    painter.rect_filled(field, radius, colors::PARAM_FIELD);
    let filled = field.with_max_x(field.left() + field.width() * fraction(info, value));
    if filled.width() > 0.0 {
        painter.rect_filled(filled, radius, colors::PARAM_FILL);
    }
    let font = FontId::proportional(12.0 * z);
    let clipped = painter.with_clip_rect(field);
    clipped.text(
        field.left_center() + Vec2::new(6.0 * z, 0.0),
        Align2::LEFT_CENTER,
        name,
        font.clone(),
        colors::TEXT,
    );
    clipped.text(
        field.right_center() - Vec2::new(6.0 * z, 0.0),
        Align2::RIGHT_CENTER,
        format_value(info, value),
        font,
        colors::TEXT,
    );
}

/// How far along its range a value is, from 0 to 1, following the taper.
pub fn fraction(info: &ParamInfo, value: f32) -> f32 {
    let f = match info.taper {
        Taper::Log if info.min > 0.0 && value > 0.0 => {
            (value / info.min).ln() / (info.max / info.min).ln()
        }
        _ => (value - info.min) / (info.max - info.min),
    };
    if f.is_finite() {
        f.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

pub fn format_value(info: &ParamInfo, value: f32) -> String {
    if let ParamKind::Stepped { labels } = &info.kind {
        let step = value.round();
        return match labels.get(step as usize) {
            Some(label) if step >= 0.0 => label.to_string(),
            _ => format!("{step}"),
        };
    }
    match info.unit {
        Unit::Hertz if value.abs() >= 1000.0 => format!("{:.2} kHz", value / 1000.0),
        Unit::Hertz => format!("{value:.1} Hz"),
        Unit::Decibels => format!("{value:.1} dB"),
        Unit::Seconds if value.abs() < 1.0 => format!("{:.0} ms", value * 1000.0),
        Unit::Seconds => format!("{value:.2} s"),
        Unit::Semitones => format!("{value:+.1} st"),
        Unit::Percent => format!("{value:.0}%"),
        Unit::None => format!("{value:.3}"),
    }
}

fn socket(painter: &Painter, f: &Frame_<'_>, port: &PortGeom) {
    let z = f.t.zoom;
    let center = f.t.to_screen(port.socket);
    let radius = (4.5 * z).max(2.5);
    let outline = Stroke::new(1.0, colors::NODE_OUTLINE);
    match port.kind {
        PortKind::Event => {
            let r = radius * 1.2;
            let points = vec![
                center + Vec2::new(0.0, -r),
                center + Vec2::new(r, 0.0),
                center + Vec2::new(0.0, r),
                center + Vec2::new(-r, 0.0),
            ];
            painter.add(PathShape::convex_polygon(
                points,
                colors::EVENT_SOCKET,
                outline,
            ));
        }
        ref kind => {
            let fill = match kind {
                PortKind::Audio => colors::AUDIO_SOCKET,
                PortKind::Param(_) => colors::PARAM_SOCKET,
                _ => colors::PROBLEM,
            };
            painter.circle(center, radius, fill, outline);
        }
    }
}

fn warning(painter: &Painter, at: Pos2, z: f32, align: Align2) {
    painter.text(
        at,
        align,
        "⚠",
        FontId::proportional(13.0 * z),
        colors::PROBLEM,
    );
}

fn curve(painter: &Painter, points: [Pos2; 4], stroke: Stroke) {
    painter.add(CubicBezierShape::from_points_stroke(
        points,
        false,
        Color32::TRANSPARENT,
        stroke,
    ));
}

/// What the current drag looks like: the wire being dragged, the selection
/// box, or the stroke.
pub fn gesture(painter: &Painter, f: &Frame_<'_>, gesture: &Gesture, pointer: Pos2) {
    let z = f.t.zoom;
    match gesture {
        Gesture::Link { anchor, side, .. } => {
            let Some(port) = f.scene.port(anchor, *side) else {
                return;
            };
            let anchor = port.socket;
            let end = f.t.to_graph(pointer);
            let (from, to) = match side {
                Side::Output => (anchor, end),
                Side::Input => (end, anchor),
            };
            let points = wire::curve(from, to).map(|p| f.t.to_screen(p));
            curve(
                painter,
                points,
                Stroke::new((2.0 * z).max(1.0), colors::WIRE_SELECTED),
            );
        }
        Gesture::BoxSelect { start, .. } => {
            let rect = Rect::from_two_pos(f.t.to_screen(*start), pointer);
            painter.rect(
                rect,
                0.0,
                colors::BOX_SELECT,
                Stroke::new(1.0, colors::TEXT_WEAK),
                StrokeKind::Inside,
            );
        }
        Gesture::Stroke { points, action } => {
            let mut line: Vec<Pos2> = points.iter().map(|&p| f.t.to_screen(p)).collect();
            line.push(pointer);
            let color = match action {
                StrokeAction::Cut => colors::CUT,
                StrokeAction::Reroute => colors::WIRE_SELECTED,
            };
            painter.extend(Shape::dashed_line(&line, Stroke::new(1.5, color), 6.0, 4.0));
        }
        Gesture::Idle | Gesture::Pan | Gesture::Move { .. } | Gesture::Resize { .. } => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fraction_follows_the_taper() {
        let linear = ParamInfo::new(-60.0, 24.0, 0.0);
        assert_eq!(fraction(&linear, -60.0), 0.0);
        assert_eq!(fraction(&linear, 24.0), 1.0);
        assert_eq!(fraction(&linear, 100.0), 1.0);
        let log = ParamInfo::new(20.0, 20_000.0, 440.0).log();
        assert!(
            (fraction(&log, 632.4555) - 0.5).abs() < 1e-3,
            "the geometric middle"
        );
        assert_eq!(fraction(&ParamInfo::new(1.0, 1.0, 1.0), 1.0), 0.0);
    }

    #[test]
    fn values_are_formatted_with_units() {
        let hz = ParamInfo::new(20.0, 20_000.0, 440.0).unit(Unit::Hertz);
        assert_eq!(format_value(&hz, 440.0), "440.0 Hz");
        assert_eq!(format_value(&hz, 2500.0), "2.50 kHz");
        let db = ParamInfo::new(-60.0, 24.0, 0.0).unit(Unit::Decibels);
        assert_eq!(format_value(&db, -6.0), "-6.0 dB");
        let s = ParamInfo::new(0.0, 10.0, 0.0).unit(Unit::Seconds);
        assert_eq!(format_value(&s, 0.25), "250 ms");
        let choice = ParamInfo::choice(["Low", "High"]);
        assert_eq!(format_value(&choice, 0.9), "High");
        assert_eq!(format_value(&choice, 7.0), "7");
    }
}
