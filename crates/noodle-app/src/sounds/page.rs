//! The Sound sources page of Settings: where sounds come from (ADR 0012).
//!
//! Lists the sources with whether each can be reached and what it takes up,
//! adds a self-hosted source (an address and optional login) or a folder, and
//! removes them. Under it, the cache the downloads share and a Clean up for
//! what no source lists any more. Works with no network: nothing here waits on
//! a fetch, and a source that can't be reached shows what it had.

use egui::{RichText, Ui};
use noodle_packs::{Licence, SPDX_IDS};

use super::manage::NewLogin;
use super::source::Source;
use super::{Reach, Sounds};

/// Text for what Noodle can't promise about a source it doesn't run.
pub const NO_PROMISE: &str = "Noodle can't vouch for a source it doesn't run, or for what its host \
     does with the requests Noodle sends it. A sound is checked against the hash the source \
     lists, which proves it wasn't damaged on the way and nothing else: not that it is safe, or \
     licensed as claimed. Add only sources you trust.";

/// What Clean up would remove.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Cleanable {
    pub files: usize,
    pub bytes: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LoginChoice {
    #[default]
    None,
    Bearer,
    Basic,
}

/// The Add a source form.
#[derive(Default)]
pub struct Form {
    pub name: String,
    pub url: String,
    pub login: LoginChoice,
    pub token: String,
    pub user: String,
    pub password: String,
    pub error: Option<String>,
    /// The Add a web source popup is open.
    pub open: bool,
    /// The source just added, while it is being asked whether it answers.
    pub checking: Option<String>,
    /// Whether Clean up is asking to be sure.
    pub confirm_clean: bool,
}

impl Form {
    fn login(&self) -> NewLogin {
        match self.login {
            LoginChoice::None => NewLogin::None,
            LoginChoice::Bearer => NewLogin::Bearer(self.token.clone()),
            LoginChoice::Basic => NewLogin::Basic {
                user: self.user.clone(),
                password: self.password.clone(),
            },
        }
    }
}

/// A size for people: "3.2 MB", "40 KB".
pub fn size(bytes: u64) -> String {
    const MB: f64 = 1024.0 * 1024.0;
    if bytes as f64 >= MB {
        format!("{:.1} MB", bytes as f64 / MB)
    } else {
        format!("{} KB", bytes.div_ceil(1024))
    }
}

/// What a source asks of the page, done once the list is no longer borrowed.
enum Do {
    Remove(String),
    Licence(String, Licence),
}

/// The colour of a source's status dot.
fn dot(ui: &mut Ui, reach: Option<&Reach>) {
    let colour = match reach {
        Some(Reach::Reachable) => egui::Color32::from_rgb(90, 190, 120),
        Some(Reach::Unreachable(_)) => ui.visuals().warn_fg_color,
        _ => ui.visuals().weak_text_color(),
    };
    let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 14.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), 4.0, colour);
}

impl Sounds {
    /// Whether any source could not be reached the last time it was asked.
    pub fn has_unreachable(&self) -> bool {
        self.reach
            .values()
            .any(|r| matches!(r, Reach::Unreachable(_)))
    }

    /// The Add source menu, shared by the Sounds panel and Settings: a web
    /// source opens the popup, a folder goes straight to the picker.
    pub fn add_menu(&mut self, ui: &mut Ui, manage: bool) {
        ui.menu_button("+ Add source", |ui| {
            if ui.button("Web source…").clicked() {
                self.form.open = true;
                ui.close();
            }
            if ui
                .button("Folder on this computer…")
                .on_hover_text("Use a folder of audio files as a source, read where it is")
                .clicked()
            {
                if let Some(path) = crate::picker::pick_folder()
                    && let Err(why) = self.add_folder(path)
                {
                    self.note = Some(why);
                }
                ui.close();
            }
            if manage {
                ui.separator();
                if ui.button("Manage sources in Settings…").clicked() {
                    self.wants_sources = true;
                    ui.close();
                }
                if ui.button("Make shareable pack…").clicked() {
                    self.choose_pack_folder();
                    ui.close();
                }
            }
        });
    }

