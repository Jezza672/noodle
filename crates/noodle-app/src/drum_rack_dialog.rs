//! Create drum rack from clip: the dialog (design:
//! docs/design/drum-rack-from-clip.md).
//!
//! While the analysis runs it shows a progress bar and Cancel. When the hits
//! are found it shows their **bins** (groups of similar hits, each with a best
//! guess at what it is) on one side and a **pad grid** on the other. A bin is
//! dragged onto a pad (the same bin may go on several), a pad with a bin of
//! several hits has a picker for which one it plays, and Create makes the rack
//! as one undo step. The user's choices live here; the session holds only the
//! analysis, so cancelling changes nothing.

use std::collections::BTreeMap;

use egui::{Id, Ui};
use noodle_core::ClipId;
use noodle_io::{
    Binning, DEFAULT_MERGE, DrumBin, DrumKind, MAX_LAYERS, bin_hits, layer_split, make_bin,
    split_bin,
};

use crate::models::Models;
use crate::session::drum_rack::{DrumAnalysis, DrumRackPlan, PadAssignment, PlannedBin, pad_name};
use crate::session::{Edit, Session};
use noodle_stems::drum::DrumPart;

/// The pads the grid starts with: the keys of the Drum Rack template's pads.
pub const COMMON_KEYS: [u8; 8] = [36, 38, 42, 46, 39, 45, 50, 49];

/// The pad a bin guessed to be a kind of drum is first dropped on.
fn default_key(kind: DrumKind) -> Option<u8> {
    Some(match kind {
        DrumKind::Kick => 36,
        DrumKind::Snare => 38,
        DrumKind::ClosedHat => 42,
        DrumKind::OpenHat => 46,
        DrumKind::Clap => 39,
        DrumKind::LowTom => 45,
        DrumKind::HighTom => 50,
        DrumKind::Crash => 49,
        DrumKind::Unknown => return None,
    })
}

/// The height of the two lists, fixed so the dialog does not change size as it fills.
const PANE_HEIGHT: f32 = 320.0;

/// A bin on a pad, and which of its hits the pad plays (from 1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Placed {
    bin: usize,
    pick: usize,
}

/// A group being dragged to a pad.
#[derive(Clone, Copy)]
struct BinDrag(usize);

/// A hit being dragged to a group.
#[derive(Clone, Copy)]
struct HitDrag(usize);

/// Where a hit goes when it is moved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Target {
    Bin(usize),
    NewBin,
    Other,
}

/// What a click or drop in the dialog asks for, done once the frame is drawn.
enum Action {
    Place(u8, usize),
    Clear(u8),
    Pick(u8, usize),
    MoveHit(usize, Target),
    Merge(usize, usize),
    Split(usize),
    Layers(usize, u8),
    Play(usize),
    Undo,
    /// Run the separation model of this id.
    Separate(&'static str),
    CancelSeparation,
    /// Play a separated part of a hit instead of the hit, or the hit again.
    TakePart(usize, Option<DrumPart>),
}

struct State {
    clip: ClipId,
    merge: f32,
    binning: Binning,
    pads: BTreeMap<u8, Placed>,
    show_all: bool,
    /// Also add the groove as a MIDI clip.
    groove: bool,
    /// The separation model chosen in the drop-down: the id of an enabled,
    /// downloaded one, or `None` when there is none.
    model: Option<&'static str>,
    /// Whether what the separation found has been put into the groups.
    separation_applied: bool,
    /// The bins and pads before each manual edit (move, split, merge), newest
    /// last. The slider throws them away with the edits.
    undo: Vec<(Binning, BTreeMap<u8, Placed>)>,
}

impl State {
    fn new(analysis: &DrumAnalysis) -> Self {
        let mut state = Self {
            clip: analysis.clip,
            merge: DEFAULT_MERGE,
            binning: bin_hits(&analysis.hits, DEFAULT_MERGE),
            pads: BTreeMap::new(),
            show_all: false,
            groove: false,
            model: None,
            separation_applied: false,
            undo: Vec::new(),
        };
        state.guess_pads();
        state
    }

    /// Re-bins at the slider's setting, which throws the pads away: the bins
    /// they point at are no longer there.
    fn rebin(&mut self, analysis: &DrumAnalysis) {
        self.binning = bin_hits(&analysis.hits, self.merge);
        self.separation_applied = false;
        self.undo.clear();
        self.guess_pads();
    }

    /// Takes in what the separation found: a hit with two parts sounding is
    /// mixed, so it ranks last in its group and is never the default pick.
    fn apply_separation(&mut self, analysis: &DrumAnalysis) {
        self.separation_applied = true;
        let mut changed = false;
        for hit in 0..analysis.hits.len() {
            if analysis.separated_mixed(hit) && !self.binning.mixed[hit] {
                self.binning.mixed[hit] = true;
                changed = true;
            }
        }
        if changed {
            for bin in 0..self.binning.bins.len() {
                let members = self.binning.bins[bin].members.clone();
                self.reset_bin(analysis, bin, members);
            }
        }
    }

    fn remember(&mut self) {
        self.undo.push((self.binning.clone(), self.pads.clone()));
    }

    fn undo(&mut self) {
        if let Some((binning, pads)) = self.undo.pop() {
            self.binning = binning;
            self.pads = pads;
        }
    }

    /// The group a hit is in (`None` for Other).
    fn bin_of(&self, hit: usize) -> Option<usize> {
        self.binning
            .bins
            .iter()
            .position(|b| b.members.contains(&hit))
    }

