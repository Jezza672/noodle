//! Painting the editor. Nothing here changes any state.

use egui::epaint::{CornerRadius, CubicBezierShape, PathShape, StrokeKind};
use egui::{Align2, Color32, FontId, Painter, Pos2, Rect, Shape, Stroke, Vec2};
use noodle_core::{Endpoint, NodeId};
use noodle_engine::{BlockStats, CpuReading};

use super::layout::{NodeGeom, PortGeom, PortKind, Side};
use super::view::Transform;
use super::{EditorState, Frame_, Gesture, Problems, Splice, StrokeAction, body, wire};
use crate::freezing::{Badge, BadgeState};
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
        } else if state.selected_wire.as_ref() == Some(&wire.connection) {
            colors::SELECTED
        } else if wire.event {
            colors::EVENT_WIRE
        } else if wire.spectral {
            colors::SPECTRAL_WIRE
        } else if wire.sample {
            colors::SAMPLE_WIRE
        } else if selected {
            colors::WIRE_SELECTED
        } else {
            colors::WIRE
        };
        let points = wire::curve(wire.from, wire.to).map(|p| f.t.to_screen(p));
        let (glow, clipping) = if problem || splicing {
            (0.0, false)
        } else {
            wire_glow(&state.bodies, f.project.graph(), wire)
        };
        if glow > 0.0 || clipping {
            let lit = if clipping { colors::METER_OVER } else { color };
            let amount = if clipping { 1.0 } else { glow };
            // A soft halo under the cable, and the cable itself brightening
            // towards white as the signal gets stronger.
            curve(
                painter,
                points,
                Stroke::new((9.0 * z).max(3.0), lit.gamma_multiply(0.30 * amount)),
            );
            let bright = lerp_color(lit, Color32::WHITE, 0.55 * amount);
            cable(painter, points, z, bright);
        } else {
            cable(painter, points, z, color);
        }
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
    hovered: Option<NodeId>,
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
        } else if hovered == Some(node.id) {
            Stroke::new(1.5, colors::HOVERED)
        } else {
            Stroke::new(1.0, colors::NODE_OUTLINE)
        };
        if node.reroute {
            reroute(painter, f, node, outline);
        } else {
            boxed(painter, f, node, outline, problem, state);
        }
        then(node);
        if !node.reroute && fades(state, node.id) {
            painter.rect_filled(
                f.t.rect_to_screen(node.rect),
                f32::from(theme::NODE_RADIUS) * f.t.zoom,
                Color32::from_black_alpha(95),
            );
        }
    }
}

