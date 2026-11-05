//! A Pattern Player's slots, in the properties panel: the list of its
//! patterns (ordinary MIDI clips the player owns), a button to open one in the
//! piano roll, and a step grid, the drum and gate editor, which writes the
//! same notes.

use egui::{Color32, Rect, RichText, Sense, Ui, pos2, vec2};
use noodle_core::{
    Clip, ClipContent, ClipId, Command, MidiClip, MidiNote, Node, NodeId, Project, Tick,
};
use noodle_nodes::{CLOCK_DIVISIONS, DIVISION_TICKS, PATTERN_MAX_SLOTS};

use crate::session::{Edit, Session};
use crate::theme;

/// A step grid's cell, in graph points.
const CELL: f32 = 18.0;

fn request_id() -> egui::Id {
    egui::Id::new("open a pattern in the piano roll")
}

/// Asks the app to open the pattern `clip` in the piano roll.
pub fn request_roll(ctx: &egui::Context, clip: ClipId) {
    ctx.data_mut(|d| d.insert_temp(request_id(), Some(clip)));
}

/// The pattern a [`request_roll`] asked for, once.
pub fn take_request(ctx: &egui::Context) -> Option<ClipId> {
    ctx.data_mut(|d| d.remove_temp::<Option<ClipId>>(request_id()).flatten())
}

/// The Pattern Players in the project, in node order.
pub fn players(project: &Project) -> Vec<NodeId> {
    project
        .graph()
        .nodes()
        .filter(|(_, n)| n.type_id == noodle_nodes::PATTERN_ID)
        .map(|(id, _)| id)
        .collect()
}

/// How a player's region is laid out: the first step, how many steps and how
/// long one is in ticks.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Region {
    pub start: usize,
    pub steps: usize,
    pub division: usize,
    pub step_ticks: i64,
}

impl Region {
    pub fn of(node: &Node) -> Self {
        let get = |key: &str, default: f32| node.params.get(key).copied().unwrap_or(default);
        let division = (get("step", 4.0).round().max(0.0) as usize).min(DIVISION_TICKS.len() - 1);
        Self {
            start: get("start", 0.0).round().max(0.0) as usize,
            steps: (get("length", 16.0).round().max(1.0) as usize).min(256),
            division,
            step_ticks: DIVISION_TICKS[division] as i64,
        }
    }

    /// The tick the region starts at.
    pub fn begin_tick(&self) -> i64 {
        self.start as i64 * self.step_ticks
    }

    /// The tick the region ends at.
    pub fn end_tick(&self) -> i64 {
        (self.start + self.steps) as i64 * self.step_ticks
    }
}

/// The slot to play with `node` set to `slot`: the clips are in ID order.
pub fn slots_of(project: &Project, node: NodeId) -> Vec<ClipId> {
    project.patterns_of(node).map(|(id, _)| id).collect()
}

/// A command adding an empty pattern as a new slot of `node`, as long as its
/// region.
pub fn new_slot(project: &Project, node: NodeId, region: &Region) -> Command {
    Command::AddClip {
        id: project.next_clip_id(),
        clip: Clip::midi(
            node,
            Tick::ZERO,
            Tick(region.end_tick().max(region.step_ticks)),
        ),
    }
}

/// A command copying a timeline MIDI clip into `node` as a new slot.
pub fn copy_to(project: &Project, clip: &Clip, node: NodeId) -> Option<Command> {
    let midi = clip.as_midi()?;
    let mut copy = midi.clone();
    copy.takes = None;
    Some(Command::AddClip {
        id: project.next_clip_id(),
        clip: Clip {
            node,
            start: Tick::ZERO,
            content: ClipContent::Midi(copy),
        },
    })
}

/// The keys the grid shows rows for, highest first: those the clip's notes
/// play in the region, and `extra`.
pub fn rows(midi: &MidiClip, region: &Region, extra: &[u8]) -> Vec<u8> {
    let mut keys: Vec<u8> = midi
        .notes
        .iter()
        .filter(|n| n.start.0 >= region.begin_tick() && n.start.0 < region.end_tick())
        .map(|n| n.key)
        .chain(extra.iter().copied())
        .collect();
    keys.sort_unstable_by(|a, b| b.cmp(a));
    keys.dedup();
    keys
}

/// The note on `key` that starts on step `step` of the region, if any.
pub fn note_at(midi: &MidiClip, region: &Region, key: u8, step: usize) -> Option<usize> {
    let start = region.begin_tick() + step as i64 * region.step_ticks;
    midi.notes
        .iter()
        .position(|n| n.key == key && n.start.0 == start)
}