    /// Puts `bin` back together from `members`, which re-ranks them. A pad on
    /// it starts again from the best hit, since the slots moved.
    fn reset_bin(&mut self, analysis: &DrumAnalysis, bin: usize, members: Vec<usize>) {
        let layers = self.binning.bins[bin].layers;
        let mut remade = make_bin(&analysis.hits, &self.binning.mixed, members);
        remade.layers = layers.min(remade.members.len().min(usize::from(MAX_LAYERS)) as u8);
        self.binning.bins[bin] = remade;
        for p in self.pads.values_mut().filter(|p| p.bin == bin) {
            p.pick = 1;
        }
    }

    /// Takes group `gone` out, and the pads on it (or onto `into`).
    fn remove_bin(&mut self, gone: usize, into: Option<usize>) {
        self.binning.bins.remove(gone);
        let fix = |b: usize| if b > gone { b - 1 } else { b };
        self.pads = std::mem::take(&mut self.pads)
            .into_iter()
            .filter_map(|(key, p)| {
                let placed = if p.bin == gone {
                    Placed {
                        bin: fix(into?),
                        pick: 1,
                    }
                } else {
                    Placed {
                        bin: fix(p.bin),
                        pick: p.pick,
                    }
                };
                Some((key, placed))
            })
            .collect();
    }

    /// Moves a hit to another group, a new one of its own, or Other.
    fn move_hit(&mut self, analysis: &DrumAnalysis, hit: usize, to: Target) {
        let from = self.bin_of(hit);
        if from.map(Target::Bin).unwrap_or(Target::Other) == to {
            return;
        }
        if matches!(to, Target::Bin(bin) if bin >= self.binning.bins.len()) {
            return;
        }
        self.remember();
        match to {
            Target::Bin(bin) => {
                let mut members = self.binning.bins[bin].members.clone();
                members.push(hit);
                self.reset_bin(analysis, bin, members);
            }
            Target::NewBin => {
                let bin = make_bin(&analysis.hits, &self.binning.mixed, vec![hit]);
                self.binning.bins.push(bin);
            }
            Target::Other => self.binning.other.push(hit),
        }
        self.binning
            .other
            .retain(|&h| h != hit || to == Target::Other);
        if to == Target::Other {
            self.binning.other.sort_unstable();
            self.binning.other.dedup();
        }
        if let Some(from) = from {
            let rest: Vec<usize> = self.binning.bins[from]
                .members
                .iter()
                .copied()
                .filter(|&m| m != hit)
                .collect();
            if rest.is_empty() {
                self.remove_bin(from, None);
            } else {
                self.reset_bin(analysis, from, rest);
            }
        }
    }

    /// Folds group `from` into `into`; its pads follow.
    fn merge_bins(&mut self, analysis: &DrumAnalysis, from: usize, into: usize) {
        if from == into || from >= self.binning.bins.len() || into >= self.binning.bins.len() {
            return;
        }
        self.remember();
        let mut members = self.binning.bins[into].members.clone();
        members.extend(&self.binning.bins[from].members);
        self.reset_bin(analysis, into, members);
        self.remove_bin(from, Some(into));
    }

    /// Splits a group into the sub-groups its hits fall apart into. False when
    /// they are too alike.
    fn split_bin(&mut self, analysis: &DrumAnalysis, bin: usize) -> bool {
        let Some(parts) = split_bin(
            &analysis.hits,
            &self.binning.mixed,
            &self.binning.bins[bin].members,
            self.merge,
        ) else {
            return false;
        };
        self.remember();
        let mut parts: Vec<DrumBin> = parts;
        let first = parts.remove(0);
        self.binning.bins[bin] = first;
        self.binning.bins.extend(parts);
        for p in self.pads.values_mut().filter(|p| p.bin == bin) {
            p.pick = 1;
        }
        true
    }

    /// The velocity layers of `bin`, softest first: its members split by level,
    /// or all of them as one when it is not layered.
    fn layers_of(&self, analysis: &DrumAnalysis, bin: usize) -> Vec<Vec<usize>> {
        let b = &self.binning.bins[bin];
        layer_split(&analysis.hits, &b.members, b.layers)
    }

    /// How many hits a pad can choose between on `bin`: the most any layer has.
    fn slots(&self, analysis: &DrumAnalysis, bin: usize) -> usize {
        self.layers_of(analysis, bin)
            .iter()
            .map(Vec::len)
            .max()
            .unwrap_or(1)
    }

    /// The hit a pad on `bin` plays when it picks `pick`: of the loudest layer,
    /// which is what a hard note on the pad plays.
    fn pad_hit(&self, analysis: &DrumAnalysis, bin: usize, pick: usize) -> usize {
        let layers = self.layers_of(analysis, bin);
        let top = layers.last().expect("a bin has hits");
        top[pick.clamp(1, top.len()) - 1]
    }

    /// Offers `bin`'s hits as `layers` velocity layers (1 turns them off).
    fn set_layers(&mut self, bin: usize, layers: u8) {
        let most = self.binning.bins[bin]
            .members
            .len()
            .min(usize::from(MAX_LAYERS)) as u8;
        let layers = layers.clamp(1, most.max(1));
        if self.binning.bins[bin].layers == layers {
            return;
        }
        self.remember();
        self.binning.bins[bin].layers = layers;
        for p in self.pads.values_mut().filter(|p| p.bin == bin) {
            p.pick = 1;
        }
    }

    /// What names the drums a hit is made of, for a mixed hit: "Kick + Closed
    /// hat". Empty when none of its parts is still a group.
    fn parts_of(&self, hit: usize) -> String {
        let mut names: Vec<String> = self
            .binning
            .parts
            .get(hit)
            .into_iter()
            .flatten()
            .filter_map(|&medoid| self.bin_of(medoid))
            .map(|bin| self.bin_name(bin))
            .collect();
        names.dedup();
        names.join(" + ")
    }