/// Whether a node is drawn faded: it made no sound in its last block, or
/// nothing listens to it. A selected node stays clear, so it can be worked on.
fn fades(state: &EditorState, id: NodeId) -> bool {
    state.bodies.display().silent_dim
        && !state.selected.contains(&id)
        && state
            .bodies
            .node_state(id)
            .is_some_and(|node| !node.ran || node.silent)
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
    state: &EditorState,
) {
    let bodies = &state.bodies;
    let badge = state.freeze.get(&node.id);
    let monitored = state.monitored.contains(&node.id);
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
    if !problem {
        if let Some(pulse) = bodies.beat_at(node.id)
            && pulse > 0.01
        {
            painter.rect_stroke(
                rect.expand(1.5 * z),
                radius + 1.5 * z,
                Stroke::new(2.0 * z, colors::ACTIVE.gamma_multiply(0.85 * pulse)),
                StrokeKind::Outside,
            );
        }
        if let Some(load) = bodies.cpu_heat(node.id) {
            // Nothing at a hundredth of the block, fully hot at a quarter.
            let heat = ((load - 0.01) / 0.24).clamp(0.0, 1.0);
            if heat > 0.0 {
                let color = lerp_color(Color32::from_rgb(240, 200, 64), colors::METER_OVER, heat);
                painter.rect_stroke(
                    rect.expand(1.5 * z),
                    radius + 1.5 * z,
                    Stroke::new(2.5 * z, color.gamma_multiply(0.35 + 0.65 * heat)),
                    StrokeKind::Outside,
                );
            }
        }
    }
    if bodies.display().voice_leds
        && let Some(state) = bodies.node_state(node.id)
        && state.voices > 1
    {
        // One dot for each voice, the first few lit for those that sound.
        let shown = usize::from(state.voices).min(16);
        for voice in 0..shown {
            let lit = voice < usize::from(state.sounding);
            painter.circle_filled(
                rect.left_bottom() + Vec2::new((10.0 + 5.0 * voice as f32) * z, -4.0 * z),
                1.8 * z,
                if lit {
                    colors::ACTIVE
                } else {
                    colors::NODE_OUTLINE
                },
            );
        }
    }

    let text = z >= MIN_TEXT_ZOOM;
    let monitor = super::monitor_button(f.project, node);
    let arm = super::arm_button(f.project, node);
    // The button sits at the right end of the header, ahead of what is
    // drawn there.
    let button_room = if monitor.is_some() || arm.is_some() {
        20.0
    } else {
        0.0
    };
    if let Some(button) = arm {
        arm_button(
            painter,
            f.t.rect_to_screen(button),
            state.armed.contains(&node.id),
        );
    }
    if let Some(button) = monitor {
        monitor_button(painter, f.t.rect_to_screen(button), monitored);
    }
    if text {
        // Leave room on the right for the warning and a header socket.
        let clipped = painter.with_clip_rect(Rect::from_min_max(
            header.min + Vec2::splat(2.0 * z),
            header.max - Vec2::new((22.0 + button_room) * z, 2.0 * z),
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
                header.right_center() - Vec2::new((8.0 + button_room) * z, 0.0),
                z,
                Align2::RIGHT_CENTER,
            );
        }
        let mut right = button_room + if problem { 24.0 } else { 10.0 };
        if let Some(cpu) = bodies.cpu(node.id) {
            let (label, color) = cpu_label(cpu);
            let galley = painter.text(
                header.right_center() - Vec2::new(right * z, 0.0),
                Align2::RIGHT_CENTER,
                label,
                FontId::monospace(9.0 * z),
                color,
            );
            right += galley.width() / z + 6.0;
        }
        if let Some(label) = state.latency.get(&node.id) {
            let galley = painter.text(
                header.right_center() - Vec2::new(right * z, 0.0),
                Align2::RIGHT_CENTER,
                label,
                FontId::monospace(9.0 * z),
                colors::TEXT_WEAK,
            );
            right += galley.width() / z + 6.0;
        }
        if let Some(badge) = badge {
            let (label, color) = badge_label(badge);
            if !label.is_empty() {
                painter.text(
                    header.right_center() - Vec2::new(right * z, 0.0),
                    Align2::RIGHT_CENTER,
                    label,
                    FontId::proportional(10.0 * z),
                    color,
                );
            }
        }
    }
    // A render under way: a bar along the foot of the header.
    if let Some(Badge {
        state: BadgeState::Rendering(fraction),
        ..
    }) = badge
    {
        let thickness = (2.5 * z).max(1.5);
        let track = Rect::from_min_max(
            Pos2::new(header.left(), header.bottom() - thickness),
            header.right_bottom(),
        );
        painter.rect_filled(track, 0.0, colors::NODE_OUTLINE);
        let mut filled = track;
        filled.set_right(track.left() + track.width() * fraction.clamp(0.0, 1.0));
        painter.rect_filled(filled, 0.0, colors::ACTIVE);
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
                            row.left_center() + Vec2::new(PORT_LABEL_INSET * z, 0.0),
                            Align2::LEFT_CENTER,
                        ),
                        Side::Output => (
                            row.right_center() - Vec2::new(PORT_LABEL_INSET * z, 0.0),
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
                        FontId::proportional(PORT_LABEL_SIZE * z),
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
            if let Some(area) = row_meter_area(painter, row, z, &port.name) {
                body::meter(
                    painter,
                    area,
                    z,
                    bodies.input_meters_of(node.id, channel),
                    body::MeterAxis::Horizontal,
                );
            }
        }
        // Any other wired audio input shows the level arriving at it.
        if text
            && !port.spare
            && port.side == Side::Input
            && mixer_channel(graph, node.id, &port.key).is_none()
        {
            let levels = bodies.tap(node.id, &port.key);
            if !levels.is_empty() {
                if port.in_header {
                    // A lone input sits on the title bar, whose text fills it:
                    // the level goes in a thin bar along the foot instead.
                    let header = f.t.rect_to_screen(node.header());
                    let area = header_meter_area(header, Side::Input, z);
                    body::meter(painter, area, z, levels, body::MeterAxis::Horizontal);
                } else {
                    let row = f.t.rect_to_screen(port.row);
                    if let Some(area) = row_meter_area(painter, row, z, &port.name) {
                        body::meter(painter, area, z, levels, body::MeterAxis::Horizontal);
                    }
                }
            }
        }
        // Any audio output shows the level leaving it, as a thin bar along
        // the bottom of its row (or of the title bar, for a port that sits
        // there).
        if text && !port.spare && port.side == Side::Output && port.kind == PortKind::Audio {
            let levels = bodies.output_tap(node.id, &port.key);
            if !levels.is_empty() {
                let edge = if port.in_header {
                    f.t.rect_to_screen(node.header())
                } else {
                    f.t.rect_to_screen(port.row)
                };
                let area = if port.in_header {
                    header_meter_area(edge, Side::Output, z)
                } else {
                    Rect::from_min_size(
                        Pos2::new(edge.right() - 62.0 * z, edge.bottom() - 5.0 * z),
                        Vec2::new(48.0 * z, 4.0 * z),
                    )
                };
                body::meter(painter, area, z, levels, body::MeterAxis::Horizontal);
            }
        }
        socket(painter, f, port);
    }

    if let Some(area) = node.body {
        let node_id = node.id;
        // What each parameter is now: a wired one's live value, else the
        // node's own, else its default, shown on the node or not (a graph
        // reads parameters the node doesn't list as ports).
        let defaults = param_defaults(f.registry, graph.node(node_id));
        let values = |key: &str| {
            let Some(own) = graph.node(node_id) else {
                return 0.0;
            };
            let default = defaults.iter().find(|(k, _)| k == key).map(|&(_, d)| d);
            crate::widgets::graph::graph_value(
                key,
                own,
                graph.source(&Endpoint::new(node_id, key)).is_some(),
                &|channel| bodies.param_live(node_id, channel).map(|live| live.value),
                default.unwrap_or(0.0),
            )
        };
        let curve = match graph
            .node(node.id)
            .and_then(|n| n.config.get("curve").or_else(|| n.config.get("layout")))
        {
            Some(noodle_core::Value::Text(text)) => text.clone(),
            _ => String::new(),
        };
        body::paint(
            painter,
            f.t.rect_to_screen(area),
            z,
            node.id,
            node.view,
            graph.node(node.id).map_or("", |n| &n.type_id),
            bodies,
            &values,
            &curve,
        );
    }
}

