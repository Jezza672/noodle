//! Painting the editor. Nothing here changes any state.

use egui::epaint::{CornerRadius, CubicBezierShape, PathShape, StrokeKind};
use egui::{Align2, Color32, FontId, Painter, Pos2, Rect, Shape, Stroke, Vec2};
use noodle_core::Endpoint;

use super::layout::{NodeGeom, PortGeom, PortKind, Side};
use super::view::Transform;
use super::{EditorState, Frame_, Gesture, Problems, Splice, StrokeAction, body, wire};
use crate::theme::{self, editor as colors};

/// Below this zoom, text is too small to read, so it isn't drawn.
pub const MIN_TEXT_ZOOM: f32 = 0.4;
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
        painter.rect(
            rect,
            f32::from(theme::NODE_RADIUS + 4) * z,
            colors::FRAME,
            Stroke::new(1.5, colors::FRAME_OUTLINE),
            StrokeKind::Inside,
        );
        if state.selected_frames.contains(&frame.id) {
            painter.rect_stroke(
                rect,
                f32::from(theme::NODE_RADIUS + 4) * z,
                Stroke::new(1.5, colors::SELECTED),
                StrokeKind::Outside,
            );
        }
        let renaming = state
            .rename
            .as_ref()
            .is_some_and(|r| r.target == super::RenameTarget::Frame(frame.id));
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
    splice: Option<&Splice>,
) {
    let z = f.t.zoom;
    for wire in &f.scene.wires {
        if detached == Some(&wire.connection.to) {
            continue; // being dragged
        }
        let problem = problems.wires.contains_key(&wire.connection.to);
        let selected = state.selected.contains(&wire.connection.from.node)
            || state.selected.contains(&wire.connection.to.node);
        let splicing = splice.is_some_and(|s| s.wire == wire.connection);
        let color = if problem {
            colors::PROBLEM
        } else if splicing {
            colors::ACTIVE
        } else if wire.event {
            colors::EVENT_WIRE
        } else if selected {
            colors::WIRE_SELECTED
        } else {
            colors::WIRE
        };
        let points = wire::curve(wire.from, wire.to).map(|p| f.t.to_screen(p));
        cable(painter, points, z, color);
        if problem && z >= MIN_TEXT_ZOOM {
            let middle = wire::flatten(wire::curve(wire.from, wire.to))[12];
            warning(painter, f.t.to_screen(middle), z, Align2::CENTER_CENTER);
        }
    }
}

/// Paints the nodes, bottom first, calling `then` after each one so widgets
/// on it stack the same way.
pub fn nodes(
    painter: &Painter,
    f: &Frame_<'_>,
    state: &EditorState,
    problems: &Problems,
    mut then: impl FnMut(&NodeGeom),
) {
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
        then(node);
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
    let radius = f32::from(theme::NODE_RADIUS) * z;
    // A soft drop shadow lifts the node off the canvas.
    painter.rect_filled(
        rect.translate(Vec2::new(0.0, 3.0 * z)).expand(1.0 * z),
        radius,
        Color32::from_black_alpha(100),
    );
    painter.rect_filled(rect, radius, colors::NODE);
    let header = f.t.rect_to_screen(node.header());
    let top = CornerRadius {
        nw: radius as u8,
        ne: radius as u8,
        sw: 0,
        se: 0,
    };
    painter.rect_filled(header, top, colors::NODE_HEADER);
    painter.circle_filled(
        header.left_center() + Vec2::new(13.0 * z, 0.0),
        3.5 * z,
        colors::header(&node.category),
    );
    painter.rect_stroke(rect, radius, outline, StrokeKind::Outside);

    let text = z >= MIN_TEXT_ZOOM;
    if text {
        // Leave room on the right for the warning and a header socket.
        let clipped = painter.with_clip_rect(Rect::from_min_max(
            header.min + Vec2::splat(2.0 * z),
            header.max - Vec2::new(22.0 * z, 2.0 * z),
        ));
        clipped.text(
            header.left_center() + Vec2::new(24.0 * z, 0.0),
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
    for port in &node.ports {
        if text && !port.in_header {
            let row = f.t.rect_to_screen(port.row);
            let connected = port.side == Side::Input
                && graph
                    .source(&Endpoint::new(node.id, port.key.clone()))
                    .is_some();
            match (&port.kind, port.side) {
                // The parameter's field is a widget, shown by `params`.
                (PortKind::Param(_), Side::Input) if !connected => {}
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
                    } else if port.spare || port.idle {
                        colors::TEXT_WEAK
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
        if text
            && !port.spare
            && port.side == Side::Input
            && let Some(channel) = mixer_channel(graph, node.id, &port.key)
        {
            let row = f.t.rect_to_screen(port.row);
            let area = Rect::from_center_size(
                Pos2::new(row.right() - 30.0 * z, row.center().y),
                Vec2::new(48.0 * z, 7.0 * z),
            );
            body::meter(painter, area, z, bodies.input_meters_of(node.id, channel));
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

fn socket(painter: &Painter, f: &Frame_<'_>, port: &PortGeom) {
    let z = f.t.zoom;
    let center = f.t.to_screen(port.socket);
    let radius = (4.5 * z).max(2.5);
    // The ring matches the node body, so a socket reads as a notch in it.
    let outline = Stroke::new(1.5, colors::NODE);
    if port.spare {
        // Nothing is stored for a spare until a wire goes to it.
        painter.circle(
            center,
            radius,
            Color32::TRANSPARENT,
            Stroke::new(1.0, colors::TEXT_WEAK),
        );
        return;
    }
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
            let fill = if port.idle {
                fill.gamma_multiply(0.6)
            } else {
                fill
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

/// A wire drawn as a thick cable with a dark core.
fn cable(painter: &Painter, points: [Pos2; 4], z: f32, color: Color32) {
    curve(painter, points, Stroke::new((4.0 * z).max(1.5), color));
    curve(
        painter,
        points,
        Stroke::new((1.4 * z).max(0.5), colors::WIRE_CORE),
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
            cable(painter, points, z, colors::WIRE_SELECTED);
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
        Gesture::Reorder {
            node,
            side,
            key,
            target,
        } => {
            // A line where the port would land.
            let Some(node) = f.scene.node(*node) else {
                return;
            };
            let rest: Vec<&PortGeom> = node
                .ports
                .iter()
                .filter(|p| p.side == *side && p.key != *key)
                .collect();
            let y = match rest.get(*target) {
                Some(port) => port.row.top(),
                None => rest
                    .last()
                    .map_or(node.rect.top(), |port| port.row.bottom()),
            };
            let (x0, x1) = match side {
                Side::Input => (node.rect.left(), node.rect.center().x),
                Side::Output => (node.rect.center().x, node.rect.right()),
            };
            let a = f.t.to_screen(Pos2::new(x0, y));
            let b = f.t.to_screen(Pos2::new(x1, y));
            painter.line_segment([a, b], Stroke::new(2.0, colors::WIRE_SELECTED));
        }
        Gesture::Idle | Gesture::Pan | Gesture::Move { .. } | Gesture::Resize { .. } => {}
    }
}

/// The meter channel of a mixer node's input `key`: its position from 0.
fn mixer_channel(graph: &noodle_core::Graph, id: noodle_core::NodeId, key: &str) -> Option<usize> {
    let mixer = graph.node(id)?.type_id == noodle_core::spare::MIXER;
    let index = noodle_core::spare::mixer_input_index(key)?;
    mixer.then(|| index - 1)
}
