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
use noodle_io::{Binning, DEFAULT_MERGE, DrumKind, bin_hits};

use crate::session::drum_rack::{DrumAnalysis, DrumRackPlan, PadAssignment, PlannedBin, pad_name};
use crate::session::{Edit, Session};

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

struct State {
    clip: ClipId,
    merge: f32,
    binning: Binning,
    pads: BTreeMap<u8, Placed>,
    show_all: bool,
}

impl State {
    fn new(analysis: &DrumAnalysis) -> Self {
        let mut state = Self {
            clip: analysis.clip,
            merge: DEFAULT_MERGE,
            binning: bin_hits(&analysis.hits, DEFAULT_MERGE),
            pads: BTreeMap::new(),
            show_all: false,
        };
        state.guess_pads();
        state
    }

    /// Re-bins at the slider's setting, which throws the pads away: the bins
    /// they point at are no longer there.
    fn rebin(&mut self, analysis: &DrumAnalysis) {
        self.binning = bin_hits(&analysis.hits, self.merge);
        self.guess_pads();
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
    fn plan(&self) -> DrumRackPlan {
        let mut used: Vec<usize> = self.pads.values().map(|p| p.bin).collect();
        used.sort_unstable();
        used.dedup();
        let bins = used
            .iter()
            .map(|&bin| PlannedBin {
                name: self.bin_name(bin),
                hits: self.binning.bins[bin].members.clone(),
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
        DrumRackPlan { bins, pads }
    }
}

/// The dialog's state, kept by the app between frames.
#[derive(Default)]
pub struct DrumRackDialog {
    state: Option<State>,
}

impl DrumRackDialog {
    pub fn show(&mut self, ctx: &egui::Context, session: &mut Session) {
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
        let mut answer = None;
        let modal = crate::dialog::modal(ctx, "drum rack", |ui| {
            ui.set_width(640.0);
            answer = body(ui, session, &analysis, state);
        });
        if modal.should_close() {
            answer.get_or_insert(false);
        }
        match answer {
            Some(true) => {
                let plan = state.plan();
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

/// Draws the dialog; returns true for Create, false for Cancel.
fn body(
    ui: &mut Ui,
    session: &Session,
    analysis: &DrumAnalysis,
    state: &mut State,
) -> Option<bool> {
    let mut answer = None;
    ui.heading("Create drum rack");
    ui.label(
        egui::RichText::new(format!(
            "{} hits found in {} groups. Drag a group onto a pad.",
            analysis.hits.len(),
            state.binning.bins.len()
        ))
        .weak(),
    );
    ui.add_space(4.0);
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
    });
    ui.separator();
    ui.columns(2, |cols| {
        bins(&mut cols[0], session, analysis, state);
        pads(&mut cols[1], analysis, state);
    });
    ui.separator();
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
    answer
}

fn bins(ui: &mut Ui, session: &Session, analysis: &DrumAnalysis, state: &State) {
    ui.label(egui::RichText::new("Groups").strong());
    egui::ScrollArea::vertical()
        .id_salt("drum bins")
        .auto_shrink([false, false])
        .max_height(PANE_HEIGHT)
        .show(ui, |ui| {
            for (i, bin) in state.binning.bins.iter().enumerate() {
                let name = state.bin_name(i);
                ui.dnd_drag_source(Id::new(("drum bin", i)), i, |ui| {
                    egui::Frame::group(ui.style()).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
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
                    });
                });
            }
            if !state.binning.other.is_empty() {
                ui.label(
                    egui::RichText::new(format!(
                        "Other ({}): quiet or cut-short hits that fit no group",
                        state.binning.other.len()
                    ))
                    .weak()
                    .small(),
                );
            }
        });
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

fn pads(ui: &mut Ui, analysis: &DrumAnalysis, state: &mut State) {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Pads").strong());
        ui.checkbox(&mut state.show_all, "Show all keys");
    });
    let keys: Vec<u8> = if state.show_all {
        (0..=127).collect()
    } else {
        COMMON_KEYS.to_vec()
    };
    egui::ScrollArea::vertical()
        .id_salt("drum pads")
        .auto_shrink([false, false])
        .max_height(PANE_HEIGHT)
        .show(ui, |ui| {
            let mut drops: Vec<(u8, usize)> = Vec::new();
            let mut clears: Vec<u8> = Vec::new();
            let mut picks: Vec<(u8, usize)> = Vec::new();
            for key in keys {
                let placed = state.pads.get(&key).copied();
                let label = format!("{} ({})", pad_name(key), crate::piano_roll::key_name(key));
                let (_, dropped) =
                    ui.dnd_drop_zone::<usize, _>(egui::Frame::group(ui.style()), |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new(label).strong());
                            match placed {
                                None => {
                                    ui.label(egui::RichText::new("empty").weak());
                                }
                                Some(p) => {
                                    ui.label(state.bin_name(p.bin));
                                    let members = &state.binning.bins[p.bin].members;
                                    if members.len() > 1
                                        && let Some(pick) =
                                            picker(ui, p.pick, members, analysis, &state.binning)
                                    {
                                        picks.push((key, pick));
                                    }
                                    if ui
                                        .small_button("×")
                                        .on_hover_text("Empty the pad")
                                        .clicked()
                                    {
                                        clears.push(key);
                                    }
                                }
                            }
                        });
                    });
                if let Some(bin) = dropped {
                    drops.push((key, *bin));
                }
            }
            for (key, pick) in picks {
                if let Some(p) = state.pads.get_mut(&key) {
                    p.pick = pick;
                }
            }
            for key in clears {
                state.pads.remove(&key);
            }
            for (key, bin) in drops {
                state.pads.insert(key, Placed { bin, pick: 1 });
            }
        });
}

/// The row choosing which hit of a bin a pad plays: previous, which, next.
/// Returns the new pick (from 1) when it was changed.
fn picker(
    ui: &mut Ui,
    pick: usize,
    members: &[usize],
    analysis: &DrumAnalysis,
    binning: &Binning,
) -> Option<usize> {
    let n = members.len();
    let pick = pick.clamp(1, n);
    let hit = members[pick - 1];
    let mut note = "";
    if binning.mixed.get(hit).copied().unwrap_or(false) {
        note = " (mixed)";
    } else if analysis.hits[hit].truncated {
        note = " (cut short)";
    }
    let mut new = pick;
    if ui.small_button("◀").on_hover_text("Previous hit").clicked() {
        new = if pick == 1 { n } else { pick - 1 };
    }
    ui.label(format!("{pick}/{n}{note}"));
    if ui.small_button("▶").on_hover_text("Next hit").clicked() {
        new = if pick == n { 1 } else { pick + 1 };
    }
    (new != pick).then_some(new)
}