/// Puts a note on `key` at `step`, half a step long so that neighbours leave
/// a gap for a gate to close in, or takes it away if there is one.
pub fn toggle(midi: &mut MidiClip, region: &Region, key: u8, step: usize) {
    match note_at(midi, region, key, step) {
        Some(index) => {
            midi.notes.remove(index);
        }
        None => {
            let start = region.begin_tick() + step as i64 * region.step_ticks;
            let mut note = MidiNote::new(Tick(start), Tick((region.step_ticks / 2).max(1)), key);
            note.velocity = 0.8;
            midi.notes.push(note);
            // The clip has to reach the note for it to play.
            midi.length = Tick(midi.length.0.max(region.end_tick()));
        }
    }
}

/// The triplet division with the same name plus a T, if there is one: the
/// index of `1/16T` for `1/16`.
pub fn triplet_of(division: usize) -> Option<usize> {
    let name = *CLOCK_DIVISIONS.get(division)?;
    if name.ends_with('T') || name.ends_with('D') {
        return None;
    }
    CLOCK_DIVISIONS
        .iter()
        .position(|other| *other == format!("{name}T"))
}

/// The pattern with every note moved from the straight grid to the triplet
/// one, step for step: three in the time of two. Its length shrinks the same
/// way, so the region still holds the same steps.
pub fn convert_to_triplets(midi: &MidiClip) -> MidiClip {
    let scale = |ticks: i64| ticks * 2 / 3;
    let mut out = midi.clone();
    out.length = Tick(scale(midi.length.0).max(1));
    for note in &mut out.notes {
        note.start = Tick(scale(note.start.0));
        note.length = Tick(scale(note.length.0).max(1));
    }
    out
}

/// The panel section for the Pattern Player `id`.
pub fn show(ui: &mut Ui, session: &Session, id: NodeId, node: &Node, edits: &mut Vec<Edit>) {
    let project = session.project();
    let region = Region::of(node);
    let slots = slots_of(project, id);
    ui.separator();
    ui.label(RichText::new("Pattern").strong());
    let playing = node.params.get("slot").copied().unwrap_or(0.0).round() as usize;
    for (index, &clip_id) in slots.iter().enumerate() {
        let Some(midi) = project.clip(clip_id).and_then(Clip::as_midi) else {
            continue;
        };
        ui.horizontal(|ui| {
            let mark = if index == playing.min(slots.len().saturating_sub(1)) {
                "▶"
            } else {
                " "
            };
            ui.label(format!(
                "{mark} Slot {} · {} notes",
                index + 1,
                midi.notes.len()
            ));
            if ui
                .button("Edit")
                .on_hover_text("Open in the piano roll")
                .clicked()
            {
                request_roll(ui.ctx(), clip_id);
            }
            if ui.button("Play").on_hover_text("Play this slot").clicked() {
                edits.push(Edit::Apply(Command::SetParam {
                    node: id,
                    key: "slot".into(),
                    value: Some(index as f32),
                }));
            }
            if ui.button("Remove").clicked() {
                edits.push(Edit::Apply(Command::RemoveClip { id: clip_id }));
            }
        });
    }
    if slots.len() < PATTERN_MAX_SLOTS
        && ui
            .button("+ New slot")
            .on_hover_text("Add an empty pattern as long as the region")
            .clicked()
    {
        edits.push(Edit::Apply(new_slot(project, id, &region)));
    }
    let Some(&current) = slots.get(playing.min(slots.len().saturating_sub(1))) else {
        ui.label(
            RichText::new("Add a slot, or copy a MIDI clip here from its menu on the timeline.")
                .weak(),
        );
        return;
    };
    if let Some(clip) = project.clip(current) {
        step_grid(ui, id, current, clip, &region, edits);
    }
}

