//! A line of text beside a button that says what the button just did, for a
//! few seconds. The status bar says it too, but a user looking at the button
//! they pressed does not look there; an action whose effect is not drawn
//! anywhere (a detected tempo, pinned markers) needs its answer where it was
//! asked.

use egui::{Id, Ui};

/// How long a result stays.
const SECONDS: f64 = 8.0;

fn id(key: &str) -> Id {
    Id::new(("inline result", key))
}

/// Records what the action `key` just did.
pub fn remember(ui: &Ui, key: &str, text: impl Into<String>) {
    let now = ui.input(|i| i.time);
    let text = text.into();
    ui.data_mut(|d| d.insert_temp(id(key), (now, text)));
}

/// What `key` last did, while it is recent.
pub fn recall(ui: &Ui, key: &str) -> Option<String> {
    let now = ui.input(|i| i.time);
    let (at, text) = ui.data(|d| d.get_temp::<(f64, String)>(id(key)))?;
    (now - at < SECONDS).then(|| {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_secs_f64(
                SECONDS - (now - at) + 0.1,
            ));
        text
    })
}

/// Shows what `key` last did, if it is recent.
pub fn show(ui: &mut Ui, key: &str) {
    if let Some(text) = recall(ui, key) {
        ui.label(egui::RichText::new(text).color(ui.visuals().strong_text_color()));
    }
}
