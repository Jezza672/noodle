//! The track headers: a name, mute and solo buttons and a gain slider, which
//! show and set the controls on the track group's boundary nodes.
//!
//! A track is a group, and its clips are played by the track input node
//! inside it. Gain and mute are read from the group's output node; solo is on
//! either boundary node, but this view writes it there too.

use egui::{Align, Layout, Pos2, Rect, RichText, Slider, Stroke, UiBuilder, vec2};
use noodle_core::group::{self, Controls};
use noodle_core::{Command, Graph, NodeId};

use crate::session::Edit;
use crate::theme::timeline as colors;

/// The slider's range, in decibels. The group stage takes -60 to +24; a
/// header doesn't need all the headroom.
const GAIN_RANGE: std::ops::RangeInclusive<f32> = -60.0..=6.0;

/// A track's controls: where they are set and what they say now.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TrackControls {
    /// The boundary node that holds gain, mute and solo.
    pub node: NodeId,
    pub controls: Controls,
}

/// The controls for the track whose clips `input` plays, if it sits in a
/// group that has an output.
pub fn controls_for(graph: &Graph, input: NodeId) -> Option<TrackControls> {
    let group = graph.node(input)?.parent?;
    let node = graph.group_ports(group).outputs.first()?.node;
    Some(TrackControls {
        node,
        controls: graph.node(node)?.controls(),
    })
}

fn set(node: NodeId, key: &str, value: Option<f32>) -> Command {
    Command::SetParam {
        node,
        key: key.to_owned(),
        value,
    }
}

/// Draws one track's header in `lane` and returns what the user changed.
pub fn show(
    ui: &mut egui::Ui,
    lane: Rect,
    index: usize,
    track: Option<TrackControls>,
) -> Vec<Edit> {
    let mut edits = Vec::new();
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
    child.label(RichText::new(format!("Track {}", index + 1)).color(colors::TEXT));
    let Some(TrackControls { node, controls }) = track else {
        return edits;
    };
    child.horizontal(|ui| {
        let button = |ui: &mut egui::Ui, text: &str, on: bool, tip: &str| {
            let text = RichText::new(text).size(11.0);
            ui.add_sized(vec2(20.0, 18.0), egui::Button::new(text).selected(on))
                .on_hover_text(tip)
                .clicked()
        };
        if button(ui, "M", controls.mute, "Mute") {
            edits.push(Edit::Apply(set(
                node,
                group::MUTE,
                Some(f32::from(!controls.mute)),
            )));
        }
        if button(ui, "S", controls.solo, "Solo") {
            edits.push(Edit::Apply(set(
                node,
                group::SOLO,
                Some(f32::from(!controls.solo)),
            )));
        }
        let mut gain = controls.gain_db;
        ui.spacing_mut().slider_width = (ui.available_width() - 6.0).max(20.0);
        let slider = ui.add(
            Slider::new(&mut gain, GAIN_RANGE)
                .show_value(false)
                .smart_aim(false),
        );
        let slider = slider.on_hover_text(format!("Gain {:+.1} dB (double-click to reset)", gain));
        if slider.double_clicked() {
            edits.push(Edit::Apply(set(node, group::GAIN, None)));
        } else if slider.changed() {
            edits.push(Edit::Drag(set(node, group::GAIN, Some(gain))));
        }
        if slider.drag_stopped() {
            edits.push(Edit::EndDrag);
        }
    });
    edits
}
