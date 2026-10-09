//! The track headers: a name, mute and solo buttons and a gain slider, which
//! show and set the controls on the track group's boundary nodes.
//!
//! A track is a group, and its clips are played by the track input node
//! inside it. Gain and mute live on the group's first output node (its first
//! input node if it has no output), and are written there. Solo counts if
//! either boundary node has it, so turning it off clears it on all of them.

use std::collections::BTreeSet;

use egui::{Align, Layout, Pos2, Rect, RichText, Slider, Stroke, UiBuilder, vec2};
use noodle_core::group::{self, Controls};
use noodle_core::{Command, Graph, NodeId, Value};

use crate::session::Edit;
use crate::theme::timeline as colors;

/// The slider's range, in decibels: what the group stage takes, and what the
/// mixer's faders cover, so a gain set in one shows in the other.
const GAIN_RANGE: std::ops::RangeInclusive<f32> = -60.0..=24.0;

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
    pub gain: bool,
    pub mute: bool,
}

pub(crate) const AUTOMATED: &str = "Driven by a lane or a wire; edit or remove that to change it";

/// A track's controls: where they are set and what they say now.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TrackControls {
    /// The boundary node that holds gain and mute, and gets solo turned on.
    pub node: NodeId,
    pub controls: Controls,
    /// Silent because another track is soloed.
    pub muted_by_solo: bool,
    /// Armed for recording. Session state, so `controls_for` leaves it off
    /// for the caller to set.
    pub armed: bool,
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
    let ports = graph.group_ports(group);
    let node = ports.outputs.first().or(ports.inputs.first())?.node;
    Some(TrackControls {
        node,
        controls: graph.group_controls(group),
        muted_by_solo: muted_by_solo.contains(&group),
        armed: false,
    })
}

/// Turns solo on at `node`, or off everywhere in the track's group, since it
/// counts wherever it is.
fn solo_edit(graph: &Graph, input: NodeId, node: NodeId, on: bool) -> Command {
    let off = |node| set(node, group::SOLO, Some(0.0));
    if on {
        return set(node, group::SOLO, Some(1.0));
    }
    let boundary: Vec<NodeId> = graph
        .node(input)
        .and_then(|n| n.parent)
        .map(|group| {
            let ports = graph.group_ports(group);
            let all = ports.inputs.iter().chain(&ports.outputs);
            all.map(|p| p.node)
                .filter(|&id| graph.node(id).is_some_and(|n| n.controls().solo))
                .collect()
        })
        .unwrap_or_default();
    Command::Batch(boundary.into_iter().map(off).collect())
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
    /// The record-arm button was pressed: the arm state it asks for.
    pub arm: Option<bool>,
    /// The header was clicked: the track is now the selected one.
    pub select: bool,
}

/// Draws one track's header in `lane` and returns what the user changed.
/// `armed` is whether the track is armed for recording.
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
    let background = ui.interact(lane, ui.id().with(("header", input)), egui::Sense::click());
    changes.select = background.clicked();
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
    let silenced = track.is_some_and(|t| t.muted_by_solo);
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
        controls,
        armed,
        ..
    }) = track
    else {
        return changes;
    };
    let arm = &mut changes.arm;
    child.horizontal(|ui| {
        let button = |ui: &mut egui::Ui, text: &str, on: bool, tip: &str| {
            let text = RichText::new(text).size(11.0);
            ui.add_sized(vec2(20.0, 18.0), egui::Button::new(text).selected(on))
                .on_hover_text(tip)
                .clicked()
        };
        if button(ui, "R", armed, "Arm for recording") {
            *arm = Some(!armed);
        }
        let mute = ui
            .add_enabled_ui(!automated.mute, |ui| button(ui, "M", controls.mute, "Mute"))
            .inner;
        if mute {
            edits.push(Edit::Apply(set(
                node,
                group::MUTE,
                Some(f32::from(!controls.mute)),
            )));
        }
        if button(ui, "S", controls.solo, "Solo") {
            edits.push(Edit::Apply(solo_edit(graph, input, node, !controls.solo)));
        }
        let mut gain = controls.gain_db;
        ui.spacing_mut().slider_width = (ui.available_width() - 6.0).max(20.0);
        let slider = ui
            .add_enabled(
                !automated.gain,
                Slider::new(&mut gain, GAIN_RANGE)
                    .show_value(false)
                    .smart_aim(false),
            )
            .on_hover_text(if automated.gain {
                AUTOMATED.to_owned()
            } else {
                format!("Gain {:+.1} dB (double-click to reset)", gain)
            })
            .on_disabled_hover_text(AUTOMATED);
        // A slider only senses drags, so its response never reports a double
        // click; ask the pointer instead.
        let double = !automated.gain
            && slider.contains_pointer()
            && ui.input(|i| {
                i.pointer
                    .button_double_clicked(egui::PointerButton::Primary)
            });
        if double {
            // Written, not removed: a control that has been set keeps its stage in
            // the compiled graph, so resetting it can't change the graph's shape.
            edits.push(Edit::Apply(set(node, group::GAIN, Some(0.0))));
        } else if slider.changed() {
            edits.push(Edit::Drag(set(node, group::GAIN, Some(gain))));
        }
        if slider.drag_stopped() {
            edits.push(Edit::EndDrag);
        }
    });
    changes
}
