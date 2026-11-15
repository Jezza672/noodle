//! A Morph node's panel: the XY pad (for four presets), the rows with a slider
//! for each corner, Set from current and Randomise for a corner, and the
//! clean-up of rows whose output nothing listens to.
//!
//! Everything here edits the node's `targets` config (one undo step per
//! gesture) or its `position` and `y` parameters. Nothing is wired from here:
//! the wires were made by "Morph between presets...", and deleting one takes
//! its parameter out of the morph.

use egui::{Color32, RichText, Sense, Stroke, Ui, pos2, vec2};
use noodle_core::group::GROUP_INPUT;
use noodle_core::{Command, Endpoint, Graph, Node, NodeId, Value};
use noodle_nodes::{
    MORPH_POSITION, MORPH_TARGETS, MORPH_Y, MorphRow, morph_output_key, morph_rows_of,
    morph_rows_to_text, randomise_corner, set_corner_from_current,
};

use crate::properties::LiveParams;
use crate::session::{Edit, Session};
use crate::theme;
use crate::widgets::Live;

const CORNERS: [&str; 4] = ["A", "B", "C", "D"];

/// What drives the Morph's `key` axis from outside, if anything: a wire from
/// a node, or from a group input the group's own wire feeds. `None` means the
/// panel can set it (an unwired group input leaves the value set here).
pub fn driven_by(graph: &Graph, morph: NodeId, key: &str) -> Option<String> {
    let source = graph.source(&Endpoint::new(morph, key))?;
    let node = graph.node(source.node)?;
    if node.type_id != GROUP_INPUT {
        return Some("a wire".to_owned());
    }
    let group = node.parent?;
    let port = graph
        .group_ports(group)
        .inputs
        .into_iter()
        .find(|p| p.node == source.node && p.key() == source.port)?;
    graph
        .source(&Endpoint::new(group, port.name))
        .map(|_| "a wire into the group".to_owned())
}

/// The rows whose output has no wire, as indices.
pub fn unconnected(graph: &Graph, morph: NodeId, rows: &[MorphRow]) -> Vec<usize> {
    (0..rows.len())
        .filter(|&i| {
            let from = Endpoint::new(morph, morph_output_key(i));
            !graph.connections().any(|c| c.from == from)
        })
        .collect()
}

/// Removes the rows with no wire. Outputs are named by position, so the wires
/// of the rows after a removed one move to their new outputs, all in one
/// command.
pub fn cleanup(graph: &Graph, morph: NodeId, rows: &[MorphRow]) -> Option<Command> {
    let gone = unconnected(graph, morph, rows);
    if gone.is_empty() {
        return None;
    }
    let mut kept = Vec::new();
    let mut disconnect = Vec::new();
    let mut connect = Vec::new();
    for (i, row) in rows.iter().enumerate() {
        if gone.contains(&i) {
            continue;
        }
        let new = kept.len();
        kept.push(row.clone());
        if new == i {
            continue;
        }
        let from = Endpoint::new(morph, morph_output_key(i));
        for c in graph.connections().filter(|c| c.from == from) {
            disconnect.push(Command::Disconnect {
                input: c.to.clone(),
            });
            connect.push(noodle_core::spare::wire(
                Endpoint::new(morph, morph_output_key(new)),
                c.to.clone(),
            ));
        }
    }
    let mut commands = disconnect;
    commands.push(set_rows(morph, &kept));
    commands.extend(connect);
    Some(Command::Batch(commands))
}

fn set_rows(morph: NodeId, rows: &[MorphRow]) -> Command {
    Command::SetConfig {
        node: morph,
        key: MORPH_TARGETS.to_owned(),
        value: Some(Value::Text(morph_rows_to_text(rows))),
    }
}

fn axis(graph: &Graph, id: NodeId, node: &Node, key: &str, live: LiveParams<'_>) -> f32 {
    let value = if driven_by(graph, id, key).is_some() {
        live(id, key).map(|l: Live| l.value)
    } else {
        None
    };
    value
        .or_else(|| node.params.get(key).copied())
        .unwrap_or(0.0)
        .clamp(0.0, 1.0)
}