    /// Drops each bin on the pad its guess names, the first bin of a kind
    /// winning the pad.
    fn guess_pads(&mut self) {
        self.pads.clear();
        for (bin, b) in self.binning.bins.iter().enumerate() {
            if let Some(key) = default_key(b.kind)
                && !self.pads.contains_key(&key)
            {
                self.pads.insert(key, Placed { bin, pick: 1 });
            }
        }
    }

    /// The bin's name: its guess, numbered when two share one.
    fn bin_name(&self, bin: usize) -> String {
        let kind = self.binning.bins[bin].kind;
        let base = if kind == DrumKind::Unknown {
            "Bin"
        } else {
            kind.name()
        };
        let same = |i: usize| {
            let k = self.binning.bins[i].kind;
            (k == kind && k != DrumKind::Unknown) || (kind == DrumKind::Unknown && k == kind)
        };
        let total = (0..self.binning.bins.len()).filter(|&i| same(i)).count();
        if total <= 1 && kind != DrumKind::Unknown {
            return base.to_owned();
        }
        let n = (0..=bin).filter(|&i| same(i)).count();
        format!("{base} {n}")
    }

    /// What the dialog's choices make: the bins in use and the pads.
    fn plan(&self, analysis_hits: &[noodle_io::Hit]) -> DrumRackPlan {
        let mut used: Vec<usize> = self.pads.values().map(|p| p.bin).collect();
        used.sort_unstable();
        used.dedup();
        let bins = used
            .iter()
            .map(|&bin| {
                let b = &self.binning.bins[bin];
                // Each hit's layer, in the order of the members.
                let layers = if b.layers > 1 {
                    let split = layer_split(analysis_hits, &b.members, b.layers);
                    b.members
                        .iter()
                        .map(|m| split.iter().position(|l| l.contains(m)).unwrap_or(0) as u8 + 1)
                        .collect()
                } else {
                    Vec::new()
                };
                PlannedBin {
                    name: self.bin_name(bin),
                    hits: b.members.clone(),
                    layers,
                }
            })
            .collect();
        let pads = self
            .pads
            .iter()
            .map(|(&key, p)| PadAssignment {
                key,
                bin: used.iter().position(|&b| b == p.bin).expect("a used bin"),
                pick: p.pick,
            })
            .collect();
        DrumRackPlan {
            bins,
            pads,
            groove: self.groove,
        }
    }
}

/// The dialog's state, kept by the app between frames.
#[derive(Default)]
pub struct DrumRackDialog {
    state: Option<State>,
}

impl DrumRackDialog {
    pub fn show(&mut self, ctx: &egui::Context, session: &mut Session, models: &mut Models) {
        let active = session.drum_progress().is_some() || session.drum_analysis().is_some();
        // The first time the dialog is wanted, offer the model downloads. The
        // dialog follows once that is answered.
        if active && models.first_use_due() {
            models.show_first_use(ctx);
            return;
        }
        if let Some(fraction) = session.drum_progress() {
            self.state = None;
            let mut cancel = false;
            let modal = crate::dialog::modal(ctx, "drum rack progress", |ui| {
                ui.set_max_width(360.0);
                ui.heading("Create drum rack");
                ui.add_space(6.0);
                ui.label("Listening for drum hits…");
                ui.add(
                    egui::ProgressBar::new(fraction)
                        .desired_width(320.0)
                        .show_percentage(),
                );
                ui.add_space(8.0);
                cancel = ui.button("Cancel").clicked();
            });
            if cancel || modal.should_close() {
                session.edit([Edit::CancelDrumAnalysis]);
            }
            // The bar moves without anything else repainting.
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
            return;
        }
        let Some(analysis) = session.drum_analysis().cloned() else {
            self.state = None;
            return;
        };
        if self.state.as_ref().is_none_or(|s| s.clip != analysis.clip) {
            self.state = Some(State::new(&analysis));
        }
        let state = self.state.as_mut().expect("just made");
        if analysis.separation.is_some() && !state.separation_applied {
            state.apply_separation(&analysis);
        }
        let ready = models.ready();
        if state
            .model
            .is_none_or(|id| !ready.iter().any(|m| m.id == id))
        {
            state.model = ready.first().map(|m| m.id);
        }
        let mut answer = None;
        let modal = crate::dialog::modal(ctx, "drum rack", |ui| {
            ui.set_width(640.0);
            answer = body(ui, session, &analysis, state, models);
        });
        if modal.should_close() {
            answer.get_or_insert(false);
        }
        match answer {
            Some(true) => {
                let plan = state.plan(&analysis.hits);
                let clip = state.clip;
                session.edit([Edit::CreateDrumRack(clip, plan)]);
                session.close_drum_analysis();
                self.state = None;
            }
            Some(false) => {
                session.close_drum_analysis();
                self.state = None;
            }
            None => {}
        }
    }
}

/// The drop-down of separation models: only the enabled, downloaded ones. With
/// none, it says where to switch one on.
fn model_choice(
    ui: &mut Ui,
    session: &Session,
    analysis: &DrumAnalysis,
    state: &mut State,
    models: &Models,
    actions: &mut Vec<Action>,
) {
    let ready = models.ready();
    ui.horizontal(|ui| {
        ui.label("Separation model");
        let name = |id: Option<&str>| {
            ready
                .iter()
                .find(|m| Some(m.id) == id)
                .map_or("None enabled", |m| m.name)
        };
        egui::ComboBox::from_id_salt("drum model")
            .selected_text(name(state.model))
            .show_ui(ui, |ui| {
                for m in &ready {
                    ui.selectable_value(&mut state.model, Some(m.id), m.name);
                }
            })
            .response
            .widget_info(|| {
                egui::WidgetInfo::labeled(egui::WidgetType::ComboBox, true, "Separation model")
            });
        if ready.is_empty() {
            let downloading = noodle_stems::drum::ALL_MODELS
                .iter()
                .any(|m| models.is_downloading(m));
            ui.weak(if downloading {
                "Downloading… (Settings, Models)"
            } else {
                "Enable and download one in Settings, on the Models tab"
            });
        }
        if let Some(fraction) = session.drum_separation_progress() {
            ui.add(
                egui::ProgressBar::new(fraction)
                    .desired_width(140.0)
                    .show_percentage(),
            );
            if ui.button("Cancel").clicked() {
                actions.push(Action::CancelSeparation);
            }
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(100));
        } else if let Some(separation) = &analysis.separation {
            let mixed = (0..analysis.hits.len())
                .filter(|&h| analysis.separated_mixed(h))
                .count();
            ui.weak(format!(
                "Separated with {}: {mixed} mixed hits",
                ready
                    .iter()
                    .find(|m| m.id == separation.model.id)
                    .map_or(separation.model.id.as_str(), |m| m.name)
            ));
        } else if let Some(id) = state.model {
            let run = ui
                .button("Find overlapping drums")
                .on_hover_text(
                    "Separates the clip into kick, snare, hats and so on, and marks hits                      where two sound together. Slow on a long clip.",
                );
            if run.clicked() {
                actions.push(Action::Separate(id));
            }
        }
    });
}

