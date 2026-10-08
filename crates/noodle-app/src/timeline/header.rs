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
use noodle_core::{Command, Graph, NodeId};

use crate::session::Edit;
use crate::theme::timeline as colors;

/// The slider's range, in decibels: what the group stage takes, and what the
/// mixer's faders cover, so a gain set in one shows in the other.
const GAIN_RANGE: std::ops::RangeInclusive<f32> = -60.0..=24.0;

/// A track's controls: where they are set and what they say now.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TrackControls {
    /// The boundary node that holds gain and mute, and gets solo turned on.
    pub node: NodeId,
    pub controls: Controls,
    /// Silent because another track is soloed.
    pub muted_by_solo: bool,
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

fn set(node: NodeId, key: &str, value: Option<f32>) -> Command {
    Command::SetParam {
        node,
        key: key.to_owned(),
        value,
    }
}

/// What the user did in a header.
#[derive(Default)]
pub struct Changes {
    pub edits: Vec<Edit>,
    /// The record-arm button was pressed: the arm state it asks for.
    pub arm: Option<bool>,
}

/// Draws one track's header in `lane` and returns what the user changed.
/// `armed` is whether the track is armed for recording.
pub fn show(
    ui: &mut egui::Ui,
    graph: &Graph,
    lane: Rect,
    index: usize,
    input: NodeId,
    track: Option<TrackControls>,
    armed: bool,
) -> Changes {
    let mut changes = Changes::default();
    let edits = &mut changes.edits;
    let painter = ui.painter_at(lane);
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
    let name = RichText::new(format!("Track {}", index + 1)).color(if silenced {
        colors::TEXT_WEAK
    } else {
        colors::TEXT
    });
    let label = child.label(name);
    if silenced {
        label.on_hover_text("Muted by solo");
    }
    let Some(TrackControls { node, controls, .. }) = track else {
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
        if button(ui, "M", controls.mute, "Mute") {
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
        let slider = ui.add(
            Slider::new(&mut gain, GAIN_RANGE)
                .show_value(false)
                .smart_aim(false),
        );
        let slider = slider.on_hover_text(format!("Gain {:+.1} dB (double-click to reset)", gain));
        // A slider only senses drags, so its response never reports a double
        // click; ask the pointer instead.
        let double = slider.contains_pointer()
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
