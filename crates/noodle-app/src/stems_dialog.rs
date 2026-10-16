//! Asking before the stem model is downloaded.

use crate::session::Session;

/// Shows the question while a split waits for the user to agree to the
/// download, and acts on the answer.
pub fn show(ctx: &egui::Context, session: &mut Session) {
    if session.stems_consent().is_none() {
        return;
    }
    let folder = session.stems_model_folder().map_or_else(
        || "the Noodle data folder".to_owned(),
        |f| f.display().to_string(),
    );
    let mut answer = None;
    let modal = egui::Modal::new(egui::Id::new("stems consent")).show(ctx, |ui| {
        ui.set_max_width(420.0);
        ui.heading("Download the stem model?");
        ui.add_space(6.0);
        ui.label(
            "Splitting a clip into stems uses an AI model (HT-Demucs) that is not \
             included with Noodle. It is downloaded once, can be several hundred \
             megabytes, and is kept in:",
        );
        ui.label(egui::RichText::new(&folder).monospace().small());
        ui.add_space(4.0);
        ui.label("The separating itself happens on this computer; your audio is not uploaded.");
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button("Download and split").clicked() {
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
    match answer {
        Some(true) => session.allow_stem_download(),
        Some(false) => session.decline_stem_download(),
        None => {}
    }
}
