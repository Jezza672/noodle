//! The export dialog: which format, which files (the mixdown and any tracks
//! or groups), and the whole project or a range of it. When an export ends
//! it shows how loud each file is.
//!
//! It only asks. Choosing "Export…" hands the choice back; the app then asks
//! where to save (a file dialog can't open mid-frame) and starts the export
//! on the session, which runs it in the background. The dialog stays up with
//! a progress bar until it ends.

use noodle_core::{Command, ExportSettings, ExportTail, NodeId};
use noodle_engine::ChannelLayout;
use noodle_io::{ExportFormat, Loudness};

use crate::session::{ExportChoice, ExportSummary, Session};

/// What the range fields hold.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Part {
    Whole,
    Range,
}

pub struct ExportDialog {
    open: bool,
    format: ExportFormat,
    layout: ChannelLayout,
    part: Part,
    /// The range in seconds.
    from: f64,
    to: f64,
    /// The length of the project in seconds, as of when the dialog opened.
    length: f64,
    /// Set when the user chose "Export…" and the app has yet to take it.
    chosen: Option<ExportChoice>,
    /// An export started from this dialog is under way, or just ended.
    started: bool,
    /// Write the mixdown.
    mixdown: bool,
    /// The groups that can be written, as of when the dialog opened, and
    /// whether each is ticked.
    groups: Vec<(NodeId, String, bool)>,
    /// Bring the mixdown to `target` LUFS.
    normalize: bool,
    target: f64,
    /// Hold the mixdown's true peak under `ceiling` dBTP.
    limit: bool,
    ceiling: f64,
    /// Written into the mixdown file, and a CUE sheet next to it.
    title: String,
    artist: String,
    isrc: String,
    cue: bool,
    /// The tail, as the project has it; changing it is one undo step.
    tail: ExportTail,
    /// How the export that ended went, shown in place of the fields.
    done: Option<ExportSummary>,
}

impl Default for ExportDialog {
    fn default() -> Self {
        Self {
            open: false,
            format: ExportFormat::Wav24,
            layout: ChannelLayout::Stereo,
            part: Part::Whole,
            from: 0.0,
            to: 0.0,
            length: 0.0,
            chosen: None,
            started: false,
            mixdown: true,
            groups: Vec::new(),
            normalize: false,
            target: -14.0,
            limit: false,
            ceiling: -1.0,
            title: String::new(),
            artist: String::new(),
            isrc: String::new(),
            cue: false,
            tail: ExportTail::default(),
            done: None,
        }
    }
}

/// A layout with its channel count, as the channel dropdowns list it.
pub(crate) fn layout_label(layout: ChannelLayout) -> String {
    let channels = layout.channels();
    format!(
        "{} ({channels} channel{})",
        layout.name(),
        if channels == 1 { "" } else { "s" }
    )
}

/// Minutes and seconds, as 1:05.3.
fn clock(seconds: f64) -> String {
    let minutes = (seconds / 60.0).floor();
    format!("{}:{:04.1}", minutes as u64, seconds - minutes * 60.0)
}

