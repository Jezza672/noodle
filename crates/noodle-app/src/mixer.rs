//! The mixer: one strip per track, showing and setting that track's gain,
//! mute and solo.
//!
//! The mixer owns no state. A track is a group node, and its controls are
//! parameters on the group's boundary nodes (see `noodle_core::group`), so a
//! strip reads them from the project and a fader move is a `SetParam` on the
//! boundary node. Strips show the top-level groups, in the order they were
//! made.

use egui::{Align, Color32, Layout, RichText, Slider, Ui, Vec2};
use noodle_core::group::{Controls, GAIN, GROUP, GROUP_INPUT, GROUP_OUTPUT, MUTE, SOLO};
use noodle_core::{Command, Endpoint, Graph, NodeId, Project, Value, spare};

use crate::editor::MeterChannel;
use crate::session::Edit;
use crate::timeline::header;

/// The config key a group's display name is kept under.
pub const NAME: &str = "name";

const FADER_RANGE: std::ops::RangeInclusive<f32> = -60.0..=24.0;
const STRIP_WIDTH: f32 = 84.0;
const FADER_HEIGHT: f32 = 170.0;

/// What a strip shows.
#[derive(Clone, Debug, PartialEq)]
pub struct Strip {
    pub group: NodeId,
    pub name: String,
    pub controls: Controls,
    /// The boundary node the controls are set on. `None` for a group with
    /// no input or output node, which has nothing to set.
    pub node: Option<NodeId>,
    /// Silent because another track is soloed.
    pub muted_by_solo: bool,
    /// An automation lane drives the gain, so the fader would do nothing.
    pub gain_automated: bool,
    /// The same for mute.
    pub mute_automated: bool,
    /// Which input of the mixer node this strip is, from 0, in a mixer view.
    pub channel: Option<usize>,
}

/// Every mixer node in the graph, in ID order: what the mixer can be shown
/// over.
pub fn mixers(project: &Project) -> Vec<NodeId> {
    project
        .graph()
        .nodes()
        .filter(|(_, node)| node.type_id == spare::MIXER)
        .map(|(id, _)| id)
        .collect()
}

/// The strips for a mixer node, one per wired input in input order. An input
/// fed by a track's group shows that track's controls; anything else has a
/// strip with nothing to set.
pub fn mixer_strips(project: &Project, mixer: NodeId) -> Vec<Strip> {
    let graph = project.graph();
    let Some(node) = graph.node(mixer) else {
        return Vec::new();
    };
    let tracks = strips(project);
    (1..=spare::mixer_inputs(node))
        .filter_map(|i| {
            let source = graph.source(&Endpoint::new(mixer, spare::mixer_input_key(i as usize)))?;
            let channel = Some(i as usize - 1);
            Some(
                match tracks.iter().find(|strip| strip.group == source.node) {
                    Some(strip) => Strip {
                        channel,
                        ..strip.clone()
                    },
                    None => Strip {
                        channel,
                        group: source.node,
                        name: format!("In {i}"),
                        controls: Controls::default(),
                        node: None,
                        muted_by_solo: false,
                        gain_automated: false,
                        mute_automated: false,
                    },
                },
            )
        })
        .collect()
}

/// The strips for the top-level groups of a graph, in track order.
pub fn strips(project: &Project) -> Vec<Strip> {
    let graph = project.graph();
    let muted_by_solo = graph.solo_muted();
    let mut ids: Vec<NodeId> = graph
        .children(None)
        .filter(|(_, node)| node.type_id == GROUP)
        .map(|(id, _)| id)
        .collect();
    project.sort_tracks(&mut ids);
    ids.into_iter()
        .filter_map(|id| Some((id, graph.node(id)?)))
        .map(|(id, node)| Strip {
            group: id,
            name: match node.config.get(NAME) {
                Some(Value::Text(name)) if !name.is_empty() => name.clone(),
                _ => format!("Group {}", id.0),
            },
            controls: graph.group_controls(id),
            node: graph.control_node(id),
            muted_by_solo: muted_by_solo.contains(&id),
            gain_automated: driven(project, graph.control_node(id), GAIN),
            mute_automated: driven(project, graph.control_node(id), MUTE),
            channel: None,
        })
        .collect()
}

/// Whether a lane drives `key` on `node`, which then overrides what the
/// strip sets.
fn driven(project: &Project, node: Option<NodeId>, key: &str) -> bool {
    node.is_some_and(|node| project.lane_for(&Endpoint::new(node, key)).is_some())
}

