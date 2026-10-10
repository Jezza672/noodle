//! The mixer: one strip per track, showing and setting that track's gain,
//! mute and solo; or, over a mixer node, one strip per wired input, showing
//! and setting that input's gain and mute.
//!
//! The mixer owns no state. A track is a group node, and its controls are
//! parameters on the group's boundary nodes (see `noodle_core::group`), so a
//! strip reads them from the project and a fader move is a `SetParam` on the
//! boundary node. Strips show the top-level groups, in the order they were
//! made. Over a mixer node the controls are the node's own `gainN` and
//! `muteN` parameters instead, and the strips know nothing of the tracks
//! feeding them: a track's own gain, mute and solo stay on the track.

use egui::{Align, Color32, Layout, RichText, Slider, Ui, Vec2};
use noodle_core::group::{Controls, GAIN, GROUP, GROUP_INPUT, GROUP_OUTPUT, MUTE, SOLO};
use noodle_core::{Command, Endpoint, Graph, NodeId, Project, Value, spare};

use crate::editor::{MeterAxis, MeterChannel};
use crate::session::Edit;
use crate::timeline::header;

/// The config key a group's display name is kept under.
pub const NAME: &str = "name";

const FADER_RANGE: std::ops::RangeInclusive<f32> = -60.0..=24.0;
const STRIP_WIDTH: f32 = 84.0;
const FADER_HEIGHT: f32 = 170.0;
const METER_WIDTH: f32 = 16.0;

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
    /// A lane or wire drives the gain, so the fader would do nothing.
    pub gain_automated: bool,
    /// The same for mute.
    pub mute_automated: bool,
    /// Which input of the mixer node this strip is, from 0, in a mixer view.
    pub channel: Option<usize>,
    /// The parameter on `node` holding the gain, in dB.
    pub gain_key: String,
    /// The parameter on `node` holding the mute, 0 or 1.
    pub mute_key: String,
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

/// The strips for a mixer node, one per wired input in input order. A strip
/// is named for the track feeding the input, if one does, but sets the
/// mixer node's own gain and mute for that input.
pub fn mixer_strips(project: &Project, mixer: NodeId) -> Vec<Strip> {
    let graph = project.graph();
    let Some(node) = graph.node(mixer) else {
        return Vec::new();
    };
    let tracks = strips(project);
    (1..=spare::mixer_inputs(node))
        .filter_map(|i| {
            let i = i as usize;
            let source = graph.source(&Endpoint::new(mixer, spare::mixer_input_key(i)))?;
            let (gain_key, mute_key) = (spare::mixer_gain_key(i), spare::mixer_mute_key(i));
            let read = |key: &str| node.params.get(key).copied();
            Some(Strip {
                group: source.node,
                name: match tracks.iter().find(|strip| strip.group == source.node) {
                    Some(strip) => strip.name.clone(),
                    None => format!("In {i}"),
                },
                controls: Controls {
                    gain_db: read(&gain_key).unwrap_or(0.0),
                    mute: read(&mute_key).is_some_and(|mute| mute >= 0.5),
                    solo: false,
                },
                node: Some(mixer),
                muted_by_solo: false,
                gain_automated: driven(project, Some(mixer), &gain_key),
                mute_automated: driven(project, Some(mixer), &mute_key),
                channel: Some(i - 1),
                gain_key,
                mute_key,
            })
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
            gain_key: GAIN.to_owned(),
            mute_key: MUTE.to_owned(),
        })
        .collect()
}