impl ExportDialog {
    pub fn open(&mut self, session: &mut Session) {
        self.open = true;
        self.started = false;
        self.chosen = None;
        self.done = None;
        session.clear_last_export();
        // Keep what was ticked for groups that are still there.
        let before = std::mem::take(&mut self.groups);
        self.groups = session
            .exportable_groups()
            .into_iter()
            .map(|(id, name)| {
                let ticked = before.iter().any(|(g, _, on)| *g == id && *on);
                (id, name, ticked)
            })
            .collect();
        self.tail = session.project().export_settings().tail;
        self.length = session.project_seconds();
        self.from = 0.0;
        self.to = self.length;
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Whether "Export…" was chosen and the app has yet to take the choice.
    pub fn has_choice(&self) -> bool {
        self.chosen.is_some()
    }

    /// The choice made with "Export…", once, for the app to pick a file for.
    pub fn take_choice(&mut self) -> Option<ExportChoice> {
        self.chosen.take()
    }

    /// The export has started (the file was chosen): wait for it.
    pub fn started(&mut self) {
        self.started = true;
    }

    /// The user backed out of choosing a file: ask again.
    pub fn declined(&mut self) {
        self.started = false;
    }

    pub fn show(&mut self, ctx: &egui::Context, session: &mut Session) {
        if !self.open {
            return;
        }
        let running = session.export_progress();
        // The export this dialog started is over: show how it went, if it
        // wrote anything.
        if self.started && running.is_none() {
            self.started = false;
            match session.last_export() {
                Some(summary) => self.done = Some(summary.clone()),
                None => {
                    self.open = false;
                    return;
                }
            }
        }
        let modal = crate::dialog::modal(ctx, "export", |ui| {
            ui.heading("Export Audio");
            ui.add_space(6.0);
            if let Some(summary) = &self.done {
                if results(ui, summary) {
                    self.open = false;
                }
                return;
            }
            ui.add_enabled_ui(running.is_none(), |ui| self.fields(ui));
            if self.tail != session.project().export_settings().tail {
                session.edit([crate::session::Edit::Apply(Command::SetExportSettings(
                    ExportSettings { tail: self.tail },
                ))]);
            }
            ui.add_space(8.0);
            if let Some(fraction) = running {
                ui.add(egui::ProgressBar::new(fraction).show_percentage());
                ui.add_space(4.0);
                if ui.button("Cancel Export").clicked() {
                    session.cancel_export();
                }
                // The bar moves without anything else repainting.
                ctx.request_repaint_after(std::time::Duration::from_millis(50));
                return;
            }
            let range_ok = self.part == Part::Whole || self.to > self.from;
            let isrc_ok = self.isrc_ok();
            let chosen = self.mixdown || self.groups.iter().any(|(_, _, on)| *on);
            let flac_ok = !self.flac_too_wide();
            let can = range_ok
                && isrc_ok
                && flac_ok
                && (!self.cue || self.mixdown)
                && chosen
                && (self.part == Part::Range || self.length > 0.0);
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(can, egui::Button::new("Export…"))
                    .on_hover_text("Choose where to write")
                    .clicked()
                {
                    self.chosen = Some(ExportChoice {
                        format: self.format,
                        layout: self.layout,
                        range: (self.part == Part::Range).then_some((self.from, self.to)),
                        mixdown: self.mixdown,
                        groups: self
                            .groups
                            .iter()
                            .filter(|(_, _, on)| *on)
                            .map(|(id, _, _)| *id)
                            .collect(),
                        normalize: self.normalize.then_some(self.target),
                        true_peak_ceiling: self.limit.then_some(self.ceiling),
                        title: self.title.clone(),
                        artist: self.artist.clone(),
                        isrc: self.isrc.clone(),
                        cue: self.cue,
                    });
                }
                if ui.button("Close").clicked() {
                    self.open = false;
                }
            });
            if !can {
                ui.add_space(4.0);
                ui.colored_label(
                    ui.visuals().warn_fg_color,
                    if !range_ok {
                        "The range ends before it starts"
                    } else if !flac_ok {
                        "FLAC holds at most 8 channels: choose WAV for this layout"
                    } else if !isrc_ok {
                        "The ISRC is two letters, three letters or digits, then seven digits"
                    } else if self.cue && !self.mixdown {
                        "A CUE sheet goes with the mixdown: tick Mixdown"
                    } else if !chosen {
                        "Choose the mixdown or a track to export"
                    } else {
                        "There is nothing to export yet"
                    },
                );
            }
        });
        if modal.should_close() && running.is_none() {
            self.open = false;
        }
    }

    /// Whether the format is FLAC and the layout has more channels than it holds.
    fn flac_too_wide(&self) -> bool {
        matches!(self.format, ExportFormat::Flac24 | ExportFormat::Flac16)
            && self.layout.channels() > noodle_io::FLAC_MAX_CHANNELS
    }

    /// Whether the ISRC is empty or a valid one.
    fn isrc_ok(&self) -> bool {
        self.isrc.trim().is_empty() || noodle_io::normalize_isrc(&self.isrc).is_some()
    }

    fn fields(&mut self, ui: &mut egui::Ui) {
        egui::Grid::new("export grid")
            .num_columns(2)
            .spacing([12.0, 6.0])
            .show(ui, |ui| {
                ui.label("Format");
                crate::widgets::Dropdown::from_id_salt("export format")
                    .selected_text(self.format.label())
                    .show_ui(ui, |ui| {
                        for format in ExportFormat::ALL {
                            ui.selectable_value(&mut self.format, format, format.label());
                        }
                    });
                ui.end_row();
                ui.label("Channels");
                crate::widgets::Dropdown::from_id_salt("export channels")
                    .selected_text(layout_label(self.layout))
                    .show_ui(ui, |ui| {
                        for layout in ChannelLayout::ALL {
                            ui.selectable_value(&mut self.layout, layout, layout_label(layout));
                        }
                    });
                ui.end_row();
                ui.label("Files");
                ui.vertical(|ui| {
                    ui.checkbox(&mut self.mixdown, "Mixdown")
                        .on_hover_text("The project as it plays, master processing included");
                    egui::ScrollArea::vertical()
                        .id_salt("export groups")
                        .max_height(140.0)
                        .show(ui, |ui| {
                            for (_, name, on) in &mut self.groups {
                                ui.checkbox(on, name.as_str());
                            }
                        });
                });
                ui.end_row();
                ui.label("Loudness");
                ui.horizontal(|ui| {
                    ui.checkbox(&mut self.normalize, "Normalise mixdown to")
                        .on_hover_text(
                            "One gain on every file, so the tracks still add up to the mixdown",
                        );
                    ui.add_enabled(
                        self.normalize,
                        egui::DragValue::new(&mut self.target)
                            .range(-60.0..=0.0)
                            .speed(0.1)
                            .suffix(" LUFS"),
                    );
                });
                ui.end_row();
                ui.label("True peak");
                ui.horizontal(|ui| {
                    ui.checkbox(&mut self.limit, "Limit the mixdown to")
                        .on_hover_text(
                            "A lookahead limiter after the loudness gain keeps the peaks between \
                             samples under the ceiling. Track files are not limited.",
                        );
                    ui.add_enabled(
                        self.limit,
                        egui::DragValue::new(&mut self.ceiling)
                            .range(-12.0..=0.0)
                            .speed(0.05)
                            .suffix(" dBTP"),
                    );
                });
                ui.end_row();
                ui.label("Title");
                ui.text_edit_singleline(&mut self.title);
                ui.end_row();
                ui.label("Artist");
                ui.text_edit_singleline(&mut self.artist);
                ui.end_row();
                ui.label("ISRC");
                ui.add(
                    egui::TextEdit::singleline(&mut self.isrc)
                        .hint_text("CC-XXX-YY-NNNNN")
                        .desired_width(150.0),
                );
                ui.end_row();
                ui.label("CUE sheet");
                ui.checkbox(&mut self.cue, "Write one next to the mixdown")
                    .on_hover_text(
                        "A .cue file with the title, artist and ISRC. For a CD, choose WAV 16-bit \
                         at 44.1 kHz.",
                    );
                ui.end_row();
                ui.label("Part");
                ui.vertical(|ui| {
                    ui.radio_value(
                        &mut self.part,
                        Part::Whole,
                        format!("Whole project ({} and a tail)", clock(self.length)),
                    );
                    ui.radio_value(&mut self.part, Part::Range, "Range");
                });
                ui.end_row();
                ui.label("Tail");
                ui.horizontal(|ui| tail_row(ui, &mut self.tail));
                ui.end_row();
                if self.part == Part::Range {
                    ui.label("From");
                    ui.add(
                        egui::DragValue::new(&mut self.from)
                            .range(0.0..=36_000.0)
                            .speed(0.1)
                            .suffix(" s"),
                    );
                    ui.end_row();
                    ui.label("To");
                    ui.add(
                        egui::DragValue::new(&mut self.to)
                            .range(0.0..=36_000.0)
                            .speed(0.1)
                            .suffix(" s"),
                    );
                    ui.end_row();
                }
            });
        ui.add_space(4.0);
        if self.groups.iter().any(|(_, _, on)| *on) {
            ui.weak("Track files are taken after the group's gain and mute, before the master.");
            ui.weak("Pick a folder; each file is named for its group.");
        }
        if self.format.clips() {
            ui.weak("Levels past full scale are cut off in this format.");
        }
        if matches!(
            self.format,
            ExportFormat::Wav24 | ExportFormat::Wav16 | ExportFormat::Flac24 | ExportFormat::Flac16
        ) {
            ui.weak("16 and 24-bit files are dithered.");
        }
    }
}

