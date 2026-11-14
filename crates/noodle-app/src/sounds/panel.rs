//! Drawing the Sounds panel.

use egui::{Context, Sense, Ui, text::LayoutJob};

use super::catalogue::{self, Entry, Kind};
use super::{Reach, Sounds, Then};
use crate::carry::{self, Item, SoundKey};

pub(super) fn show(sounds: &mut Sounds, ctx: &Context) {
    let mut open = true;
    egui::Window::new("Sounds")
        .open(&mut open)
        .default_width(420.0)
        .default_height(520.0)
        .show(ctx, |ui| contents(sounds, ui, ctx));
    sounds.open = open;
    confirm(sounds, ctx);
}

fn contents(sounds: &mut Sounds, ui: &mut Ui, ctx: &Context) {
    // Nodes, Groups and Presets join these as tabs when they are browsable.
    ui.horizontal(|ui| {
        let _ = ui.selectable_label(true, Kind::Sound.name());
    });
    ui.horizontal(|ui| {
        ui.add(
            egui::TextEdit::singleline(&mut sounds.filter.query)
                .hint_text("Search name, tag, pack or author")
                .desired_width(ui.available_width() - 70.0),
        );
        let refreshing = sounds.refreshing.is_some();
        if ui
            .add_enabled(!refreshing, egui::Button::new("Refresh"))
            .on_hover_text("Ask every source what it has now")
            .clicked()
        {
            sounds.refresh();
        }
    });
    filters(sounds, ui);
    for source in sounds.sources.clone() {
        if let Some(Reach::Unreachable(why)) = sounds.reach.get(&source.id) {
            ui.colored_label(
                crate::theme::MUTE,
                format!("⚠ {} can't be reached: {why}", source.name),
            );
        }
    }
    status(sounds, ui);

    let found = catalogue::list(&sounds.entries, &sounds.filter, &sounds.cached);
    ui.weak(format!(
        "{} of {} sounds",
        found.len(),
        sounds.entries.len()
    ));
    if sounds.entries.is_empty() && sounds.refreshing.is_none() {
        ui.label("No sounds yet. Refresh when this computer is online.");
    }
    let mut clicked = None;
    egui::ScrollArea::vertical()
        .max_height(ui.available_height() - 6.0)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for entry in found {
                let reachable = matches!(sounds.reach.get(&entry.source), Some(Reach::Reachable));
                let cached = sounds.cached.contains(&entry.key);
                if row(ui, ctx, entry, cached, reachable) {
                    clicked = Some(entry.clone());
                }
            }
        });
    if let Some(entry) = clicked {
        sounds.request(entry, Then::Audition);
    }
}

fn filters(sounds: &mut Sounds, ui: &mut Ui) {
    // One chip for each source, and All.
    ui.horizontal_wrapped(|ui| {
        if ui
            .selectable_label(sounds.filter.source.is_none(), "All sources")
            .clicked()
        {
            sounds.filter.source = None;
        }
        for source in sounds.sources.clone() {
            let on = sounds.filter.source.as_deref() == Some(source.id.as_str());
            let tip = match (&source.folder, source.is_self_hosted()) {
                (Some(folder), _) => format!("Folder {}", folder.path.display()),
                (None, true) => format!("{}\nSelf-hosted: Noodle can't vouch for it", source.url),
                (None, false) => source.url.clone(),
            };
            let label = if source.is_self_hosted() {
                format!("⚠ {}", source.name)
            } else {
                source.name.clone()
            };
            if ui.selectable_label(on, label).on_hover_text(tip).clicked() {
                sounds.filter.source = (!on).then(|| source.id.clone());
            }
        }
        let add = ui
            .small_button("Sources…")
            .on_hover_text("Add, remove and manage where sounds come from, in Settings");
        if add.clicked() {
            sounds.wants_sources = true;
        }
        add.context_menu(|ui| {
            if ui.button("Add a folder as a source…").clicked() {
                if let Some(path) = crate::picker::pick_folder()
                    && let Err(why) = sounds.add_folder(path)
                {
                    sounds.message = Some(why);
                }
                ui.close();
            }
        });
    });
    ui.horizontal(|ui| {
        let selected = sounds
            .filter
            .licence
            .clone()
            .unwrap_or_else(|| "Any licence".into());
        egui::ComboBox::from_id_salt("sounds licence")
            .selected_text(selected)
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut sounds.filter.licence, None, "Any licence");
                for label in catalogue::licences(&sounds.entries) {
                    ui.selectable_value(&mut sounds.filter.licence, Some(label.clone()), label);
                }
            });
    });
}