/// Draws the dialog; returns true for Create, false for Cancel.
fn body(
    ui: &mut Ui,
    session: &mut Session,
    analysis: &DrumAnalysis,
    state: &mut State,
    models: &Models,
) -> Option<bool> {
    let mut answer = None;
    let mut actions: Vec<Action> = Vec::new();
    ui.heading("Create drum rack");
    ui.label(
        egui::RichText::new(format!(
            "{} hits found in {} groups. Drag a group onto a pad, or use its Place on… menu.",
            analysis.hits.len(),
            state.binning.bins.len()
        ))
        .weak(),
    );
    ui.add_space(4.0);
    model_choice(ui, session, analysis, state, models, &mut actions);
    ui.horizontal(|ui| {
        ui.label("Merge more");
        let slider = ui.add(egui::Slider::new(&mut state.merge, 0.0..=1.0).show_value(false));
        slider.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Slider, true, "Merge more / Split more")
        });
        ui.label("Split more");
        if slider.changed() {
            state.rebin(analysis);
        }
        let undo = ui
            .add_enabled(!state.undo.is_empty(), egui::Button::new("Undo"))
            .on_hover_text("Takes back the last edit of the groups or pads");
        if undo.clicked() {
            actions.push(Action::Undo);
        }
    });
    ui.separator();
    ui.columns(2, |cols| {
        bins(&mut cols[0], session, analysis, state, &mut actions);
        pads(&mut cols[1], analysis, state, &mut actions);
    });
    ui.separator();
    ui.checkbox(&mut state.groove, "Also add the groove as a MIDI clip")
        .on_hover_text(
            "Adds a MIDI clip beside the rack with a note for every hit that is on a pad, \
             at its time, as loud as the hit was",
        );
    ui.horizontal(|ui| {
        let create = ui
            .add_enabled(!state.pads.is_empty(), egui::Button::new("Create"))
            .on_hover_text("Makes a new track with the rack");
        if create.clicked() {
            answer = Some(true);
        }
        if ui.button("Cancel").clicked() {
            answer = Some(false);
        }
    });
    for action in actions {
        match action {
            Action::Place(key, bin) => {
                state.remember();
                state.pads.insert(key, Placed { bin, pick: 1 });
            }
            Action::Clear(key) => {
                state.remember();
                state.pads.remove(&key);
            }
            Action::Pick(key, pick) => {
                state.remember();
                if let Some(p) = state.pads.get_mut(&key) {
                    p.pick = pick;
                }
            }
            Action::MoveHit(hit, to) => state.move_hit(analysis, hit, to),
            Action::Merge(from, into) => state.merge_bins(analysis, from, into),
            Action::Split(bin) => {
                if !state.split_bin(analysis, bin) {
                    session.notify("Those hits sound too alike to split".into());
                }
            }
            Action::Layers(bin, n) => state.set_layers(bin, n),
            Action::Play(hit) => session.audition_drum_hit(hit),
            Action::Undo => state.undo(),
            Action::Separate(id) => session.edit([Edit::SeparateDrumHits(id.to_owned())]),
            Action::CancelSeparation => session.edit([Edit::CancelDrumSeparation]),
            Action::TakePart(hit, part) => session.edit([Edit::TakeSeparatedPart(hit, part)]),
        }
    }
    answer
}

/// The pads a Place on… menu offers: the grid as it is shown.
fn visible_keys(state: &State) -> Vec<u8> {
    if state.show_all {
        (0..=127).collect()
    } else {
        COMMON_KEYS.to_vec()
    }
}