/// Whether a lane or a wire drives `key` on `node`, which then overrides what
/// the strip sets.
fn driven(project: &Project, node: Option<NodeId>, key: &str) -> bool {
    node.is_some_and(|node| {
        let input = Endpoint::new(node, key);
        project.lane_for(&input).is_some() || project.graph().source(&input).is_some()
    })
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
/// node shown. The mixer only ever looks at one mixer node, so a view that is
/// unset, or whose node has gone, falls back to the first one.
pub fn show(
    ui: &mut Ui,
    project: &Project,
    view: &mut Option<NodeId>,
    levels: &dyn Fn(NodeId, usize) -> Option<MeterChannel>,
) -> Vec<Edit> {
    let graph = project.graph();
    let mixers = mixers(project);
    if view.is_none_or(|m| !mixers.contains(&m)) {
        *view = mixers.first().copied();
    }
    let Some(mixer) = *view else {
        ui.weak("There is no mixer yet. Add a Mix node, or add a track.");
        return Vec::new();
    };
    if mixers.len() > 1 {
        ui.horizontal(|ui| {
            ui.label("Mixer");
            egui::ComboBox::from_id_salt("mixer view")
                .selected_text(format!("Mix {}", mixer.0))
                .show_ui(ui, |ui| {
                    for &m in &mixers {
                        ui.selectable_value(view, Some(m), format!("Mix {}", m.0));
                    }
                });
        });
    }
    let strips = mixer_strips(project, mixer);
    if strips.is_empty() {
        ui.weak("Nothing is wired into this mixer.");
        return Vec::new();
    }
    let mut edits = Vec::new();
    egui::ScrollArea::horizontal().show(ui, |ui| {
        ui.horizontal_top(|ui| {
            for (index, strip) in strips.iter().enumerate() {
                ui.push_id((strip.group, index), |ui| {
                    let level = strip.channel.and_then(|channel| levels(mixer, channel));
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
            // A silent strip's fader is greyed, but its buttons stay live so
            // it can be unmuted or unsoloed.
            let silent = strip.controls.mute || strip.muted_by_solo;
            // A lane overrides the parameter it drives, so those controls
            // are greyed out with the reason, as on the track header.
            ui.horizontal_top(|ui| {
                // The input's level beside its fader, in a mixer view.
                if strip.channel.is_some() {
                    let (area, _) = ui.allocate_exact_size(
                        Vec2::new(METER_WIDTH, FADER_HEIGHT),
                        egui::Sense::hover(),
                    );
                    crate::editor::draw_level(
                        ui.painter(),
                        area,
                        &level.unwrap_or_default(),
                        MeterAxis::Vertical,
                    );
                }
                let faded = ui.add_enabled_ui(
                    strip.node.is_some() && !silent && !strip.gain_automated,
                    |ui| ui.vertical(|ui| fader(ui, strip, edits)),
                );
                if strip.gain_automated {
                    faded.response.on_disabled_hover_text(header::AUTOMATED);
                }
            });
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
                        edits.push(Edit::Apply(set(node, &strip.mute_key, value)));
                    }
                    // Solo belongs to the track, not to a mixer's input.
                    if strip.channel.is_none() {
                        let solo = toggle(ui, "S", strip.controls.solo, crate::theme::SOLO);
                        if solo.on_hover_text("Solo: mutes the other tracks").clicked() {
                            edits.extend(solo_edit(graph, strip, !strip.controls.solo));
                        }
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
    // As long as the meter beside it.
    ui.spacing_mut().slider_width = FADER_HEIGHT;
    let response = ui.add(
        Slider::new(&mut db, FADER_RANGE)
            .vertical()
            .show_value(false)
            .handle_shape(egui::style::HandleShape::Rect { aspect_ratio: 0.5 })
            .trailing_fill(false),
    );
    if response.dragged() {
        edits.push(Edit::Drag(set(node, &strip.gain_key, Some(db))));
    } else if response.drag_stopped() {
        edits.push(Edit::EndDrag);
    } else if response.changed() {
        edits.push(Edit::Apply(set(node, &strip.gain_key, Some(db))));
    }
    // The reading doubles as the reset: a click puts the gain back to 0 dB.
    let reading = ui
        .small_button(format!("{:+.1} dB", strip.controls.gain_db))
        .on_hover_text("Click to reset to 0 dB");
    if reading.clicked() {
        edits.push(Edit::Apply(set(node, &strip.gain_key, Some(0.0))));
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
        let harness = harness(mixed());
        harness.get_by_label("Drums");
        harness.get_by_label("Bass");
    }

    #[test]
    fn with_no_mixer_it_says_so_and_there_is_no_all_tracks_view() {
        let harness = harness(two_tracks());
        harness.get_by_label_contains("There is no mixer yet");
        assert!(harness.query_by_label("Drums").is_none());
        assert!(harness.query_by_label_contains("All tracks").is_none());
    }

    #[test]
    fn an_unwired_mixer_says_so() {
        let mut session = Session::new(crate::session::Nodes::all());
        session.edit([Edit::Apply(Command::AddNode {
            id: NodeId(100),
            node: Node::new(spare::MIXER),
        })]);
        let harness = harness(session);
        harness.get_by_label("Nothing is wired into this mixer.");
    }

    #[test]
    fn mute_toggles_the_channels_mute_parameter() {
        let mut harness = harness(mixed());
        harness.get_all_by_label("M").next().unwrap().click();
        harness.run();
        assert_eq!(param(&harness, 100, "mute1"), Some(1.0));
        assert_eq!(param(&harness, 100, "mute2"), None);
        // Again: back to the default, still written so the stage stays.
        harness.get_all_by_label("M").next().unwrap().click();
        harness.run();
        assert_eq!(param(&harness, 100, "mute1"), Some(0.0));
    }

    #[test]
    fn solo_mutes_the_other_track_and_unsolo_clears_it_wherever_it_was_set() {
        let mut session = two_tracks();
        let drums = strips(session.project())[0].clone();
        session.edit(solo_edit(session.project().graph(), &drums, true));
        assert_eq!(
            session
                .project()
                .graph()
                .node(NodeId(3))
                .unwrap()
                .params
                .get(SOLO),
            Some(&1.0)
        );
        assert_eq!(session.project().graph().solo_muted(), [NodeId(4)].into());
        let strips = strips(session.project());
        assert!(strips[1].muted_by_solo && !strips[0].muted_by_solo);

        // Solo on the input node, as a hand-edited file might have it.
        session.edit([Edit::Apply(Command::SetParam {
            node: NodeId(2),
            key: SOLO.into(),
            value: Some(1.0),
        })]);
        let drums = self::strips(session.project())[0].clone();
        session.edit(solo_edit(session.project().graph(), &drums, false));
        assert!(session.project().graph().solo_muted().is_empty());
    }

    #[test]
    fn a_fader_reads_the_gain_and_its_reading_resets_it() {
        let mut session = mixed();
        session.edit([Edit::Apply(set(NodeId(100), "gain1", Some(-12.0)))]);
        let mut harness = harness(session);
        let fader = harness.get_all_by_role(Role::Slider).next().unwrap();
        assert_eq!(fader.accesskit_node().numeric_value(), Some(-12.0));
        harness.get_by_label("-12.0 dB").click();
        harness.run();
        assert_eq!(param(&harness, 100, "gain1"), Some(0.0));
        harness.get_all_by_label("+0.0 dB").next().unwrap();
    }

    #[test]
    fn dragging_the_fader_sets_the_gain_as_one_undo_step() {
        let mut harness = harness(mixed());
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
        let gain = param(&harness, 100, "gain1").expect("the fader moved");
        assert!(gain > 0.0, "dragged up: {gain}");
        // The whole drag is one step.
        let state = harness.state_mut();
        state.undo();
        assert_eq!(param(&harness, 100, "gain1"), None);
    }

    #[test]
    fn a_lane_on_gain_or_mute_greys_that_control_on_the_strip() {
        use noodle_core::{AutomationLane, AutomationPoint, Curve, LaneId};
        let mut session = mixed();
        let point = AutomationPoint {
            tick: noodle_core::Tick(0),
            value: -6.0,
            curve: Curve::Hold,
        };
        let lane = |key: &str, id: u64| {
            Edit::Apply(Command::AddLane {
                id: LaneId(id),
                lane: AutomationLane::new(Endpoint::new(NodeId(100), key), vec![point]),
            })
        };
        session.edit([lane("gain1", 1)]);
        let strips = mixer_strips(session.project(), NodeId(100));
        assert_eq!(
            [strips[0].gain_automated, strips[0].mute_automated],
            [true, false]
        );
        assert_eq!(
            [strips[1].gain_automated, strips[1].mute_automated],
            [false, false]
        );

        session.edit([lane("mute1", 2)]);
        let mut harness = harness(session);
        // The first channel's mute is driven, so clicking it changes nothing.
        harness.get_all_by_label("M").next().unwrap().click();
        harness.run();
        assert_eq!(param(&harness, 100, "mute1"), None);
        harness.get_all_by_label("M").nth(1).unwrap().click();
        harness.run();
        assert_eq!(param(&harness, 100, "mute2"), Some(1.0));
        let faders: Vec<_> = harness
            .query_all_by_role(Role::Slider)
            .map(|n| n.accesskit_node().is_disabled())
            .collect();
        assert_eq!(
            faders,
            [true, true, false],
            "channel 1 is driven, channel 2 is muted now, channel 3 is free"
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
        // Every input has the mixer's own controls, fed by a track or not.
        assert_eq!(
            names,
            [
                ("Bass".to_string(), true),
                ("Drums".to_string(), true),
                ("In 3".to_string(), true)
            ]
        );
        assert_eq!(mixers(project), [NodeId(100)]);
        assert!(mixer_strips(project, NodeId(999)).is_empty());
    }

    #[test]
    fn a_mixer_views_controls_are_the_mixers_own_not_the_tracks() {
        let mut session = mixed();
        session.edit([
            // Bass (the first input) is quiet as a track, and the mixer's
            // second input is muted and turned down.
            Edit::Apply(set(NodeId(6), GAIN, Some(-9.0))),
            Edit::Apply(set(NodeId(100), "gain2", Some(-3.0))),
            Edit::Apply(set(NodeId(100), "mute2", Some(1.0))),
        ]);
        let strips = mixer_strips(session.project(), NodeId(100));
        let summary: Vec<_> = strips
            .iter()
            .map(|s| (s.controls.gain_db, s.controls.mute, s.node))
            .collect();
        assert_eq!(
            summary,
            [
                (0.0, false, Some(NodeId(100))),
                (-3.0, true, Some(NodeId(100))),
                (0.0, false, Some(NodeId(100)))
            ]
        );

        let mut harness = Harness::new_ui_state(
            |ui, session: &mut Session| {
                let mut view = Some(NodeId(100));
                let edits = show(ui, session.project(), &mut view, &|_, _| None);
                session.edit(edits);
            },
            session,
        );
        harness.set_size(vec2(600.0, 600.0));
        harness.run();
        // Solo is the track's, so a mixer view has none.
        assert!(harness.query_by_label("S").is_none());
        // Mute on the first input is the mixer's `mute1`.
        harness.get_all_by_label("M").next().unwrap().click();
        harness.run();
        assert_eq!(param(&harness, 100, "mute1"), Some(1.0));
        assert_eq!(param(&harness, 6, MUTE), None);
        // Resetting the third reading writes `gain3`.
        let third = harness.get_all_by_label("+0.0 dB").last().unwrap();
        third.click();
        harness.run();
        assert_eq!(param(&harness, 100, "gain3"), Some(0.0));
        assert_eq!(param(&harness, 6, GAIN), Some(-9.0));
    }

    #[test]
    fn a_wire_or_lane_on_a_channels_gain_greys_its_fader() {
        let mut session = mixed();
        session.edit([Edit::Apply(spare::wire(
            Endpoint::new(NodeId(101), "out"),
            Endpoint::new(NodeId(100), "gain2"),
        ))]);
        let strips = mixer_strips(session.project(), NodeId(100));
        let driven: Vec<_> = strips.iter().map(|s| s.gain_automated).collect();
        assert_eq!(driven, [false, true, false]);
    }

    #[test]
    fn the_view_falls_back_to_the_first_mixer_when_unset_or_gone() {
        let session = mixed();
        let mut harness = Harness::new_ui_state(
            |ui, view: &mut Option<NodeId>| {
                show(ui, session.project(), view, &|_, _| None);
            },
            Some(NodeId(999)),
        );
        harness.run();
        assert_eq!(*harness.state(), Some(NodeId(100)));
        harness.get_by_label("Drums");
    }
}