/// What is happening: a download, a sound playing, a problem.
fn status(sounds: &mut Sounds, ui: &mut Ui) {
    if let Some(playing) = &sounds.playing {
        let name = playing.name.clone();
        ui.horizontal(|ui| {
            ui.label(format!("Auditioning {name}"));
            if ui.small_button("Stop").clicked() {
                sounds.stop();
            }
        });
    }
    let mut cancel = None;
    for (i, fetch) in sounds.fetching.iter_mut().enumerate() {
        ui.horizontal(|ui| {
            ui.add(
                egui::ProgressBar::new(fetch.job.fraction())
                    .desired_width(160.0)
                    .text(format!("Downloading {}", fetch.entry.name)),
            );
            if ui.small_button("Cancel").clicked() {
                cancel = Some(i);
            }
        });
    }
    if let Some(i) = cancel {
        // Dropping the job cancels it.
        sounds.fetching.remove(i);
    }
    if let Some(message) = sounds.message.clone() {
        ui.horizontal(|ui| {
            ui.colored_label(crate::theme::MUTE, message);
            if ui.small_button("Dismiss").clicked() {
                sounds.message = None;
            }
        });
    }
}

/// One sound. Click auditions it; dragging carries it to a Sampler, a pad,
/// the canvas or the timeline. Returns whether it was clicked.
fn row(ui: &mut Ui, ctx: &Context, entry: &Entry, cached: bool, reachable: bool) -> bool {
    let text_colour = if reachable || cached {
        ui.visuals().text_color()
    } else {
        ui.visuals().weak_text_color()
    };
    let size = entry
        .bytes
        .map_or(String::new(), |b| format!(" · {}", megabytes(b)));
    let details = format!(
        "{} · {}{}{}",
        [entry.pack.as_str(), entry.author.as_str()]
            .iter()
            .filter(|s| !s.is_empty())
            .map(|s| format!("{s} · "))
            .collect::<String>(),
        entry.licence.label(),
        size,
        if entry.file.is_some() {
            " · in a folder"
        } else if cached {
            " · on this computer"
        } else {
            ""
        }
    );
    let mut job = LayoutJob::default();
    let font = egui::TextStyle::Body.resolve(ui.style());
    job.append(
        &entry.name,
        0.0,
        egui::TextFormat::simple(font, text_colour),
    );
    job.append(
        &format!("\n{details}"),
        0.0,
        egui::TextFormat::simple(
            egui::TextStyle::Small.resolve(ui.style()),
            ui.visuals().weak_text_color(),
        ),
    );
    let response = ui
        .add(
            egui::Button::new(job)
                .frame(false)
                .min_size(egui::vec2(ui.available_width(), 0.0))
                .sense(Sense::click_and_drag()),
        )
        .on_hover_text(if entry.tags.is_empty() {
            "Click to hear it. Drag it onto a Sampler, a drum pad, the canvas or the timeline."
                .to_owned()
        } else {
            format!(
                "{}\nClick to hear it. Drag it onto a Sampler, a drum pad, the canvas or the timeline.",
                entry.tags.join(", ")
            )
        });
    if let (true, Some(key), Some(pointer)) = (
        response.dragged(),
        SoundKey::from_hash(&entry.key),
        ctx.pointer_latest_pos(),
    ) {
        carry::carrying(ctx, Item::Sound(key), pointer, &entry.name);
    }
    if response.drag_stopped()
        && let (Some(key), Some(pointer)) =
            (SoundKey::from_hash(&entry.key), ctx.pointer_latest_pos())
    {
        carry::release(ctx, Item::Sound(key), pointer);
    }
    response.clicked()
}

fn megabytes(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{} KB", bytes.div_ceil(1024))
    }
}

/// The question before a big download.
fn confirm(sounds: &mut Sounds, ctx: &Context) {
    let Some((entry, _)) = &sounds.confirm else {
        return;
    };
    let (name, bytes) = (entry.name.clone(), entry.bytes.unwrap_or(0));
    let source = sounds
        .source(&entry.source)
        .map_or("its source".to_owned(), |s| s.name.clone());
    let mut answer = None;
    let modal = crate::dialog::modal(ctx, "sounds confirm", |ui| {
        ui.set_max_width(380.0);
        ui.heading("Download this sound?");
        ui.label(format!(
            "{name} is {} and comes from {source}. It is kept on this computer, so it is \
             downloaded once.",
            megabytes(bytes)
        ));
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button("Download").clicked() {
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
    if let Some(yes) = answer
        && let Some((entry, then)) = sounds.confirm.take()
        && yes
    {
        sounds.start(entry, then);
    }
}
