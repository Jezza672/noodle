//! The track headers: a name, arm, mute and solo buttons, which
//! show and set the controls on the track group's boundary nodes.
//!
//! A track is a group, and its clips are played by the track input node
//! inside it. Mute is the track input's parameter, which the engine applies
//! at the group's outputs; there is no track gain (a clip has its own). Solo
//! lives on the group's boundary nodes and counts if either has it, so
//! turning it off clears it on all of them.

use std::collections::BTreeSet;

use egui::{Align, Layout, Pos2, Rect, RichText, Stroke, UiBuilder, vec2};
use noodle_core::group::{self, Controls};
use noodle_core::{Command, Graph, NodeId, Value};

use crate::session::Edit;
use crate::theme::timeline as colors;

/// The group's config setting that holds the track's name; the mixer reads
/// the same one.
use crate::mixer::NAME;

/// A name being typed in a header.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rename {
    pub group: NodeId,
    pub text: String,
}

/// The name a track was given, if any.
fn stored_name(graph: &Graph, group: NodeId) -> Option<String> {
    match graph.node(group)?.config.get(NAME) {
        Some(Value::Text(text)) if !text.is_empty() => Some(text.clone()),
        _ => None,
    }
}

/// Which of a track's controls a lane or a wire drives. Either overrides
/// the parameter it automates, so those controls are greyed out rather than
/// left doing nothing.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Automated {
    pub mute: bool,
}

pub(crate) const AUTOMATED: &str = "Driven by a lane or a wire; edit or remove that to change it";

/// A track's controls: where they are set and what they say now.
#[derive(Clone, Debug, PartialEq)]
pub struct TrackControls {
    /// The boundary node that holds gain and mute, and gets solo turned on,
    /// and the ID of the port on it they belong to.
    pub node: NodeId,
    pub port: u32,
    /// Where mute is set: the track input's parameter, or else the group
    /// output's own if something else is wired to that.
    pub mute_target: (NodeId, String),
    pub controls: Controls,
    /// Silent because another track is soloed.
    pub muted_by_solo: bool,
    /// Armed for recording. Session state, so `controls_for` leaves it off
    /// for the caller to set.
    pub armed_audio: bool,
    pub armed_midi: bool,
}

/// The controls for the track whose clips `input` plays, if it sits in a group
/// that has a boundary node. `muted_by_solo` is the graph's `solo_muted` set,
/// worked out once for all the headers.
pub fn controls_for(
    graph: &Graph,
    input: NodeId,
    muted_by_solo: &BTreeSet<NodeId>,
) -> Option<TrackControls> {
    let group = graph.node(input)?.parent?;
    let port = graph.control_port(group)?;
    Some(TrackControls {
        node: port.node,
        port: port.id,
        mute_target: graph.control_target(group, group::MUTE)?,
        controls: graph.group_controls(group),
        muted_by_solo: muted_by_solo.contains(&group),
        armed_audio: false,
        armed_midi: false,
    })
}

/// Turns solo on at port `port` of `node`, or off everywhere in the track's
/// group, since it counts wherever it is.
fn solo_edit(graph: &Graph, input: NodeId, node: NodeId, port: u32, on: bool) -> Command {
    let off = |node, port| set(node, &group::control_key(group::SOLO, port), Some(0.0));
    if on {
        return set(node, &group::control_key(group::SOLO, port), Some(1.0));
    }
    let soloed: Vec<(NodeId, u32)> = graph
        .node(input)
        .and_then(|n| n.parent)
        .map(|group| {
            let ports = graph.group_ports(group);
            let all = ports.inputs.iter().chain(&ports.outputs);
            all.filter(|p| {
                graph
                    .node(p.node)
                    .is_some_and(|n| n.port_controls(p.id).solo)
            })
            .map(|p| (p.node, p.id))
            .collect()
        })
        .unwrap_or_default();
    Command::Batch(soloed.into_iter().map(|(n, p)| off(n, p)).collect())
}

/// Removes a track: its group, with the track input, the clips it plays and
/// the lanes on it, as one undo step. Wires from the group go with it.
pub fn delete_track(group: NodeId) -> Command {
    Command::RemoveNode { id: group }
}

fn set(node: NodeId, key: &str, value: Option<f32>) -> Command {
    Command::SetParam {
        node,
        key: key.to_owned(),
        value,
    }
}

/// Where a track's header is, and which track it is.
#[derive(Clone, Copy, Debug)]
pub struct Lane {
    pub rect: Rect,
    pub index: usize,
    /// The track input node that plays the track's clips.
    pub input: NodeId,
    /// The track is selected, so Delete removes it.
    pub selected: bool,
}

/// What the user did in a header.
#[derive(Default)]
pub struct Changes {
    pub edits: Vec<Edit>,
    /// A record-arm button was pressed: the arm state it asks for.
    pub arm_audio: Option<bool>,
    pub arm_midi: Option<bool>,
    /// The header was clicked: the track is now the selected one.
    pub select: bool,
    /// The header is being dragged to a new place in the track order.
    pub drag: Option<HeaderDrag>,
}

/// A header being dragged by its background.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HeaderDrag {
    /// The pointer's screen position.
    pub x: f32,
    pub y: f32,
    /// The button was let go, so the track lands here.
    pub released: bool,
}