/// The step grid of the slot `clip_id`: a row for each key, a cell for each
/// step of the region. Click a cell to put a note there or take it away; drag
/// up and down on a note to set its velocity. Each cell is a button named
/// "Step N on key K".
fn step_grid(
    ui: &mut Ui,
    node: NodeId,
    clip_id: ClipId,
    clip: &Clip,
    region: &Region,
    edits: &mut Vec<Edit>,
) {
    let Some(midi) = clip.as_midi() else { return };
    let extra_id = egui::Id::new((node, "grid rows"));
    let mut extra: Vec<u8> = ui.data(|d| d.get_temp(extra_id)).unwrap_or_default();
    let keys = rows(midi, region, &extra);
    ui.label(RichText::new("Step grid").strong());
    let steps = region.steps.min(64);
    let size = vec2(steps as f32 * CELL + 30.0, keys.len().max(1) as f32 * CELL);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let painter = ui.painter_at(rect);
    let origin = rect.min + vec2(30.0, 0.0);
    // One bright cell in four, to read the beat by.
    let per_beat = (960 / region.step_ticks.max(1)).max(1) as usize;
    for (row, &key) in keys.iter().enumerate() {
        let top = origin.y + row as f32 * CELL;
        painter.text(
            pos2(rect.left(), top + CELL / 2.0),
            egui::Align2::LEFT_CENTER,
            key.to_string(),
            egui::FontId::monospace(10.0),
            ui.visuals().weak_text_color(),
        );
        for step in 0..steps {
            let cell =
                Rect::from_min_size(pos2(origin.x + step as f32 * CELL, top), vec2(CELL, CELL))
                    .shrink(1.0);
            let beat = step % per_beat.max(1) == 0;
            painter.rect_filled(
                cell,
                2.0,
                if beat {
                    theme::editor::BODY.gamma_multiply(1.6)
                } else {
                    theme::editor::BODY
                },
            );
            let found = note_at(midi, region, key, step);
            if let Some(i) = found {
                let height = cell.height() * midi.notes[i].velocity.clamp(0.1, 1.0);
                painter.rect_filled(
                    Rect::from_min_max(pos2(cell.left(), cell.bottom() - height), cell.max),
                    2.0,
                    Color32::from_rgb(120, 190, 255),
                );
            }
            let response = ui.interact(
                cell,
                ui.id().with(("step cell", node, key, step)),
                Sense::click_and_drag(),
            );
            response.widget_info(|| {
                egui::WidgetInfo::labeled(
                    egui::WidgetType::Button,
                    true,
                    format!("Step {} on key {key}", step + 1),
                )
            });
            if response.clicked() {
                let mut edited = midi.clone();
                toggle(&mut edited, region, key, step);
                edits.push(Edit::Apply(Command::SetClip {
                    id: clip_id,
                    clip: with_midi(clip, edited),
                }));
            }
            if let (true, Some(i), Some(p)) =
                (response.dragged(), found, response.interact_pointer_pos())
            {
                let velocity = ((cell.bottom() - p.y) / cell.height()).clamp(0.05, 1.0);
                let mut edited = midi.clone();
                edited.notes[i].velocity = velocity;
                edits.push(Edit::Drag(Command::SetClip {
                    id: clip_id,
                    clip: with_midi(clip, edited),
                }));
            }
            if response.drag_stopped() {
                edits.push(Edit::EndDrag);
            }
        }
    }
    ui.horizontal(|ui| {
        let mut key = ui
            .data(|d| d.get_temp::<u8>(extra_id.with("key")))
            .unwrap_or(36);
        ui.label("Row key");
        ui.add(egui::DragValue::new(&mut key).range(0..=127));
        ui.data_mut(|d| d.insert_temp(extra_id.with("key"), key));
        if ui.button("+ Row").clicked() && !extra.contains(&key) {
            extra.push(key);
            ui.data_mut(|d| d.insert_temp(extra_id, extra.clone()));
        }
        if let Some(triplet) = triplet_of(region.division)
            && ui
                .button("To triplets")
                .on_hover_text(
                    "Move every note from the straight grid to the triplet one, step for \
                     step, and set the step to match",
                )
                .clicked()
        {
            edits.push(Edit::Apply(Command::Batch(vec![
                Command::SetClip {
                    id: clip_id,
                    clip: with_midi(clip, convert_to_triplets(midi)),
                },
                Command::SetParam {
                    node,
                    key: "step".into(),
                    value: Some(triplet as f32),
                },
            ])));
        }
    });
}

fn with_midi(clip: &Clip, midi: MidiClip) -> Clip {
    Clip {
        content: ClipContent::Midi(midi),
        ..clip.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn region() -> Region {
        Region {
            start: 0,
            steps: 16,
            division: 4,
            step_ticks: 240,
        }
    }

    #[test]
    fn toggling_a_cell_adds_and_removes_the_note() {
        let mut midi = MidiClip::new(Tick(0));
        toggle(&mut midi, &region(), 36, 4);
        assert_eq!(midi.notes.len(), 1);
        assert_eq!(midi.notes[0].start, Tick(960));
        assert_eq!(midi.notes[0].length, Tick(120));
        // The clip grew to hold the region.
        assert_eq!(midi.length, Tick(3840));
        assert_eq!(note_at(&midi, &region(), 36, 4), Some(0));
        toggle(&mut midi, &region(), 36, 4);
        assert!(midi.notes.is_empty());
    }

    #[test]
    fn rows_are_the_keys_in_use_highest_first() {
        let mut midi = MidiClip::new(Tick(3840));
        toggle(&mut midi, &region(), 36, 0);
        toggle(&mut midi, &region(), 42, 2);
        assert_eq!(rows(&midi, &region(), &[38, 36]), [42, 38, 36]);
    }

    #[test]
    fn triplets_keep_each_note_on_its_step() {
        let mut midi = MidiClip::new(Tick(3840));
        toggle(&mut midi, &region(), 36, 3);
        let triplet = convert_to_triplets(&midi);
        assert_eq!(triplet.length, Tick(2560));
        // Step 3 of the sixteenth-triplet grid (160 ticks).
        assert_eq!(triplet.notes[0].start, Tick(480));
        assert_eq!(triplet.notes[0].length, Tick(80));
        assert_eq!(CLOCK_DIVISIONS[triplet_of(4).unwrap()], "1/16T");
        assert_eq!(triplet_of(9), None);
    }
}