fn bins(
    ui: &mut Ui,
    session: &Session,
    analysis: &DrumAnalysis,
    state: &State,
    actions: &mut Vec<Action>,
) {
    ui.label(egui::RichText::new("Groups").strong());
    let keys = visible_keys(state);
    egui::ScrollArea::vertical()
        .id_salt("drum bins")
        .auto_shrink([false, false])
        .max_height(PANE_HEIGHT)
        .show(ui, |ui| {
            for (i, bin) in state.binning.bins.iter().enumerate() {
                let name = state.bin_name(i);
                let (_, dropped) =
                    ui.dnd_drop_zone::<HitDrag, _>(egui::Frame::group(ui.style()), |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            ui.dnd_drag_source(Id::new(("drum bin", i)), BinDrag(i), |ui| {
                                ui.label("⠿").on_hover_text("Drag onto a pad");
                            });
                            ui.label(egui::RichText::new(&name).strong());
                            let sure = if bin.kind == DrumKind::Unknown {
                                "not sure".to_owned()
                            } else {
                                format!("{:.0}% sure", bin.confidence * 100.0)
                            };
                            ui.label(
                                egui::RichText::new(format!("{} hits · {sure}", bin.members.len()))
                                    .weak()
                                    .small(),
                            );
                        });
                        medoid_waveform(ui, session, analysis, bin.medoid);
                        ui.horizontal(|ui| {
                            if ui
                                .small_button("▶ Play")
                                .on_hover_text("Plays the group's best hit on the monitor output")
                                .clicked()
                            {
                                actions.push(Action::Play(bin.members[0]));
                            }
                            ui.menu_button("Place on…", |ui| {
                                for &key in &keys {
                                    let label = match state.pads.get(&key) {
                                        Some(p) => format!(
                                            "Place on {} (now {})",
                                            pad_name(key),
                                            state.bin_name(p.bin)
                                        ),
                                        None => format!("Place on {}", pad_name(key)),
                                    };
                                    if ui.button(label).clicked() {
                                        actions.push(Action::Place(key, i));
                                        ui.close();
                                    }
                                }
                            });
                            if bin.members.len() > 1
                                && ui
                                    .small_button("Split")
                                    .on_hover_text("Breaks the group into the sounds it holds")
                                    .clicked()
                            {
                                actions.push(Action::Split(i));
                            }
                            if state.binning.bins.len() > 1 {
                                ui.menu_button("Merge with…", |ui| {
                                    for j in (0..state.binning.bins.len()).filter(|&j| j != i) {
                                        if ui.button(state.bin_name(j)).clicked() {
                                            actions.push(Action::Merge(j, i));
                                            ui.close();
                                        }
                                    }
                                });
                            }
                        });
                        if bin.members.len() > 1 {
                            ui.horizontal(|ui| {
                                ui.label("Velocity layers")
                                    .on_hover_text("Offers soft and hard hits as layers, so how hard a note is played picks the hit");
                                let most = bin.members.len().min(usize::from(MAX_LAYERS)) as u8;
                                for n in 1..=most {
                                    if ui
                                        .selectable_label(bin.layers == n, n.to_string())
                                        .clicked()
                                    {
                                        actions.push(Action::Layers(i, n));
                                    }
                                }
                                if bin.layers > 1 {
                                    let sizes: Vec<String> = state
                                        .layers_of(analysis, i)
                                        .iter()
                                        .zip(layer_names(bin.layers))
                                        .map(|(l, name)| format!("{} {name}", l.len()))
                                        .collect();
                                    ui.label(egui::RichText::new(sizes.join(" · ")).weak().small());
                                }
                            });
                        }
                        egui::CollapsingHeader::new(format!("Hits ({})", bin.members.len()))
                            .id_salt(("drum bin hits", i))
                            .show(ui, |ui| {
                                for &hit in &bin.members {
                                    hit_row(ui, state, analysis, hit, Some(i), actions);
                                }
                            });
                    });
                if let Some(hit) = dropped {
                    actions.push(Action::MoveHit(hit.0, Target::Bin(i)));
                }
            }
            if !state.binning.other.is_empty() {
                egui::CollapsingHeader::new(format!(
                    "Other ({}): quiet, cut-short or moved-out hits",
                    state.binning.other.len()
                ))
                .id_salt("drum other hits")
                .show(ui, |ui| {
                    for &hit in &state.binning.other {
                        hit_row(ui, state, analysis, hit, None, actions);
                    }
                });
            }
        });
}

/// One hit in a group's list: drag it to another group, or use Move to….
fn hit_row(
    ui: &mut Ui,
    state: &State,
    analysis: &DrumAnalysis,
    hit: usize,
    from: Option<usize>,
    actions: &mut Vec<Action>,
) {
    let mut note = hit_note(state, analysis, hit);
    if let Some(bin) = from
        && state.binning.bins[bin].layers > 1
    {
        let layers = state.layers_of(analysis, bin);
        if let Some(at) = layers.iter().position(|l| l.contains(&hit)) {
            note.push_str(&format!(" · {}", layer_names(layers.len() as u8)[at]));
        }
    }
    ui.horizontal(|ui| {
        ui.dnd_drag_source(Id::new(("drum hit", hit)), HitDrag(hit), |ui| {
            ui.label("⠿").on_hover_text("Drag onto a group");
        });
        ui.label(format!(
            "Hit {} · {:.0} dB{note}",
            hit + 1,
            analysis.hits[hit].level_db
        ));
        if ui
            .small_button("▶")
            .on_hover_text("Play this hit")
            .clicked()
        {
            actions.push(Action::Play(hit));
        }
        if let Some(found) = analysis
            .separation
            .as_ref()
            .and_then(|s| s.hits.get(hit))
            .filter(|h| h.is_mixed())
        {
            ui.menu_button("Use part…", |ui| {
                for &part in &found.active {
                    if ui.button(format!("Use the {part} part")).clicked() {
                        actions.push(Action::TakePart(hit, Some(part)));
                        ui.close();
                    }
                }
                if analysis.is_taken(hit) && ui.button("Use the original hit").clicked() {
                    actions.push(Action::TakePart(hit, None));
                    ui.close();
                }
            })
            .response
            .on_hover_text(
                "Plays one drum of this hit, as the separator heard it, instead of the whole hit",
            );
        }
        ui.menu_button("Move to…", |ui| {
            for j in 0..state.binning.bins.len() {
                if Some(j) != from && ui.button(state.bin_name(j)).clicked() {
                    actions.push(Action::MoveHit(hit, Target::Bin(j)));
                    ui.close();
                }
            }
            if ui.button("A new group").clicked() {
                actions.push(Action::MoveHit(hit, Target::NewBin));
                ui.close();
            }
            if from.is_some() && ui.button("Other (leave out)").clicked() {
                actions.push(Action::MoveHit(hit, Target::Other));
                ui.close();
            }
        });
    });
}