/// A change to a group's gain, as one step of a drag or a lone edit.
fn set(node: NodeId, key: &str, value: Option<f32>) -> Command {
    Command::SetParam {
        node,
        key: key.into(),
        value,
    }
}

/// Turns solo on or off for a group. Off clears it on every boundary node,
/// since any of them can hold it.
///
/// Controls are reset by writing their default, never by removing the
/// parameter: a set control keeps its stage in the compiled graph, and
/// removing it would take the stage out and fade the whole output.
pub fn solo_edit(graph: &Graph, strip: &Strip, on: bool) -> Option<Edit> {
    let node = strip.node?;
    if on {
        return Some(Edit::Apply(set(node, SOLO, Some(1.0))));
    }
    let commands: Vec<_> = graph
        .children(Some(strip.group))
        .filter(|(_, n)| {
            matches!(n.type_id.as_str(), GROUP_INPUT | GROUP_OUTPUT) && n.controls().solo
        })
        .map(|(id, _)| set(id, SOLO, Some(0.0)))
        .collect();
    Some(Edit::Apply(Command::Batch(commands)))
}

/// Draws the mixer and returns the edits made in it. `view` is the mixer
/// node shown, or `None` for one strip per track; a node that has gone falls
/// back to that.
pub fn show(
    ui: &mut Ui,
    project: &Project,
    view: &mut Option<NodeId>,
    levels: &dyn Fn(NodeId, usize) -> Option<MeterChannel>,
) -> Vec<Edit> {
    let graph = project.graph();
    let mixers = mixers(project);
    if view.is_some_and(|m| !mixers.contains(&m)) {
        *view = None;
    }
    if !mixers.is_empty() {
        ui.horizontal(|ui| {
            ui.label("Mixer");
            let label =
                |m: Option<NodeId>| m.map_or("All tracks".to_string(), |m| format!("Mix {}", m.0));
            egui::ComboBox::from_id_salt("mixer view")
                .selected_text(label(*view))
                .show_ui(ui, |ui| {
                    ui.selectable_value(view, None, label(None));
                    for &m in &mixers {
                        ui.selectable_value(view, Some(m), label(Some(m)));
                    }
                });
        });
    }
    let strips = match *view {
        Some(mixer) => mixer_strips(project, mixer),
        None => strips(project),
    };
    if strips.is_empty() {
        ui.weak(if view.is_some() {
            "Nothing is wired into this mixer."
        } else {
            "No tracks yet."
        });
        return Vec::new();
    }
    let mut edits = Vec::new();
    egui::ScrollArea::horizontal().show(ui, |ui| {
        ui.horizontal_top(|ui| {
            for (index, strip) in strips.iter().enumerate() {
                ui.push_id((strip.group, index), |ui| {
                    let level = (*view)
                        .zip(strip.channel)
                        .and_then(|(mixer, channel)| levels(mixer, channel));
                    strip_ui(ui, graph, strip, level, &mut edits);
                });
                ui.separator();
            }
        });
    });
    edits
}

fn strip_ui(
    ui: &mut Ui,
    graph: &Graph,
    strip: &Strip,
    level: Option<MeterChannel>,
    edits: &mut Vec<Edit>,
) {
    ui.allocate_ui_with_layout(
        Vec2::new(STRIP_WIDTH, 0.0),
        Layout::top_down(Align::Center),
        |ui| {
            ui.set_width(STRIP_WIDTH);
            ui.label(RichText::new(&strip.name).strong());
            // The input's level, in a mixer view.
            if strip.channel.is_some() {
                let (area, _) = ui
                    .allocate_exact_size(Vec2::new(STRIP_WIDTH - 12.0, 8.0), egui::Sense::hover());
                crate::editor::draw_level(ui.painter(), area, &level.unwrap_or_default());
            }
            // A silent strip's fader is greyed, but its buttons stay live so
            // it can be unmuted or unsoloed.
            let silent = strip.controls.mute || strip.muted_by_solo;
            // A lane overrides the parameter it drives, so those controls
            // are greyed out with the reason, as on the track header.
            let faded = ui.add_enabled_ui(
                strip.node.is_some() && !silent && !strip.gain_automated,
                |ui| fader(ui, strip, edits),
            );
            if strip.gain_automated {
                faded.response.on_disabled_hover_text(header::AUTOMATED);
            }
            ui.add_enabled_ui(strip.node.is_some(), |ui| {
                ui.horizontal(|ui| {
                    let mute = ui
                        .add_enabled_ui(!strip.mute_automated, |ui| {
                            toggle(ui, "M", strip.controls.mute, crate::theme::MUTE)
                        })
                        .inner;
                    let tip = if strip.mute_automated {
                        header::AUTOMATED
                    } else {
                        "Mute"
                    };
                    if mute
                        .on_hover_text(tip)
                        .on_disabled_hover_text(tip)
                        .clicked()
                        && let Some(node) = strip.node
                    {
                        let value = Some(f32::from(u8::from(!strip.controls.mute)));
                        edits.push(Edit::Apply(set(node, MUTE, value)));
                    }
                    let solo = toggle(ui, "S", strip.controls.solo, crate::theme::SOLO);
                    if solo.on_hover_text("Solo: mutes the other tracks").clicked() {
                        edits.extend(solo_edit(graph, strip, !strip.controls.solo));
                    }
                });
            });
            if strip.muted_by_solo && !strip.controls.mute {
                ui.weak("muted by solo");
            }
        },
    );
}