/// A record dot, red while its Automation node is armed.
fn arm_button(painter: &Painter, rect: Rect, on: bool) {
    let color = if on { theme::RECORD } else { colors::TEXT_WEAK };
    if on {
        painter.rect_filled(rect.expand(1.0), rect.width() / 4.0, colors::NODE_OUTLINE);
    }
    painter.circle_filled(rect.center(), rect.width() * 0.32, color);
}

/// A pair of headphones, lit while its node is monitored.
fn monitor_button(painter: &Painter, rect: Rect, on: bool) {
    let color = if on {
        colors::ACTIVE
    } else {
        colors::TEXT_WEAK
    };
    if on {
        painter.rect_filled(rect.expand(1.0), rect.width() / 4.0, colors::NODE_OUTLINE);
    }
    let u = rect.width() / 14.0;
    let c = rect.center();
    let at = |x: f32, y: f32| c + Vec2::new(x * u, y * u);
    // The band over the head, then the two ear cups it ends in.
    let band: Vec<Pos2> = (0..=12)
        .map(|i| {
            let angle = std::f32::consts::PI + std::f32::consts::PI * i as f32 / 12.0;
            at(4.6 * angle.cos(), 0.9 + 4.6 * angle.sin())
        })
        .collect();
    painter.add(PathShape::line(
        band,
        Stroke::new((2.0 * u).max(1.0), color),
    ));
    for (left, right) in [(-6.2, -2.6), (2.6, 6.2)] {
        painter.rect_filled(
            Rect::from_min_max(at(left, -0.7), at(right, 4.7)),
            1.6 * u,
            color,
        );
    }
}