/// What is odd about a hit, for its row: the drums a mixed hit is made of, or
/// that it was cut short.
fn hit_note(state: &State, analysis: &DrumAnalysis, hit: usize) -> String {
    let separated = analysis
        .separation
        .as_ref()
        .and_then(|s| s.hits.get(hit))
        .filter(|h| h.is_mixed());
    let mut note = if let Some(found) = separated {
        let parts: Vec<&str> = found.active.iter().map(|p| p.label()).collect();
        format!(" (mixed: {})", parts.join(" + "))
    } else if state.binning.mixed.get(hit).copied().unwrap_or(false) {
        match state.parts_of(hit) {
            parts if parts.is_empty() => " (mixed)".to_owned(),
            parts => format!(" (mixed: {parts})"),
        }
    } else if analysis.hits[hit].truncated {
        " (cut short)".to_owned()
    } else {
        String::new()
    };
    if analysis.is_taken(hit) {
        note.push_str(" · plays the separated part");
    }
    note
}

/// What a velocity layer is called, softest first.
fn layer_names(layers: u8) -> Vec<&'static str> {
    match layers {
        0 | 1 => vec!["hits"],
        2 => vec!["soft", "hard"],
        _ => vec!["soft", "medium", "hard"],
    }
}

fn medoid_waveform(ui: &mut Ui, session: &Session, analysis: &DrumAnalysis, hit: usize) {
    let Some(window) = analysis.window(hit) else {
        return;
    };
    let Some(sample) = session.samples().get(&window.hash) else {
        return;
    };
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 32.0), egui::Sense::hover());
    crate::sample_view::draw_preview(
        ui,
        rect,
        &window.hash,
        &sample,
        crate::sample_view::Region {
            start: window.start,
            end: window.end,
            looping: None,
        },
    );
}

fn pads(ui: &mut Ui, analysis: &DrumAnalysis, state: &mut State, actions: &mut Vec<Action>) {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Pads").strong());
        ui.checkbox(&mut state.show_all, "Show all keys");
    });
    let keys = visible_keys(state);
    egui::ScrollArea::vertical()
        .id_salt("drum pads")
        .auto_shrink([false, false])
        .max_height(PANE_HEIGHT)
        .show(ui, |ui| {
            for key in keys {
                let placed = state.pads.get(&key).copied();
                let label = format!("{} ({})", pad_name(key), crate::piano_roll::key_name(key));
                let (_, dropped) =
                    ui.dnd_drop_zone::<BinDrag, _>(egui::Frame::group(ui.style()), |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new(label).strong());
                            match placed {
                                None => {
                                    ui.label(egui::RichText::new("empty").weak());
                                }
                                Some(p) => {
                                    ui.label(state.bin_name(p.bin));
                                    let slots = state.slots(analysis, p.bin);
                                    if ui
                                        .small_button("▶")
                                        .on_hover_text("Plays the hit this pad plays")
                                        .clicked()
                                    {
                                        actions.push(Action::Play(
                                            state.pad_hit(analysis, p.bin, p.pick),
                                        ));
                                    }
                                    if slots > 1
                                        && let Some(pick) = picker(ui, state, analysis, p, slots)
                                    {
                                        actions.push(Action::Pick(key, pick));
                                    }
                                    if ui
                                        .small_button("×")
                                        .on_hover_text("Empty the pad")
                                        .clicked()
                                    {
                                        actions.push(Action::Clear(key));
                                    }
                                }
                            }
                        });
                    });
                if let Some(bin) = dropped {
                    actions.push(Action::Place(key, bin.0));
                }
            }
        });
}

/// The row choosing which hit of a bin a pad plays (of each layer, when it
/// has them): previous, which, next. Returns the new pick (from 1) when it was
/// changed.
fn picker(
    ui: &mut Ui,
    state: &State,
    analysis: &DrumAnalysis,
    placed: Placed,
    slots: usize,
) -> Option<usize> {
    let pick = placed.pick.clamp(1, slots);
    let hit = state.pad_hit(analysis, placed.bin, pick);
    let note = hit_note(state, analysis, hit);
    let mut new = pick;
    if ui.small_button("◀").on_hover_text("Previous hit").clicked() {
        new = if pick == 1 { slots } else { pick - 1 };
    }
    ui.label(format!("{pick}/{slots}{note}"));
    if ui.small_button("▶").on_hover_text("Next hit").clicked() {
        new = if pick == slots { 1 } else { pick + 1 };
    }
    (new != pick).then_some(new)
}

#[cfg(test)]
mod tests {
    use noodle_io::{Hit, HitFeatures};

    use super::*;

    /// A hit whose spectrum slopes down (a kick) or up (a hat).
    fn hit(at: u64, low: bool) -> Hit {
        let bands = std::array::from_fn(|i| {
            let i = if low { i } else { 23 - i };
            -3.0 * i as f32
        });
        Hit {
            onset: at,
            start: at,
            end: at + 5_000,
            truncated: false,
            mixed: false,
            level_db: -10.0 - (at / 10_000) as f32,
            features: HitFeatures {
                bands,
                centroid: if low { 120.0 } else { 9_000.0 },
                flatness: 0.3,
                split: if low {
                    [0.9, 0.1, 0.0]
                } else {
                    [0.0, 0.1, 0.9]
                },
                decay: 0.1,
                pitched: 0.0,
                pitch_key: None,
                attack_peaks: 1,
            },
        }
    }