/// Draws one track's header in `lane` and returns what the user changed.
/// `armed_audio` and `armed_midi` are whether the track is armed for
/// recording each kind of input.
pub fn show(
    ui: &mut egui::Ui,
    graph: &Graph,
    lane: Lane,
    track: Option<TrackControls>,
    automated: Automated,
    renaming: &mut Option<Rename>,
) -> Changes {
    let Lane {
        rect: lane,
        index,
        input,
        selected,
    } = lane;
    let mut changes = Changes::default();
    let edits = &mut changes.edits;
    // Under the header's widgets, so a click on empty header selects the
    // track and a right-click offers to delete it.
    let background = ui.interact(
        lane,
        ui.id().with(("header", input)),
        egui::Sense::click_and_drag(),
    );
    changes.select = background.clicked() || background.drag_started();
    if (background.dragged() || background.drag_stopped())
        && let Some(pos) = background.interact_pointer_pos()
    {
        changes.drag = Some(HeaderDrag {
            x: pos.x,
            y: pos.y,
            released: background.drag_stopped(),
        });
    }
    let group_of_track = graph.node(input).and_then(|n| n.parent);
    background.context_menu(|ui| {
        if ui
            .add_enabled(group_of_track.is_some(), egui::Button::new("Delete track"))
            .clicked()
        {
            ui.close();
            if let Some(group) = group_of_track {
                edits.push(Edit::Apply(delete_track(group)));
            }
        }
    });
    let painter = ui.painter_at(lane);
    if selected {
        painter.rect_stroke(
            lane.shrink(1.0),
            0.0,
            Stroke::new(1.5, colors::SELECTED),
            egui::StrokeKind::Inside,
        );
    }
    painter.rect_filled(
        Rect::from_min_size(lane.min, vec2(4.0, lane.height() - 1.0)),
        0.0,
        colors::track_colour(index),
    );
    painter.hline(
        lane.x_range(),
        lane.bottom() - 0.5,
        Stroke::new(1.0, colors::LANE_EVEN),
    );
    let inner = Rect::from_min_max(
        Pos2::new(lane.left() + 12.0, lane.top() + 5.0),
        Pos2::new(lane.right() - 6.0, lane.bottom() - 4.0),
    );
    let mut child = ui.new_child(
        UiBuilder::new()
            .max_rect(inner)
            .layout(Layout::top_down(Align::Min)),
    );
    child.set_clip_rect(lane.intersect(ui.clip_rect()));
    let silenced = track.as_ref().is_some_and(|t| t.muted_by_solo);
    let group = graph.node(input).and_then(|n| n.parent);
    let stored = group.and_then(|group| stored_name(graph, group));
    let shown = stored
        .clone()
        .unwrap_or_else(|| format!("Track {}", index + 1));
    let typing = group.is_some() && renaming.as_ref().is_some_and(|r| Some(r.group) == group);
    if typing {
        let rename = renaming.as_mut().expect("checked above");
        let field = child.add(
            egui::TextEdit::singleline(&mut rename.text)
                .desired_width(inner.width())
                .id(child.id().with(("rename", rename.group))),
        );
        if !field.has_focus() && !field.lost_focus() {
            field.request_focus();
        }
        if field.lost_focus() {
            let cancelled = child.input(|i| i.key_pressed(egui::Key::Escape));
            let done = renaming.take().expect("checked above");
            let text = done.text.trim().to_owned();
            let wanted = (!text.is_empty()).then_some(text);
            if !cancelled && wanted != stored {
                edits.push(Edit::Apply(Command::SetConfig {
                    node: done.group,
                    key: NAME.to_owned(),
                    value: wanted.map(Value::Text),
                }));
            }
        }
    } else {
        let name = RichText::new(&shown).color(if silenced {
            colors::TEXT_WEAK
        } else {
            colors::TEXT
        });
        let label = child.add(egui::Label::new(name).sense(egui::Sense::click()));
        if silenced {
            label.clone().on_hover_text("Muted by solo");
        }
        if let Some(group) = group
            && label.double_clicked()
        {
            *renaming = Some(Rename {
                group,
                text: stored.unwrap_or_default(),
            });
        }
    }
    let Some(TrackControls {
        node,
        port,
        mute_target,
        controls,
        armed_audio,
        armed_midi,
        ..
    }) = track
    else {
        return changes;
    };
    let (arm_audio, arm_midi) = (&mut changes.arm_audio, &mut changes.arm_midi);
    child.horizontal(|ui| {
        let button = |ui: &mut egui::Ui, text: &str, on: bool, tip: &str| {
            let text = RichText::new(text).size(10.0);
            let width = if text.text().len() > 1 { 23.0 } else { 18.0 };
            ui.add_sized(vec2(width, 18.0), egui::Button::new(text).selected(on))
                .on_hover_text(tip)
                .clicked()
        };
        if button(ui, "RA", armed_audio, "Arm for recording audio") {
            *arm_audio = Some(!armed_audio);
        }
        if button(
            ui,
            "RM",
            armed_midi,
            "Arm for recording MIDI, and playing it live",
        ) {
            *arm_midi = Some(!armed_midi);
        }
        let mute = ui
            .add_enabled_ui(!automated.mute, |ui| button(ui, "M", controls.mute, "Mute"))
            .inner;
        if mute {
            edits.push(Edit::Apply(set(
                mute_target.0,
                &mute_target.1,
                Some(f32::from(!controls.mute)),
            )));
        }
        if button(ui, "S", controls.solo, "Solo") {
            edits.push(Edit::Apply(solo_edit(
                graph,
                input,
                node,
                port,
                !controls.solo,
            )));
        }
    });
    changes
}