pub fn show(
    ui: &mut Ui,
    session: &Session,
    id: NodeId,
    node: &Node,
    live: LiveParams<'_>,
    edits: &mut Vec<Edit>,
) {
    let graph = session.project().graph();
    let mut rows = morph_rows_of(&node.config);
    ui.separator();
    ui.label(RichText::new("Morph").strong());
    if rows.is_empty() {
        ui.weak("No rows. Use \"Morph between presets...\" on a group.");
        return;
    }
    let xy = rows.iter().any(MorphRow::uses_y);
    let x = axis(graph, id, node, MORPH_POSITION, live);
    let y = axis(graph, id, node, MORPH_Y, live);
    if xy {
        pad(ui, graph, id, x, y, edits);
    }

    let corners = if xy { 4 } else { 2 };
    let seed_key = ui.id().with(("morph seed", id));
    let mut changed = false;
    for (corner, name) in CORNERS.iter().enumerate().take(corners) {
        ui.horizontal(|ui| {
            ui.label(RichText::new(*name).strong());
            if ui
                .small_button("Set from current")
                .on_hover_text("Make this corner what the morph sounds like right now")
                .clicked()
            {
                set_corner_from_current(&mut rows, corner, x, y);
                changed = true;
            }
            if ui
                .small_button("Randomise")
                .on_hover_text("Fill this corner with random values within each row's range")
                .clicked()
            {
                let seed: u64 = ui.data_mut(|d| {
                    let next = d
                        .get_temp::<u64>(seed_key)
                        .unwrap_or(id.0.wrapping_mul(7919).wrapping_add(1))
                        .wrapping_add(1);
                    d.insert_temp(seed_key, next);
                    next
                });
                randomise_corner(&mut rows, corner, seed);
                changed = true;
            }
        });
    }
    if changed {
        edits.push(Edit::Apply(set_rows(id, &rows)));
        return;
    }

    let mut drag_edit: Option<(Command, bool, bool)> = None;
    let original = rows.clone();
    for (r, row) in rows.iter_mut().enumerate() {
        ui.label(RichText::new(&row.name).small());
        ui.horizontal(|ui| {
            let width = (ui.available_width() / corners as f32 - 6.0).max(30.0);
            for (corner, name) in CORNERS.iter().enumerate().take(corners) {
                let mut t = row.corner(corner);
                let response = ui.add_sized(
                    [width, 16.0],
                    egui::Slider::new(&mut t, 0.0..=1.0)
                        .show_value(false)
                        .smallest_positive(0.0),
                );
                let shown = row.at_position(t);
                response
                    .clone()
                    .on_hover_text(format!("{name}: {shown:.3}"));
                if response.changed() {
                    row.set_corner(corner, t);
                    let mut edited = original.clone();
                    edited[r] = row.clone();
                    drag_edit = Some((
                        set_rows(id, &edited),
                        response.dragged(),
                        response.drag_stopped(),
                    ));
                } else if response.drag_stopped() {
                    drag_edit = Some((set_rows(id, &original), false, true));
                }
            }
        });
    }
    if let Some((command, dragging, stopped)) = drag_edit {
        let apply = rows != original;
        if apply {
            edits.push(if dragging {
                Edit::Drag(command)
            } else {
                Edit::Apply(command)
            });
        }
        if stopped {
            edits.push(Edit::EndDrag);
        }
    }

    let loose = unconnected(graph, id, &original);
    if !loose.is_empty()
        && ui
            .button(format!("Remove {} unconnected rows", loose.len()))
            .on_hover_text("Rows whose parameter was taken out of the morph by deleting its wire")
            .clicked()
        && let Some(command) = cleanup(graph, id, &original)
    {
        edits.push(Edit::Apply(command));
    }
}