    /// Kicks and hats alternating: hits 0, 2, 4, 6 are kicks.
    fn analysis() -> DrumAnalysis {
        let hits = (0..8).map(|i| hit(i * 10_000, i % 2 == 0)).collect();
        DrumAnalysis::synthetic(ClipId(1), hits)
    }

    fn two_groups() -> (DrumAnalysis, State) {
        let analysis = analysis();
        let state = State::new(&analysis);
        assert_eq!(state.binning.bins.len(), 2, "{:?}", state.binning);
        (analysis, state)
    }

    fn sorted(bin: &DrumBin) -> Vec<usize> {
        let mut m = bin.members.clone();
        m.sort_unstable();
        m
    }

    #[test]
    fn moving_a_hit_changes_both_groups_and_undo_takes_it_back() {
        let (analysis, mut state) = two_groups();
        let before = state.binning.clone();
        let (kicks, hats) = (state.bin_of(0).unwrap(), state.bin_of(1).unwrap());
        state.move_hit(&analysis, 1, Target::Bin(kicks));
        assert_eq!(sorted(&state.binning.bins[kicks]), [0, 1, 2, 4, 6]);
        assert_eq!(sorted(&state.binning.bins[hats]), [3, 5, 7]);
        state.undo();
        assert_eq!(state.binning, before);
        // To Other, and back out of it into a group.
        state.move_hit(&analysis, 3, Target::Other);
        assert_eq!(state.binning.other, [3]);
        assert_eq!(state.bin_of(3), None);
        state.move_hit(&analysis, 3, Target::Bin(hats));
        assert!(state.binning.other.is_empty());
        assert_eq!(sorted(&state.binning.bins[hats]), [1, 3, 5, 7]);
        // To a group of its own.
        state.move_hit(&analysis, 7, Target::NewBin);
        assert_eq!(state.binning.bins.len(), 3);
        assert_eq!(state.binning.bins[2].members, [7]);
        // Moving a hit where it already is does nothing, and no undo step.
        let steps = state.undo.len();
        state.move_hit(&analysis, 7, Target::Bin(2));
        assert_eq!(state.undo.len(), steps);
    }

    #[test]
    fn moving_the_last_hit_out_removes_the_group_and_its_pads() {
        let (analysis, mut state) = two_groups();
        let kicks = state.bin_of(0).unwrap();
        let hats = state.bin_of(1).unwrap();
        state.pads.insert(
            36,
            Placed {
                bin: kicks,
                pick: 2,
            },
        );
        state.pads.insert(42, Placed { bin: hats, pick: 3 });
        for hit in [1, 3, 5] {
            state.move_hit(&analysis, hit, Target::Bin(kicks));
        }
        state.move_hit(&analysis, 7, Target::Bin(kicks));
        assert_eq!(state.binning.bins.len(), 1);
        assert!(!state.pads.contains_key(&42), "{:?}", state.pads);
        assert_eq!(state.pads[&36].bin, 0);
        assert_eq!(state.binning.bins[0].members.len(), 8);
    }

    #[test]
    fn merging_follows_the_pads_and_splitting_separates_them_again() {
        let (analysis, mut state) = two_groups();
        let kicks = state.bin_of(0).unwrap();
        let hats = state.bin_of(1).unwrap();
        state.pads.insert(
            36,
            Placed {
                bin: kicks,
                pick: 1,
            },
        );
        state.pads.insert(42, Placed { bin: hats, pick: 2 });
        state.merge_bins(&analysis, hats, kicks);
        assert_eq!(state.binning.bins.len(), 1);
        assert_eq!(state.binning.bins[0].members.len(), 8);
        assert_eq!(state.pads[&36].bin, 0);
        assert_eq!(state.pads[&42], Placed { bin: 0, pick: 1 });
        assert!(state.split_bin(&analysis, 0));
        assert_eq!(state.binning.bins.len(), 2);
        let mut groups: Vec<Vec<usize>> = state.binning.bins.iter().map(sorted).collect();
        groups.sort();
        assert_eq!(groups, [vec![0, 2, 4, 6], vec![1, 3, 5, 7]]);
        state.undo();
        assert_eq!(state.binning.bins.len(), 1);
        state.undo();
        assert_eq!(state.binning.bins.len(), 2);
        // The kicks alone are too alike to split.
        let kicks = state.bin_of(0).unwrap();
        let steps = state.undo.len();
        assert!(!state.split_bin(&analysis, kicks));
        assert_eq!(state.undo.len(), steps);
    }

    #[test]
    fn undo_takes_back_pad_edits_and_a_bad_target_leaves_no_step() {
        let (analysis, mut state) = two_groups();
        let kicks = state.bin_of(0).unwrap();
        let hats = state.bin_of(1).unwrap();
        state.merge_bins(&analysis, hats, kicks);
        let merged_pads = state.pads.clone();
        state.remember();
        state.pads.insert(49, Placed { bin: 0, pick: 1 });
        // Undo takes back the pad first, then the merge.
        state.undo();
        assert_eq!(state.pads, merged_pads);
        assert_eq!(state.binning.bins.len(), 1);
        state.undo();
        assert_eq!(state.binning.bins.len(), 2);
        let steps = state.undo.len();
        state.move_hit(&analysis, 1, Target::Bin(9));
        assert_eq!(state.undo.len(), steps);
    }