/// A node's CPU load as a percentage of the block time, coloured by how close
/// it is to making the audio thread late.
pub(crate) fn cpu_label(reading: CpuReading) -> (String, Color32) {
    let percent = reading.load * 100.0;
    let label = if percent < 0.05 {
        "0%".to_owned()
    } else if percent < 10.0 {
        format!("{percent:.1}%")
    } else {
        format!("{percent:.0}%")
    };
    let color = if reading.peak >= 0.9 || reading.load >= 0.5 {
        colors::CPU_HIGH
    } else if reading.load >= 0.15 {
        colors::CPU_MEDIUM
    } else {
        colors::TEXT_WEAK
    };
    (label, color)
}

/// How the slowest recent block did against its time, and how many ran over
/// (each is an underrun on a live stream).
pub(crate) fn block_label(stats: &BlockStats) -> (String, Color32) {
    let worst = (stats.worst * 100.0).round();
    if stats.panics > 0 {
        (
            format!("a node panicked in {} blocks", stats.panics),
            colors::CPU_HIGH,
        )
    } else if stats.misses > 0 {
        (
            format!(
                "{} block{} ran too slow (worst {worst}% of its time)\nFreeze a track, or raise the buffer in Settings > Audio",
                stats.misses,
                if stats.misses == 1 { "" } else { "s" }
            ),
            colors::CPU_HIGH,
        )
    } else {
        let color = if stats.worst >= 0.9 {
            colors::CPU_HIGH
        } else if stats.worst >= 0.5 {
            colors::CPU_MEDIUM
        } else {
            colors::TEXT_WEAK
        };
        (format!("worst block {worst}%"), color)
    }
}