/// The loudness readout of an export that ended. Returns whether "Close" was
/// pressed.
fn results(ui: &mut egui::Ui, summary: &ExportSummary) -> bool {
    ui.label(format!("Exported to {}", summary.location.display()));
    if summary.gain_db != 0.0 {
        ui.label(format!(
            "Normalised: {:+.1} dB on every file",
            summary.gain_db
        ));
    }
    ui.add_space(4.0);
    egui::Grid::new("export results")
        .num_columns(3)
        .spacing([12.0, 4.0])
        .show(ui, |ui| {
            ui.strong("File");
            ui.strong("Loudness");
            ui.strong("True peak");
            ui.end_row();
            for (name, loudness) in &summary.files {
                ui.label(name);
                ui.label(lufs(loudness));
                ui.label(true_peak(loudness));
                ui.end_row();
            }
        });
    if summary.files.iter().any(|(_, l)| l.sample_peak_db > 0.0) {
        ui.add_space(4.0);
        ui.colored_label(
            ui.visuals().warn_fg_color,
            "Some files go past full scale; 16 and 24-bit files cut that off.",
        );
    }
    if let Some(cue) = &summary.cue {
        ui.add_space(4.0);
        ui.label(format!("CUE sheet: {}", cue.display()));
    }
    ui.add_space(8.0);
    ui.button("Close").clicked()
}

