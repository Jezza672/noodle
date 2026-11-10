//! Articulations in the piano roll: a pill in the toolbar that gives the
//! selected notes an articulation from the project's list (and adds to or
//! renames the list), and the tint the notes take from theirs. The track
//! sends the change ahead of the note; an Articulation Map node between the
//! track and the instrument turns it into a keyswitch or the like.

use std::collections::BTreeSet;

use egui::Color32;
use noodle_core::{Clip, ClipId, MidiClip, Project};

use super::{Output, set_notes};
use crate::session::Edit;
use crate::toolbar::{Icon, Mode, Opens, Pill, pill_with_popover};

/// The colour of articulation `id` in `project`, if it has one.
pub fn tint(project: &Project, id: Option<u16>) -> Option<Color32> {
    let [r, g, b] = project.articulation(id?)?.colour;
    Some(Color32::from_rgb(r, g, b))
}

/// The toolbar pill: pressing it opens the articulation menu.
#[allow(clippy::too_many_arguments)]
pub fn controls(
    ui: &mut egui::Ui,
    mode: Mode,
    out: &mut Output,
    project: &Project,
    selected: &BTreeSet<u32>,
    id: ClipId,
    clip: &Clip,
    midi: &MidiClip,
) {
    let spec = Pill {
        icon: Icon::Tag,
        label: "Articulation",
        active: false,
        enabled: true,
        tip: "Give the selected notes an articulation, and edit the list",
    };
    pill_with_popover(ui, mode, &spec, Opens::Itself, "", |ui| {
        let enabled = !selected.is_empty();
        let mut choose = |ui: &mut egui::Ui, label: &str, articulation: Option<u16>| {
            if ui.add_enabled(enabled, egui::Button::new(label)).clicked() {
                let mut notes = midi.notes.clone();
                for note in notes.iter_mut().filter(|n| selected.contains(&n.id)) {
                    note.articulation = articulation;
                }
                out.edits.push(set_notes(id, clip, notes, false));
                ui.close();
            }
        };
        choose(ui, "Default", None);
        for articulation in project.articulations() {
            choose(ui, &articulation.name, Some(articulation.id));
        }
        if !enabled {
            ui.weak("Select notes to give them an articulation");
        }
        ui.separator();
        let new_id = egui::Id::new("new articulation");
        let mut name: String = ui.data(|d| d.get_temp(new_id)).unwrap_or_default();
        ui.horizontal(|ui| {
            ui.text_edit_singleline(&mut name);
            if ui
                .add_enabled(!name.trim().is_empty(), egui::Button::new("Add"))
                .clicked()
                && let Ok(command) = project.add_articulation_command(name.trim())
            {
                out.edits.push(Edit::Apply(command));
                name.clear();
            }
        });
        ui.data_mut(|d| d.insert_temp(new_id, name));
        if !project.articulations().is_empty() {
            ui.collapsing("Rename", |ui| {
                for articulation in project.articulations() {
                    let key = egui::Id::new(("rename articulation", articulation.id));
                    let mut text: String = ui
                        .data(|d| d.get_temp(key))
                        .unwrap_or_else(|| articulation.name.clone());
                    let edit = ui.text_edit_singleline(&mut text);
                    if edit.lost_focus()
                        && !text.trim().is_empty()
                        && text.trim() != articulation.name
                        && let Ok(command) =
                            project.rename_articulation_command(articulation.id, text.trim())
                    {
                        out.edits.push(Edit::Apply(command));
                    }
                    ui.data_mut(|d| d.insert_temp(key, text));
                }
            });
        }
    });
}