/// A small button that stays lit while `on`.
fn toggle(ui: &mut Ui, label: &str, on: bool, lit: Color32) -> egui::Response {
    let mut text = RichText::new(label).strong();
    if on {
        text = text.color(Color32::BLACK);
    }
    let button = egui::Button::new(text)
        .selected(on)
        .fill(if on {
            lit
        } else {
            ui.visuals().widgets.inactive.bg_fill
        })
        .min_size(Vec2::splat(28.0));
    ui.add(button)
}

fn fader(ui: &mut Ui, strip: &Strip, edits: &mut Vec<Edit>) {
    let Some(node) = strip.node else { return };
    let mut db = strip.controls.gain_db;
    let response = ui.add(
        Slider::new(&mut db, FADER_RANGE)
            .vertical()
            .show_value(false)
            .handle_shape(egui::style::HandleShape::Rect { aspect_ratio: 0.5 })
            .trailing_fill(false),
    );
    ui.set_min_height(FADER_HEIGHT);
    if response.dragged() {
        edits.push(Edit::Drag(set(node, GAIN, Some(db))));
    } else if response.drag_stopped() {
        edits.push(Edit::EndDrag);
    } else if response.changed() {
        edits.push(Edit::Apply(set(node, GAIN, Some(db))));
    }
    // The reading doubles as the reset: a click puts the gain back to 0 dB.
    let reading = ui
        .small_button(format!("{:+.1} dB", strip.controls.gain_db))
        .on_hover_text("Click to reset to 0 dB");
    if reading.clicked() {
        edits.push(Edit::Apply(set(node, GAIN, Some(0.0))));
    }
}

#[cfg(test)]
mod tests {
    use egui::accesskit::Role;
    use egui::vec2;
    use egui_kittest::Harness;
    use egui_kittest::kittest::{NodeT, Queryable};
    use noodle_core::group::{GROUP, PORT_NAME};
    use noodle_core::{Config, Node};

    use super::*;
    use crate::session::Session;

    /// Two tracks, each a group with an input and an output node. IDs: the
    /// first track is 1 (nodes 2 and 3), the second is 4 (nodes 5 and 6).
    fn two_tracks() -> Session {
        let mut session = Session::new(crate::session::Nodes::all());
        let mut edits = Vec::new();
        for (i, name) in ["Drums", "Bass"].into_iter().enumerate() {
            let group = NodeId(1 + 3 * i as u64);
            edits.push(Edit::Apply(Command::AddNode {
                id: group,
                node: Node::new(GROUP)
                    .with_config(Config::new().with(NAME, Value::Text(name.into()))),
            }));
            for (offset, kind) in [(1, GROUP_INPUT), (2, GROUP_OUTPUT)] {
                let node = Node::new(kind)
                    .with_config(Config::new().with(PORT_NAME, Value::Text("p".into())))
                    .in_group(group);
                edits.push(Edit::Apply(Command::AddNode {
                    id: NodeId(group.0 + offset),
                    node,
                }));
            }
        }
        session.edit(edits);
        session
    }