fn lufs(loudness: &Loudness) -> String {
    if loudness.lufs.is_finite() {
        format!("{:.1} LUFS", loudness.lufs)
    } else {
        "silent".to_owned()
    }
}

fn true_peak(loudness: &Loudness) -> String {
    if loudness.true_peak_db.is_finite() {
        format!("{:.1} dBTP", loudness.true_peak_db)
    } else {
        "silent".to_owned()
    }
}

/// The Tail row: to silence (up to a cap) or a fixed length. One length for
/// every file, so the stems stay the same length.
fn tail_row(ui: &mut egui::Ui, tail: &mut ExportTail) {
    let silence = matches!(tail, ExportTail::ToSilence { .. });
    let label = if silence {
        "To silence, up to"
    } else {
        "Fixed"
    };
    crate::widgets::Dropdown::from_id_salt("export tail")
        .selected_text(label)
        .show_ui(ui, |ui| {
            if ui.selectable_label(silence, "To silence, up to").clicked() && !silence {
                *tail = ExportTail::ToSilence { cap: 10.0 };
            }
            if ui.selectable_label(!silence, "Fixed").clicked() && silence {
                *tail = ExportTail::Fixed { seconds: 4.0 };
            }
        });
    let value = match tail {
        ExportTail::ToSilence { cap } => cap,
        ExportTail::Fixed { seconds } => seconds,
    };
    ui.add(
        egui::DragValue::new(value)
            .range(0.0..=60.0)
            .speed(0.1)
            .suffix(" s"),
    )
    .on_hover_text("Every file gets the same tail, so the stems line up at bar 1");
}