/// The XY pad: A top left, B top right, C bottom left, D bottom right. A drag
/// sets `position` and `y`; when something drives them it only shows them.
fn pad(ui: &mut Ui, graph: &Graph, id: NodeId, x: f32, y: f32, edits: &mut Vec<Edit>) {
    let side = ui.available_width().clamp(80.0, 200.0);
    let (response, painter) = ui.allocate_painter(vec2(side, side), Sense::click_and_drag());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Other, true, "Morph pad"));
    let rect = response.rect;
    let free =
        driven_by(graph, id, MORPH_POSITION).is_none() && driven_by(graph, id, MORPH_Y).is_none();
    let visuals = ui.visuals();
    painter.rect_filled(rect, 4.0, visuals.extreme_bg_color);
    painter.rect_stroke(
        rect,
        4.0,
        Stroke::new(1.0, visuals.widgets.noninteractive.bg_stroke.color),
        egui::StrokeKind::Inside,
    );
    for (name, at) in [
        ("A", rect.left_top() + vec2(8.0, 8.0)),
        ("B", rect.right_top() + vec2(-8.0, 8.0)),
        ("C", rect.left_bottom() + vec2(8.0, -8.0)),
        ("D", rect.right_bottom() + vec2(-8.0, -8.0)),
    ] {
        painter.text(
            at,
            egui::Align2::CENTER_CENTER,
            name,
            egui::FontId::proportional(11.0),
            visuals.weak_text_color(),
        );
    }
    let dot = pos2(
        rect.left() + x * rect.width(),
        rect.top() + y * rect.height(),
    );
    painter.circle_filled(dot, 6.0, if free { theme::ACCENT } else { Color32::GRAY });
    if free
        && (response.dragged() || response.clicked())
        && let Some(p) = response.interact_pointer_pos()
    {
        let nx = ((p.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
        let ny = ((p.y - rect.top()) / rect.height()).clamp(0.0, 1.0);
        for (key, value) in [(MORPH_POSITION, nx), (MORPH_Y, ny)] {
            let command = Command::SetParam {
                node: id,
                key: key.to_owned(),
                value: Some(value),
            };
            edits.push(if response.dragged() {
                Edit::Drag(command)
            } else {
                Edit::Apply(command)
            });
        }
    }
    if response.drag_stopped() {
        edits.push(Edit::EndDrag);
    }
    response.on_hover_text(if free {
        "Drag to blend the four presets"
    } else {
        "Driven from outside, so this only shows where it is"
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::Nodes;
    use egui::vec2;
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;
    use noodle_core::spare;
    use noodle_nodes::{MORPH_ID, morph_config};

    fn rows(n: usize) -> Vec<MorphRow> {
        (0..n)
            .map(|i| {
                MorphRow::two(
                    format!("Row {i}"),
                    0.0,
                    10.0,
                    false,
                    i as f32 / 10.0,
                    1.0,
                    false,
                )
            })
            .collect()
    }

    fn pad_rows(n: usize) -> Vec<MorphRow> {
        let mut r = rows(n);
        for row in &mut r {
            row.c = 1.0;
            row.d = 0.0;
        }
        r
    }

    struct State {
        session: Session,
    }

    /// A Morph node (ID 1) with `rows`, each output wired to a Gain's `gain`
    /// (IDs 2 and up) except those in `loose`.
    fn session(rows: &[MorphRow], loose: &[usize]) -> Session {
        let mut session = Session::new(Nodes::all());
        let mut edits = vec![Edit::Apply(Command::AddNode {
            id: NodeId(1),
            node: Node::new(MORPH_ID).with_config(morph_config(rows)),
        })];
        for i in 0..rows.len() {
            edits.push(Edit::Apply(Command::AddNode {
                id: NodeId(i as u64 + 2),
                node: Node::new("noodle.util.gain"),
            }));
        }
        session.edit(edits);
        for i in (0..rows.len()).filter(|i| !loose.contains(i)) {
            session.edit([Edit::Apply(spare::wire(
                Endpoint::new(NodeId(1), morph_output_key(i)),
                Endpoint::new(NodeId(i as u64 + 2), "gain"),
            ))]);
        }
        session
    }

    fn harness(session: Session) -> Harness<'static, State> {
        crate::testkit::harness(
            State { session },
            vec2(320.0, 600.0),
            |ui, state: &mut State| {
                let node = state
                    .session
                    .project()
                    .graph()
                    .node(NodeId(1))
                    .unwrap()
                    .clone();
                let mut edits = Vec::new();
                show(
                    ui,
                    &state.session,
                    NodeId(1),
                    &node,
                    &|_, _| None,
                    &mut edits,
                );
                state.session.edit(edits);
            },
        )
    }

    fn rows_now(h: &Harness<'_, State>) -> Vec<MorphRow> {
        let graph = h.state().session.project().graph();
        morph_rows_of(&graph.node(NodeId(1)).unwrap().config)
    }

    #[test]
    fn cleanup_drops_loose_rows_and_moves_the_later_wires() {
        let all = rows(4);
        let session = session(&all, &[1]);
        let graph = session.project().graph();
        assert_eq!(unconnected(graph, NodeId(1), &all), vec![1]);
        let command = cleanup(graph, NodeId(1), &all).unwrap();
        let mut after = session;
        after.edit([Edit::Apply(command)]);
        let graph = after.project().graph();
        let kept = morph_rows_of(&graph.node(NodeId(1)).unwrap().config);
        let names: Vec<_> = kept.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["Row 0", "Row 2", "Row 3"]);
        // Row 2 is now output 2, and still drives its own Gain.
        for (out, gain) in [(0, 2), (1, 4), (2, 5)] {
            let to = Endpoint::new(NodeId(gain), "gain");
            assert_eq!(
                graph.source(&to).unwrap(),
                &Endpoint::new(NodeId(1), morph_output_key(out)),
                "gain {gain}"
            );
        }
        assert!(graph.source(&Endpoint::new(NodeId(3), "gain")).is_none());
        // One undo step.
        after.undo();
        assert_eq!(
            morph_rows_of(&after.project().graph().node(NodeId(1)).unwrap().config).len(),
            4
        );
        assert!(cleanup(after.project().graph(), NodeId(1), &all).is_some());
    }

    #[test]
    fn nothing_to_clean_when_every_row_is_wired() {
        let all = rows(3);
        let session = session(&all, &[]);
        assert!(cleanup(session.project().graph(), NodeId(1), &all).is_none());
    }

    #[test]
    fn dragging_the_pad_sets_position_and_y_in_one_undo_step() {
        let mut h = harness(session(&pad_rows(2), &[]));
        h.run();
        let rect = h.get_by_label("Morph pad").rect();
        let from = rect.center();
        let to = egui::pos2(
            rect.left() + rect.width() * 0.75,
            rect.top() + rect.height() * 0.25,
        );
        h.hover_at(from);
        h.run();
        h.drag_at(from);
        h.run();
        h.hover_at(to);
        h.run();
        h.drop_at(to);
        h.run();
        let params = h
            .state()
            .session
            .project()
            .graph()
            .node(NodeId(1))
            .unwrap()
            .params
            .clone();
        assert!((params["position"] - 0.75).abs() < 0.03, "{params:?}");
        assert!((params["y"] - 0.25).abs() < 0.03, "{params:?}");
        h.state_mut().session.undo();
        let params = h
            .state()
            .session
            .project()
            .graph()
            .node(NodeId(1))
            .unwrap()
            .params
            .clone();
        assert!(
            !params.contains_key("position") && !params.contains_key("y"),
            "{params:?}"
        );
    }

    #[test]
    fn randomise_and_set_from_current_edit_one_corner() {
        let mut h = harness(session(&pad_rows(3), &[]));
        h.run();
        let before = rows_now(&h);
        h.get_all_by_label("Randomise").nth(1).unwrap().click();
        h.run();
        let after = rows_now(&h);
        assert!(
            after
                .iter()
                .zip(&before)
                .all(|(a, b)| a.a == b.a && a.c == b.c && a.d == b.d)
        );
        assert!(after.iter().zip(&before).any(|(a, b)| a.b != b.b));
        assert!(after.iter().all(|r| (0.0..=1.0).contains(&r.b)));
        // Position 0, y 0: set D from current captures A.
        h.get_all_by_label("Set from current")
            .nth(3)
            .unwrap()
            .click();
        h.run();
        let after = rows_now(&h);
        assert!(after.iter().all(|r| r.d == r.a));
        h.state_mut().session.undo();
        assert!(rows_now(&h).iter().zip(&before).all(|(a, b)| a.d == b.d));
    }
}
