//! The export dialog: which format, and the whole project or a range of it.
//!
//! It only asks. Choosing "Export…" hands the choice back; the app then asks
//! where to save (a file dialog can't open mid-frame) and starts the export
//! on the session, which runs it in the background. The dialog stays up with
//! a progress bar until it ends.

use noodle_io::ExportFormat;

use crate::session::{ExportChoice, Session};

/// What the range fields hold.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Part {
    Whole,
    Range,
}

pub struct ExportDialog {
    open: bool,
    format: ExportFormat,
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
}

impl Default for ExportDialog {
    fn default() -> Self {
        Self {
            open: false,
            format: ExportFormat::Wav24,
            part: Part::Whole,
            from: 0.0,
            to: 0.0,
            length: 0.0,
            chosen: None,
            started: false,
        }
    }
}

/// Minutes and seconds, as 1:05.3.
fn clock(seconds: f64) -> String {
    let minutes = (seconds / 60.0).floor();
    format!("{}:{:04.1}", minutes as u64, seconds - minutes * 60.0)
}

impl ExportDialog {
    pub fn open(&mut self, session: &Session) {
        self.open = true;
        self.started = false;
        self.chosen = None;
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
        // The export this dialog started is over.
        if self.started && running.is_none() {
            self.open = false;
            return;
        }
        let modal = egui::Modal::new(egui::Id::new("export")).show(ctx, |ui| {
            ui.heading("Export Audio");
            ui.add_space(6.0);
            ui.add_enabled_ui(running.is_none(), |ui| self.fields(ui));
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
            let can = range_ok && (self.part == Part::Range || self.length > 0.0);
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(can, egui::Button::new("Export…"))
                    .on_hover_text("Choose a file to write")
                    .clicked()
                {
                    self.chosen = Some(ExportChoice {
                        format: self.format,
                        range: (self.part == Part::Range).then_some((self.from, self.to)),
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
                    if range_ok {
                        "There is nothing to export yet"
                    } else {
                        "The range ends before it starts"
                    },
                );
            }
        });
        if modal.should_close() && running.is_none() {
            self.open = false;
        }
    }

    fn fields(&mut self, ui: &mut egui::Ui) {
        egui::Grid::new("export grid")
            .num_columns(2)
            .spacing([12.0, 6.0])
            .show(ui, |ui| {
                ui.label("Format");
                egui::ComboBox::from_id_salt("export format")
                    .selected_text(self.format.label())
                    .show_ui(ui, |ui| {
                        for format in ExportFormat::ALL {
                            ui.selectable_value(&mut self.format, format, format.label());
                        }
                    });
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
        if self.format.clips() {
            ui.add_space(4.0);
            ui.weak("Levels past full scale are cut off in this format.");
        }
    }
}