    /// The body of the Sound sources tab.
    pub fn body(&mut self, ui: &mut Ui) {
        // Opening the page is the first time a source may be asked.
        if !self.asked {
            self.refresh();
        }
        ui.horizontal(|ui| {
            if ui
                .add_enabled(self.refreshing.is_none(), egui::Button::new("Refresh"))
                .on_hover_text("Ask every source what it has now, and rescan the folders")
                .clicked()
            {
                self.refresh();
            }
            if self.refreshing.is_some() {
                ui.spinner();
            }
            self.add_menu(ui, false);
        });
        if let Some(note) = self.note.clone() {
            ui.horizontal(|ui| {
                ui.label(note);
                if ui.small_button("Dismiss").clicked() {
                    self.note = None;
                }
            });
        }

        let mut todo = Vec::new();
        for source in self.sources.clone() {
            ui.separator();
            self.source_row(ui, &source, &mut todo);
        }
        for action in todo {
            match action {
                Do::Remove(id) => self.remove_source(&id),
                Do::Licence(id, licence) => self.set_folder_licence(&id, licence),
            }
        }

        ui.separator();
        self.storage(ui);
    }

    fn source_row(&self, ui: &mut Ui, source: &Source, todo: &mut Vec<Do>) {
        let reach = self.reach.get(&source.id);
        ui.horizontal(|ui| {
            dot(ui, reach);
            let place = match &source.folder {
                Some(folder) => folder.path.display().to_string(),
                None => source.url.clone(),
            };
            ui.label(RichText::new(&source.name).strong())
                .on_hover_text(place);
            if source.folder.is_some() {
                ui.weak("Folder");
            } else if source.is_self_hosted() {
                ui.colored_label(ui.visuals().warn_fg_color, "⚠ Self-hosted")
                    .on_hover_text("Noodle can't vouch for what this source contains.");
            } else {
                ui.weak("Built in");
            }
            if !source.is_built_in() {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.menu_button("⋯", |ui| {
                        if ui
                            .button("Remove source")
                            .on_hover_text(if source.folder.is_some() {
                                "Stop listing this folder. Its files are not touched"
                            } else {
                                "Forget this source and its login. Sounds already downloaded stay until Clean up"
                            })
                            .clicked()
                        {
                            todo.push(Do::Remove(source.id.clone()));
                            ui.close();
                        }
                    });
                });
            }
        });
        ui.horizontal(|ui| {
            ui.add_space(16.0);
            let usage = self.usage(&source.id);
            match reach {
                Some(Reach::Unreachable(why)) => {
                    ui.colored_label(
                        ui.visuals().warn_fg_color,
                        format!("Can't be reached: {why}"),
                    );
                }
                Some(Reach::Reachable) if source.folder.is_some() => {
                    ui.weak(format!("{} sounds, read in place", usage.sounds));
                }
                Some(Reach::Reachable) => {
                    ui.weak(format!(
                        "{} sounds, {} downloaded ({})",
                        usage.sounds,
                        usage.downloaded,
                        size(usage.bytes)
                    ));
                }
                _ => {
                    ui.weak("Asking…");
                }
            }
        });
        if let Some(folder) = &source.folder {
            ui.horizontal(|ui| {
                ui.add_space(16.0);
                ui.label("Licence of these sounds");
                let mut choice = folder.licence.clone();
                egui::ComboBox::from_id_salt(("folder licence", &source.id))
                    .selected_text(choice.label())
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut choice, Licence::Unknown, "Unknown");
                        ui.selectable_value(&mut choice, Licence::Default, "Default");
                        for id in SPDX_IDS {
                            ui.selectable_value(&mut choice, Licence::Spdx((*id).into()), *id);
                        }
                    })
                    .response
                    .on_hover_text(
                        "Unknown until you say. The Sounds panel can filter by it, and a \
                         sound that needs credit carries it onto the timeline",
                    );
                if choice != folder.licence {
                    todo.push(Do::Licence(source.id.clone(), choice));
                }
            });
        }
    }

    /// The Add a web source popup, over whatever is open (Settings or the
    /// Sounds panel). "Check and add" adds the source, waits until it has
    /// answered, and only then closes: an address that does not work stays
    /// in the popup with the reason, and the source is not kept.
    pub fn show_add_dialog(&mut self, ctx: &egui::Context) {
        if !self.form.open {
            return;
        }
        if let Some(id) = self.form.checking.clone() {
            match self.reach.get(&id).cloned() {
                Some(Reach::Unreachable(why)) => {
                    self.remove_source(&id);
                    self.note = None;
                    self.form.checking = None;
                    self.form.error = Some(format!(
                        "Couldn't reach it: {why}. Check the address and the login."
                    ));
                }
                Some(Reach::Reachable) => {
                    self.form = Form::default();
                    return;
                }
                _ => ctx.request_repaint_after(std::time::Duration::from_millis(100)),
            }
        }
        let mut submit = false;
        let mut cancel = false;
        let modal = crate::dialog::modal(ctx, "add web source", |ui| {
            ui.set_max_width(440.0);
            ui.heading("Add a web source");
            ui.add_space(4.0);
            ui.colored_label(ui.visuals().warn_fg_color, NO_PROMISE);
            ui.weak(
                "A source is a folder of files on any web host: an index.json, a manifest for \
                 each pack and the audio.",
            );
            ui.add_space(6.0);
            let busy = self.form.checking.is_some();
            ui.add_enabled_ui(!busy, |ui| {
                egui::Grid::new("add source").num_columns(2).show(ui, |ui| {
                    ui.label("Address");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.form.url)
                            .hint_text("https://example.com/sounds")
                            .desired_width(300.0),
                    );
                    ui.end_row();
                    ui.label("Name");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.form.name)
                            .hint_text("Optional")
                            .desired_width(300.0),
                    );
                    ui.end_row();
                });
                let login = egui::CollapsingHeader::new("This source needs a login")
                    .default_open(self.form.login != LoginChoice::None)
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.selectable_value(
                                &mut self.form.login,
                                LoginChoice::Bearer,
                                "Access token",
                            );
                            ui.selectable_value(
                                &mut self.form.login,
                                LoginChoice::Basic,
                                "User name and password",
                            );
                        });
                        egui::Grid::new("add source login").num_columns(2).show(
                            ui,
                            |ui| match self.form.login {
                                LoginChoice::Basic => {
                                    ui.label("User");
                                    ui.add(
                                        egui::TextEdit::singleline(&mut self.form.user)
                                            .desired_width(260.0),
                                    );
                                    ui.end_row();
                                    ui.label("Password");
                                    ui.add(
                                        egui::TextEdit::singleline(&mut self.form.password)
                                            .password(true)
                                            .desired_width(260.0),
                                    );
                                    ui.end_row();
                                }
                                _ => {
                                    ui.label("Token");
                                    ui.add(
                                        egui::TextEdit::singleline(&mut self.form.token)
                                            .password(true)
                                            .desired_width(260.0),
                                    );
                                    ui.end_row();
                                }
                            },
                        );
                        ui.weak(format!(
                            "The login is kept in {}, and sent only to this address.",
                            self.vault.home().name()
                        ));
                    });
                // A closed disclosure means no login.
                match login.body_response {
                    None => self.form.login = LoginChoice::None,
                    Some(_) if self.form.login == LoginChoice::None => {
                        self.form.login = LoginChoice::Bearer;
                    }
                    Some(_) => {}
                }
            });
            if let Some(error) = &self.form.error {
                ui.colored_label(ui.visuals().error_fg_color, error);
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                submit = ui
                    .add_enabled(!busy, egui::Button::new("Check and add"))
                    .clicked();
                cancel = ui.button("Cancel").clicked();
                if busy {
                    ui.spinner();
                    ui.weak("Checking the address…");
                }
            });
        });
        if cancel || modal.should_close() {
            if let Some(id) = self.form.checking.take() {
                self.remove_source(&id);
                self.note = None;
            }
            self.form = Form::default();
        } else if submit {
            self.form.error = None;
            let (name, url, login) = (
                self.form.name.clone(),
                self.form.url.clone(),
                self.form.login(),
            );
            match self.add_remote(&name, &url, login) {
                Ok(id) => self.form.checking = Some(id),
                Err(why) => self.form.error = Some(why),
            }
        }
    }

    fn storage(&mut self, ui: &mut Ui) {
        ui.label(RichText::new("Downloaded sounds").strong());
        let total: u64 = self.sizes.values().sum();
        match self.cache.as_ref().map(|c| c.dir().display().to_string()) {
            Some(dir) => {
                ui.label(format!("{} in", size(total)));
                ui.label(RichText::new(dir).monospace().small());
            }
            None => {
                ui.colored_label(
                    ui.visuals().warn_fg_color,
                    "There is nowhere to keep downloaded sounds on this computer.",
                );
            }
        }
        ui.weak(
            "Each sound is kept once, however many sources offer it, so a sound two sources \
             share counts under both.",
        );
        let found = self.cleanable();
        if found.files == 0 {
            ui.weak("Nothing to clean up.");
            self.form.confirm_clean = false;
        } else if !self.form.confirm_clean {
            if ui
                .button(format!("Clean up ({})", size(found.bytes)))
                .on_hover_text("Remove downloads that no source offers any more")
                .clicked()
            {
                self.form.confirm_clean = true;
            }
        } else {
            ui.label(format!(
                "Remove {} files, {}? No source lists them, so a source you removed has to be \
                 added again to get them back.",
                found.files,
                size(found.bytes)
            ));
            ui.horizontal(|ui| {
                if ui.button("Remove them").clicked() {
                    self.clean_up();
                    self.form.confirm_clean = false;
                }
                if ui.button("Keep them").clicked() {
                    self.form.confirm_clean = false;
                }
            });
        }
    }
}