    #[test]
    fn the_slider_throws_the_manual_edits_away() {
        let (analysis, mut state) = two_groups();
        state.move_hit(&analysis, 1, Target::NewBin);
        assert!(!state.undo.is_empty());
        state.rebin(&analysis);
        assert!(state.undo.is_empty());
        assert_eq!(state.binning.bins.len(), 2);
    }

    #[test]
    fn layers_split_a_group_by_level_and_reach_the_plan_and_undo_takes_them_back() {
        let (analysis, mut state) = two_groups();
        let kicks = state.bin_of(0).unwrap();
        state.pads.insert(
            36,
            Placed {
                bin: kicks,
                pick: 2,
            },
        );
        assert!(state.plan(&analysis.hits).bins[0].layers.is_empty());
        state.set_layers(kicks, 2);
        assert_eq!(state.binning.bins[kicks].layers, 2);
        assert_eq!(state.pads[&36].pick, 1, "the slots moved");
        // The kicks get quieter with each hit: 0 and 2 are the hard pair.
        let layers = state.layers_of(&analysis, kicks);
        assert_eq!(layers.len(), 2);
        let mut soft = layers[0].clone();
        soft.sort_unstable();
        assert_eq!(soft, [4, 6]);
        let plan = state.plan(&analysis.hits);
        let planned = &plan.bins[0];
        let layer_of = |hit: usize| {
            let at = planned.hits.iter().position(|&h| h == hit).unwrap();
            planned.layers[at]
        };
        assert_eq!(
            [layer_of(0), layer_of(2), layer_of(4), layer_of(6)],
            [2, 2, 1, 1]
        );
        // A pad plays a hit of the hardest layer, and has two slots to choose from.
        assert_eq!(state.slots(&analysis, kicks), 2);
        assert!([0, 2].contains(&state.pad_hit(&analysis, kicks, 1)));
        // More layers than hits is not offered, and a group of one stays flat.
        state.set_layers(kicks, 9);
        assert_eq!(state.binning.bins[kicks].layers, 3);
        // Moving a hit out keeps the layers (as many as still fit).
        state.move_hit(&analysis, 6, Target::Other);
        assert_eq!(state.binning.bins[kicks].layers, 3);
        state.move_hit(&analysis, 4, Target::Other);
        assert_eq!(state.binning.bins[kicks].layers, 2, "two hits, two layers");
        for _ in 0..4 {
            state.undo();
        }
        assert_eq!(state.binning.bins[kicks].layers, 1);
        // The slider resets them with everything else.
        state.set_layers(kicks, 2);
        state.rebin(&analysis);
        assert!(state.binning.bins.iter().all(|b| b.layers == 1));
    }

    #[test]
    fn the_groove_choice_goes_into_the_plan_and_is_off_by_default() {
        let (analysis, mut state) = two_groups();
        assert!(!state.plan(&analysis.hits).groove);
        state.groove = true;
        assert!(state.plan(&analysis.hits).groove);
    }

    #[test]
    fn a_mixed_hit_names_the_groups_it_is_made_of_while_they_exist() {
        let (analysis, mut state) = two_groups();
        let (kicks, hats) = (state.bin_of(0).unwrap(), state.bin_of(1).unwrap());
        let (kick_medoid, hat_medoid) = (
            state.binning.bins[kicks].medoid,
            state.binning.bins[hats].medoid,
        );
        state.binning.mixed[3] = true;
        state.binning.parts[3] = vec![kick_medoid, hat_medoid];
        let both = format!("{} + {}", state.bin_name(kicks), state.bin_name(hats));
        assert_eq!(state.parts_of(3), both);
        assert!(hit_note(&state, &analysis, 3).contains(&both));
        // A group that is gone is not named.
        state.move_hit(&analysis, kick_medoid, Target::Other);
        assert!(state.bin_of(kick_medoid).is_none());
        assert_eq!(
            state.parts_of(3),
            state.bin_name(state.bin_of(hat_medoid).unwrap())
        );
        // Mixed with no known parts says only that.
        state.binning.parts[3].clear();
        assert_eq!(hit_note(&state, &analysis, 3), " (mixed)");
    }
    #[test]
    fn a_hit_the_separation_found_mixed_ranks_last_and_is_named_by_its_parts() {
        use noodle_stems::drum::{HitSeparation, Separation};
        use noodle_stems::{ModelId, Stereo};
        let (mut analysis, mut state) = two_groups();
        // Hit 0 is the best kick by default; the separation hears a hat in it.
        let kicks = state.bin_of(0).unwrap();
        assert_ne!(state.binning.bins[kicks].members.last(), Some(&0));
        let none = || HitSeparation {
            levels: Vec::new(),
            active: vec![DrumPart::Kick],
            audio: Vec::new(),
        };
        let mut hits: Vec<HitSeparation> = (0..8).map(|_| none()).collect();
        hits[0].active = vec![DrumPart::Kick, DrumPart::HiHat];
        hits[0].audio = vec![(DrumPart::Kick, Stereo::silent(10))];
        analysis.separation = Some(std::sync::Arc::new(Separation {
            model: ModelId {
                id: "larsnet".into(),
                version: "1".into(),
            },
            parts: vec![DrumPart::Kick, DrumPart::HiHat],
            hits,
        }));
        assert!(!state.separation_applied);
        state.apply_separation(&analysis);
        assert!(state.separation_applied);
        assert!(state.binning.mixed[0]);
        assert_eq!(state.binning.bins[kicks].members.last(), Some(&0));
        assert_ne!(state.binning.bins[kicks].members[0], 0);
        assert_eq!(hit_note(&state, &analysis, 0), " (mixed: kick + hi-hat)");
        // The slider re-bins, so what the separation found is taken in again.
        state.rebin(&analysis);
        assert!(!state.separation_applied);
    }
}