/// What a frozen or offline node says about its render, and in what colour.
fn badge_label(badge: &Badge) -> (String, Color32) {
    match (&badge.state, badge.frozen) {
        (BadgeState::Ready, true) => ("Frozen".into(), colors::ACTIVE),
        (BadgeState::Ready, false) => ("Cached".into(), colors::TEXT_WEAK),
        (BadgeState::Rendering(fraction), frozen) => (
            format!(
                "{} {:.0}%",
                if frozen { "Freezing" } else { "Rendering" },
                fraction * 100.0
            ),
            colors::TEXT,
        ),
        (BadgeState::Waiting, _) => ("Waiting".into(), colors::TEXT_WEAK),
        (BadgeState::Failed(_), _) => ("Not rendered".into(), colors::PROBLEM),
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
        if port.kind == PortKind::Any {
            any_marks(
                painter,
                center,
                radius,
                Stroke::new(1.5, colors::AUDIO_SOCKET),
                Stroke::new(1.5, colors::EVENT_SOCKET),
            );
        }
        return;
    }
    match port.kind {
        // Takes any kind of wire: half the colour of each, until one is
        // wired and the port becomes that kind.
        PortKind::Any => {
            painter.circle(center, radius, colors::AUDIO_SOCKET, outline);
            let half: Vec<Pos2> = (0..=8)
                .map(|i| {
                    let angle =
                        -3.0 * std::f32::consts::FRAC_PI_4 + std::f32::consts::PI * i as f32 / 8.0;
                    center + Vec2::angled(angle) * radius
                })
                .collect();
            painter.add(PathShape::convex_polygon(
                half,
                colors::EVENT_SOCKET,
                Stroke::NONE,
            ));
            painter.circle_stroke(center, radius, outline);
        }
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
        // A square, for frames of spectrum.
        PortKind::Spectral => {
            let r = radius * 0.9;
            painter.rect(
                Rect::from_center_size(center, Vec2::splat(2.0 * r)),
                1.0,
                colors::SPECTRAL_SOCKET,
                outline,
                egui::StrokeKind::Middle,
            );
        }
        // A triangle, for a reference to a sample.
        PortKind::Sample => {
            let r = radius * 1.2;
            let points = vec![
                center + Vec2::new(-r, -r * 0.9),
                center + Vec2::new(r, 0.0),
                center + Vec2::new(-r, r * 0.9),
            ];
            painter.add(PathShape::convex_polygon(
                points,
                colors::SAMPLE_SOCKET,
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

/// The marks on a spare socket that takes any kind of wire: an arc of each
/// kind's colour over the grey ring.
fn any_marks(painter: &Painter, center: Pos2, radius: f32, audio: Stroke, event: Stroke) {
    let arc = |from: f32, to: f32| -> Vec<Pos2> {
        (0..=8)
            .map(|i| {
                let angle = from + (to - from) * i as f32 / 8.0;
                center + Vec2::angled(angle) * radius
            })
            .collect()
    };
    let quarter = std::f32::consts::FRAC_PI_2;
    painter.add(PathShape::line(arc(quarter, 3.0 * quarter), audio));
    painter.add(PathShape::line(arc(-quarter, quarter), event));
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
/// How much `wire` should shine, 0 to 1, and whether the signal in it has
/// clipped lately. An audio wire follows the level arriving at its end, a
/// modulation wire how lately its signal moved, an event wire how lately
/// events came out of its start.
fn wire_glow(
    bodies: &body::Bodies,
    graph: &noodle_core::Graph,
    wire: &super::layout::WireGeom,
) -> (f32, bool) {
    let display = bodies.display();
    let Endpoint { node, port } = &wire.connection.to;
    if display.audio_wires {
        let channels = match mixer_channel(graph, *node, port) {
            Some(channel) => bodies.input_meters_of(*node, channel),
            None => bodies.tap(*node, port),
        };
        if !channels.is_empty() {
            let (level, clipped) = body::Bodies::level_of(channels);
            return (level, clipped && display.clip_flash);
        }
    }
    let from = &wire.connection.from;
    if wire.event {
        return (bodies.event_glow(from.node, &from.port), false);
    }
    (bodies.param_glow(*node, port), false)
}

fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let mix = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * t).round() as u8;
    Color32::from_rgb(mix(a.r(), b.r()), mix(a.g(), b.g()), mix(a.b(), b.b()))
}

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

/// The thin level bar along the foot of a title bar, for a port that sits on
/// it. It lies below the title text (13 units, centred), never across it.
fn header_meter_area(header: Rect, side: Side, z: f32) -> Rect {
    let left = match side {
        Side::Input => header.left() + 24.0 * z,
        Side::Output => header.right() - 62.0 * z,
    };
    Rect::from_min_size(
        Pos2::new(left, header.bottom() - 4.0 * z),
        Vec2::new(48.0 * z, 3.0 * z),
    )
}

/// A port label's font size and its distance from the row's edge, in graph
/// units: shared by the label and the meter that must stay clear of it.
const PORT_LABEL_SIZE: f32 = 12.0;
const PORT_LABEL_INSET: f32 = 12.0;

/// Where the level bar beside an input's name goes in its `row`: up to 48
/// units wide at the right, shrunk so it never covers the name (a group's
/// port names are the user's, so they can be long). None when too little room
/// is left to read.
fn row_meter_area(painter: &Painter, row: Rect, z: f32, name: &str) -> Option<Rect> {
    let name_width = painter
        .layout_no_wrap(
            name.to_owned(),
            FontId::proportional(PORT_LABEL_SIZE * z),
            colors::TEXT,
        )
        .size()
        .x;
    let right = row.right() - 6.0 * z;
    let left = (right - 48.0 * z).max(row.left() + PORT_LABEL_INSET * z + name_width + 6.0 * z);
    (right - left >= 12.0 * z).then(|| {
        Rect::from_min_max(
            Pos2::new(left, row.center().y - 3.5 * z),
            Pos2::new(right, row.center().y + 3.5 * z),
        )
    })
}

/// The meter channel of a mixer node's input `key`: its position from 0.
fn mixer_channel(graph: &noodle_core::Graph, id: noodle_core::NodeId, key: &str) -> Option<usize> {
    let mixer = graph.node(id)?.type_id == noodle_core::spare::MIXER;
    let index = noodle_core::spare::mixer_input_index(key)?;
    mixer.then(|| index - 1)
}

/// Every parameter's default by key, shown on the node as a port or not: a
/// graph reads parameters the node doesn't list.
fn param_defaults(
    registry: &noodle_engine::Registry,
    node: Option<&noodle_core::Node>,
) -> Vec<(String, f32)> {
    let Some(node) = node else {
        return Vec::new();
    };
    let Some(layout) = registry
        .get(&node.type_id)
        .and_then(|t| t.layout(&node.config).ok())
    else {
        return Vec::new();
    };
    layout
        .inputs
        .iter()
        .filter_map(|input| match &input.kind {
            noodle_engine::InputKind::Param(info) => Some((input.key.to_string(), info.default)),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod default_tests {
    use super::*;

    #[test]
    fn a_graph_node_knows_the_defaults_of_parameters_it_does_not_show() {
        let nodes = crate::session::Nodes::all();
        let node = noodle_core::Node::new("noodle.effect.eq");
        let defaults = param_defaults(&nodes.registry, Some(&node));
        let get = |key: &str| defaults.iter().find(|(k, _)| k == key).map(|&(_, d)| d);
        assert_eq!(get("freq2"), Some(400.0));
        assert!((get("q1").unwrap() - 0.707).abs() < 1e-3);
    }

    #[test]
    fn a_header_meter_stays_below_the_title_text() {
        for z in [0.5, 1.0, 2.0] {
            let header = Rect::from_min_size(
                Pos2::ZERO,
                Vec2::new(200.0 * z, crate::editor::layout::HEADER_HEIGHT * z),
            );
            let text_bottom = header.center().y + 0.5 * 13.0 * z;
            for side in [Side::Input, Side::Output] {
                let area = header_meter_area(header, side, z);
                assert!(area.top() >= text_bottom, "{side:?} at {z}");
                assert!(header.contains_rect(area));
            }
        }
    }

    #[test]
    fn a_row_meter_never_covers_the_port_name() {
        let ctx = egui::Context::default();
        let mut out = ctx.run_ui(Default::default(), |ui| {
            let painter = ui.painter();
            for z in [0.5, 1.0, 2.0] {
                let row = Rect::from_min_size(Pos2::new(0.0, 0.0), Vec2::new(180.0 * z, 22.0 * z));
                for name in [
                    "L",
                    "Left",
                    "A rather long output name",
                    "x".repeat(80).as_str(),
                ] {
                    let width = painter
                        .layout_no_wrap(
                            name.to_owned(),
                            FontId::proportional(PORT_LABEL_SIZE * z),
                            colors::TEXT,
                        )
                        .size()
                        .x;
                    let label_right = row.left() + PORT_LABEL_INSET * z + width;
                    if let Some(area) = row_meter_area(painter, row, z, name) {
                        assert!(area.left() >= label_right, "{name} at {z}");
                        assert!(area.right() <= row.right());
                    }
                }
            }
        });
        out.textures_delta.clear();
    }
}

#[cfg(test)]
mod glow_tests {
    use noodle_core::{Command, Connection, Node, Project};
    use noodle_engine::{Level, Telemetry};

    use super::*;
    use crate::display::Display;

    fn wire(connection: Connection, event: bool) -> super::super::layout::WireGeom {
        super::super::layout::WireGeom {
            connection,
            lane: None,
            from: Pos2::ZERO,
            to: Pos2::new(100.0, 0.0),
            event,
            spectral: false,
            sample: false,
        }
    }

    /// An oscillator wired into a filter's audio input.
    fn audio_rig() -> (Project, Connection, Telemetry) {
        let mut project = Project::new();
        let (src, dst) = (project.new_node_id(), project.new_node_id());
        for (id, type_id) in [(src, "noodle.osc.sine"), (dst, "noodle.filter.svf")] {
            Command::AddNode {
                id,
                node: Node::new(type_id),
            }
            .apply(&mut project)
            .unwrap();
        }
        let connection = Connection {
            from: Endpoint::new(src, "out"),
            to: Endpoint::new(dst, "in"),
        };
        Command::Connect(connection.clone())
            .apply(&mut project)
            .unwrap();
        (project, connection, Telemetry::new())
    }

    fn registry() -> noodle_engine::Registry {
        crate::session::Nodes::all().registry
    }

    #[test]
    fn an_audio_wire_shines_with_its_level_and_flashes_when_it_clips() {
        let (project, connection, telemetry) = audio_rig();
        let writer = telemetry.open_input_meter(connection.to.node, "in", 1);
        let mut bodies = body::Bodies::default();
        let w = wire(connection.clone(), false);

        // Silence: dark.
        writer.write(0, Level::default());
        bodies.update(&telemetry, &project, &registry(), 1.0 / 60.0);
        assert_eq!(wire_glow(&bodies, project.graph(), &w), (0.0, false));

        // Quiet and loud signals shine differently.
        writer.write(
            0,
            Level {
                peak: 0.01,
                rms: 0.005,
            },
        );
        bodies.update(&telemetry, &project, &registry(), 1.0 / 60.0);
        let (quiet, _) = wire_glow(&bodies, project.graph(), &w);
        writer.write(
            0,
            Level {
                peak: 0.8,
                rms: 0.5,
            },
        );
        bodies.update(&telemetry, &project, &registry(), 1.0 / 60.0);
        let (loud, clipped) = wire_glow(&bodies, project.graph(), &w);
        assert!(0.0 < quiet && quiet < loud && loud <= 1.0, "{quiet} {loud}");
        assert!(!clipped);

        // Over full scale it flashes, unless that is switched off.
        writer.write(
            0,
            Level {
                peak: 2.0,
                rms: 1.0,
            },
        );
        bodies.update(&telemetry, &project, &registry(), 1.0 / 60.0);
        assert!(wire_glow(&bodies, project.graph(), &w).1);
        bodies.set_display(Display {
            clip_flash: false,
            ..Display::default()
        });
        writer.write(
            0,
            Level {
                peak: 2.0,
                rms: 1.0,
            },
        );
        bodies.update(&telemetry, &project, &registry(), 1.0 / 60.0);
        assert!(!wire_glow(&bodies, project.graph(), &w).1);

        // With the wire glow off, the wire stays plain.
        bodies.set_display(Display {
            audio_wires: false,
            ..Display::default()
        });
        bodies.update(&telemetry, &project, &registry(), 1.0 / 60.0);
        assert_eq!(wire_glow(&bodies, project.graph(), &w), (0.0, false));
    }

    #[test]
    fn an_event_wire_shines_while_events_leave_its_start() {
        let mut project = Project::new();
        let (src, dst) = (project.new_node_id(), project.new_node_id());
        for (id, type_id) in [(src, "noodle.input.button"), (dst, "noodle.event.key")] {
            Command::AddNode {
                id,
                node: Node::new(type_id),
            }
            .apply(&mut project)
            .unwrap();
        }
        let connection = Connection {
            from: Endpoint::new(src, "events"),
            to: Endpoint::new(dst, "events"),
        };
        let telemetry = Telemetry::new();
        let writer = telemetry.open_events(src, "events");
        let mut bodies = body::Bodies::default();
        bodies.set_sample_rate(48_000.0);
        let w = wire(connection, true);
        writer.write(0);
        bodies.update(&telemetry, &project, &registry(), 1.0 / 60.0);
        assert_eq!(wire_glow(&bodies, project.graph(), &w).0, 0.0);
        writer.write(3);
        bodies.update(&telemetry, &project, &registry(), 1.0 / 60.0);
        assert_eq!(wire_glow(&bodies, project.graph(), &w).0, 1.0);
    }

    #[test]
    fn a_faded_node_is_one_that_did_not_sound_and_is_not_selected() {
        let mut project = Project::new();
        let id = project.new_node_id();
        Command::AddNode {
            id,
            node: Node::new("noodle.osc.sine"),
        }
        .apply(&mut project)
        .unwrap();
        let telemetry = Telemetry::new();
        let writer = telemetry.open_state(id);
        let mut state = EditorState::default();
        state.bodies.set_sample_rate(48_000.0);
        let sounding = noodle_engine::NodeActivity {
            ran: true,
            silent: false,
            sounding: 1,
            voices: 1,
        };
        writer.write(sounding);
        state
            .bodies
            .update(&telemetry, &project, &registry(), 1.0 / 60.0);
        assert!(!fades(&state, id));
        writer.write(noodle_engine::NodeActivity {
            silent: true,
            ..sounding
        });
        state
            .bodies
            .update(&telemetry, &project, &registry(), 1.0 / 60.0);
        assert!(fades(&state, id));
        state.selected.insert(id);
        assert!(!fades(&state, id));
        state.selected.clear();
        // Not running at all (nothing listens) fades too.
        writer.write(noodle_engine::NodeActivity::default());
        state
            .bodies
            .update(&telemetry, &project, &registry(), 1.0 / 60.0);
        assert!(fades(&state, id));
        state.bodies.set_display(Display {
            silent_dim: false,
            ..Display::default()
        });
        assert!(!fades(&state, id));
    }
}
