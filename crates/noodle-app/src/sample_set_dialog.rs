//! Showing what a rescan of a Sample Set's folder would change, before it does.

use crate::session::Session;

/// Shows the diff while a rescan waits for the user, and acts on the answer.
pub fn show(ctx: &egui::Context, session: &mut Session) {
    let mut answer = None;
    {
        let pending = session.pending_rescan();
        let Some(pending) = pending.as_ref() else {
            return;
        };
        let plan = &pending.plan;
        let modal = crate::dialog::modal(ctx, "sample set rescan", |ui| {
            ui.set_max_width(460.0);
            ui.heading("Rescan the folder?");
            ui.add_space(6.0);
            let group = |ui: &mut egui::Ui, title: &str, lines: Vec<String>| {
                if lines.is_empty() {
                    return;
                }
                ui.label(egui::RichText::new(format!("{title} ({})", lines.len())).strong());
                for line in lines {
                    ui.label(egui::RichText::new(line).monospace().small());
                }
                ui.add_space(4.0);
            };
            group(ui, "Added", plan.added.clone());
            group(
                ui,
                "Replaced (new audio under the same name)",
                plan.replaced.clone(),
            );
            group(ui, "Removed", plan.removed.clone());
            group(
                ui,
                "Renamed (same audio, nothing to do)",
                plan.renamed
                    .iter()
                    .map(|(a, b)| format!("{a} → {b}"))
                    .collect(),
            );
            group(ui, "Couldn't read a note from", plan.unplaced.clone());
            ui.label(
                egui::RichText::new(
                    "Rows you edited keep their edits, except where the file was replaced.",
                )
                .weak()
                .small(),
            );
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("Apply").clicked() {
                    answer = Some(true);
                }
                if ui.button("Cancel").clicked() {
                    answer = Some(false);
                }
            });
        });
        if modal.should_close() {
            answer.get_or_insert(false);
        }
        // Enter takes the diff, as Apply does.
        if ctx.input(|i| i.key_pressed(egui::Key::Enter)) {
            answer.get_or_insert(true);
        }
    }
    match answer {
        Some(true) => session.apply_rescan(),
        Some(false) => session.cancel_rescan(),
        None => {}
    }
}