    fn harness(session: Session) -> Harness<'static, Session> {
        let mut harness = Harness::new_ui_state(
            |ui, session: &mut Session| {
                let edits = show(ui, session.project(), &mut None, &|_, _| None);
                session.edit(edits);
            },
            session,
        );
        harness.set_size(vec2(600.0, 600.0));
        harness.run();
        harness
    }

    fn param(harness: &Harness<'_, Session>, node: u64, key: &str) -> Option<f32> {
        let graph = harness.state().project().graph();
        graph.node(NodeId(node))?.params.get(key).copied()
    }

    #[test]
    fn a_strip_per_top_level_group_in_order() {
        let mut session = two_tracks();
        // A nested group and a plain node don't get strips.
        session.edit([
            Edit::Apply(Command::AddNode {
                id: NodeId(10),
                node: Node::new(GROUP).in_group(NodeId(1)),
            }),
            Edit::Apply(Command::AddNode {
                id: NodeId(11),
                node: Node::new("noodle.util.gain"),
            }),
            Edit::Apply(Command::SetParam {
                node: NodeId(6),
                key: GAIN.into(),
                value: Some(-6.0),
            }),
        ]);
        let strips = strips(session.project());
        let summary: Vec<_> = strips
            .iter()
            .map(|s| (s.name.as_str(), s.node, s.controls.gain_db))
            .collect();
        assert_eq!(
            summary,
            [
                ("Drums", Some(NodeId(3)), 0.0),
                ("Bass", Some(NodeId(6)), -6.0)
            ]
        );
    }

    #[test]
    fn an_unnamed_group_is_named_by_its_id_and_one_without_boundary_nodes_has_no_controls() {
        let mut session = Session::new(crate::session::Nodes::all());
        session.edit([Edit::Apply(Command::AddNode {
            id: NodeId(7),
            node: Node::new(GROUP),
        })]);
        let strips = strips(session.project());
        assert_eq!((strips[0].name.as_str(), strips[0].node), ("Group 7", None));
        // Nothing to set, so nothing is offered.
        assert_eq!(solo_edit(session.project().graph(), &strips[0], true), None);
    }

    #[test]
    fn showing_the_mixer_names_the_tracks() {
        let harness = harness(two_tracks());
        harness.get_by_label("Drums");
        harness.get_by_label("Bass");
    }

    #[test]
    fn no_tracks_says_so() {
        let harness = harness(Session::new(crate::session::Nodes::all()));
        harness.get_by_label("No tracks yet.");
    }

    #[test]
    fn mute_toggles_the_output_nodes_mute_parameter() {
        let mut harness = harness(two_tracks());
        harness.get_all_by_label("M").next().unwrap().click();
        harness.run();
        assert_eq!(param(&harness, 3, MUTE), Some(1.0));
        assert_eq!(param(&harness, 6, MUTE), None);
        // Again: back to the default, still written so the stage stays.
        harness.get_all_by_label("M").next().unwrap().click();
        harness.run();
        assert_eq!(param(&harness, 3, MUTE), Some(0.0));
    }

    #[test]
    fn solo_mutes_the_other_track_and_says_so() {
        let mut harness = harness(two_tracks());
        harness.get_all_by_label("S").next().unwrap().click();
        harness.run();
        assert_eq!(param(&harness, 3, SOLO), Some(1.0));
        let graph = harness.state().project().graph();
        assert_eq!(graph.solo_muted(), [NodeId(4)].into());
        harness.get_by_label("muted by solo");
        // The muted-by-solo track's own mute button is untouched and live.
        assert_eq!(param(&harness, 6, MUTE), None);

        harness.get_all_by_label("S").next().unwrap().click();
        harness.run();
        assert_eq!(param(&harness, 3, SOLO), Some(0.0));
        assert!(harness.query_by_label("muted by solo").is_none());
    }

    #[test]
    fn unsoloing_clears_solo_wherever_it_was_set() {
        let mut session = two_tracks();
        // Solo on the input node, as a hand-edited file might have it.
        session.edit([Edit::Apply(Command::SetParam {
            node: NodeId(2),
            key: SOLO.into(),
            value: Some(1.0),
        })]);
        let mut harness = harness(session);
        harness.get_by_label("muted by solo");
        harness.get_all_by_label("S").next().unwrap().click();
        harness.run();
        assert_eq!(param(&harness, 2, SOLO), Some(0.0));
        assert!(harness.query_by_label("muted by solo").is_none());
    }

    #[test]
    fn a_fader_reads_the_gain_and_its_reading_resets_it() {
        let mut session = two_tracks();
        session.edit([Edit::Apply(Command::SetParam {
            node: NodeId(3),
            key: GAIN.into(),
            value: Some(-12.0),
        })]);
        let mut harness = harness(session);
        let fader = harness.get_all_by_role(Role::Slider).next().unwrap();
        assert_eq!(fader.accesskit_node().numeric_value(), Some(-12.0));
        harness.get_by_label("-12.0 dB").click();
        harness.run();
        assert_eq!(param(&harness, 3, GAIN), Some(0.0));
        harness.get_all_by_label("+0.0 dB").next().unwrap();
    }

    #[test]
    fn dragging_the_fader_sets_the_gain_as_one_undo_step() {
        let mut harness = harness(two_tracks());
        let fader = harness.get_all_by_role(Role::Slider).next().unwrap();
        let from = fader.rect().center();
        let to = from + vec2(0.0, -40.0);
        harness.event(egui::Event::PointerMoved(from));
        let button = |pressed, pos| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        harness.event(button(true, from));
        harness.step();
        for i in 1..=4 {
            harness.event(egui::Event::PointerMoved(
                from + (to - from) * (i as f32 / 4.0),
            ));
            harness.step();
        }
        harness.event(button(false, to));
        harness.run();
        let gain = param(&harness, 3, GAIN).expect("the fader moved");
        assert!(gain > 0.0, "dragged up: {gain}");
        // The whole drag is one step.
        let state = harness.state_mut();
        state.undo();
        assert_eq!(
            state
                .project()
                .graph()
                .node(NodeId(3))
                .unwrap()
                .params
                .get(GAIN),
            None
        );
    }

    #[test]
    fn a_lane_on_gain_or_mute_greys_that_control_on_the_strip() {
        use noodle_core::{AutomationLane, AutomationPoint, Curve, LaneId};
        let mut session = two_tracks();
        let point = AutomationPoint {
            tick: noodle_core::Tick(0),
            value: -6.0,
            curve: Curve::Hold,
        };
        let lane = |key: &str, id: u64| {
            Edit::Apply(Command::AddLane {
                id: LaneId(id),
                lane: AutomationLane::new(Endpoint::new(NodeId(3), key), vec![point]),
            })
        };
        session.edit([lane(GAIN, 1)]);
        let strips = strips(session.project());
        assert_eq!(
            [strips[0].gain_automated, strips[0].mute_automated],
            [true, false]
        );
        assert_eq!(
            [strips[1].gain_automated, strips[1].mute_automated],
            [false, false]
        );

        session.edit([lane(MUTE, 2)]);
        let mut harness = harness(session);
        // Drums' mute is driven, so clicking it changes nothing; Bass's is not.
        harness.get_all_by_label("M").next().unwrap().click();
        harness.run();
        assert_eq!(param(&harness, 3, MUTE), None);
        harness.get_all_by_label("M").nth(1).unwrap().click();
        harness.run();
        assert_eq!(param(&harness, 6, MUTE), Some(1.0));
        let faders: Vec<_> = harness
            .query_all_by_role(Role::Slider)
            .map(|n| n.accesskit_node().is_disabled())
            .collect();
        assert_eq!(
            faders,
            [true, true],
            "Bass is muted now, so its fader is greyed too"
        );
    }

    /// A mixer node (ID 100) with the second track wired into `in1` and the
    /// first into `in2`, and a stray gain node into `in3`.
    fn mixed() -> Session {
        let mut session = two_tracks();
        let mut edits = vec![
            Edit::Apply(Command::AddNode {
                id: NodeId(100),
                node: Node::new(spare::MIXER)
                    .with_config(Config::new().with(spare::MIXER_INPUTS, Value::Int(3))),
            }),
            Edit::Apply(Command::AddNode {
                id: NodeId(101),
                node: Node::new("noodle.util.gain"),
            }),
        ];
        for (from, key, to) in [(4, "p", "in1"), (1, "p", "in2"), (101, "out", "in3")] {
            edits.push(Edit::Apply(spare::wire(
                Endpoint::new(NodeId(from), key),
                Endpoint::new(NodeId(100), to),
            )));
        }
        session.edit(edits);
        session
    }

    #[test]
    fn a_mixer_views_strips_follow_its_inputs() {
        let session = mixed();
        let project = session.project();
        let names: Vec<_> = mixer_strips(project, NodeId(100))
            .into_iter()
            .map(|s| (s.name, s.node.is_some()))
            .collect();
        // Tracks show their controls; the gain node's strip has none.
        assert_eq!(
            names,
            [
                ("Bass".to_string(), true),
                ("Drums".to_string(), true),
                ("In 3".to_string(), false)
            ]
        );
        assert_eq!(mixers(project), [NodeId(100)]);
        assert!(mixer_strips(project, NodeId(999)).is_empty());
    }

    #[test]
    fn the_drop_down_falls_back_when_the_mixer_is_gone() {
        let session = two_tracks();
        let mut harness = Harness::new_ui_state(
            |ui, view: &mut Option<NodeId>| {
                show(ui, session.project(), view, &|_, _| None);
            },
            Some(NodeId(100)),
        );
        harness.run();
        assert_eq!(*harness.state(), None);
        harness.get_by_label("Drums");
    }
}
