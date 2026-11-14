//! The Sources page of Settings: where sounds come from (ADR 0012).
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

impl Sounds {
    /// The body of the Sources tab.
    pub fn body(&mut self, ui: &mut Ui) {
        // Opening the page is the first time a source may be asked.
        if !self.asked {
            self.refresh();
        }
        ui.weak(
            "Where the Sounds panel gets its sounds. What a source has already offered stays \
             listed when this computer is offline.",
        );
        ui.horizontal(|ui| {
            if ui
                .add_enabled(self.refreshing.is_none(), egui::Button::new("Refresh all"))
                .on_hover_text("Ask every source what it has now, and rescan the folders")
                .clicked()
            {
                self.refresh();
            }
            if self.refreshing.is_some() {
                ui.spinner();
            }
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
            self.source_block(ui, &source, &mut todo);
        }
        for action in todo {
            match action {
                Do::Remove(id) => self.remove_source(&id),
                Do::Licence(id, licence) => self.set_folder_licence(&id, licence),
            }
        }

        ui.separator();
        self.add_form(ui);
        ui.separator();
        self.storage(ui);
    }

    fn source_block(&self, ui: &mut Ui, source: &Source, todo: &mut Vec<Do>) {
        ui.horizontal(|ui| {
            ui.label(RichText::new(&source.name).strong());
            if source.folder.is_some() {
                ui.weak("Folder on this computer");
            } else if source.is_self_hosted() {
                ui.colored_label(ui.visuals().warn_fg_color, "⚠ Self-hosted");
            } else {
                ui.weak("Built in");
            }
        });
        let place = match &source.folder {
            Some(folder) => folder.path.display().to_string(),
            None => source.url.clone(),
        };
        ui.label(RichText::new(place).monospace().small());
        ui.horizontal(|ui| {
            match self.reach.get(&source.id) {
                Some(Reach::Reachable) => ui.label("Reachable"),
                Some(Reach::Unreachable(why)) => {
                    ui.colored_label(crate::theme::MUTE, format!("⚠ Can't be reached: {why}"))
                }
                _ => ui.weak("Asking…"),
            };
            let usage = self.usage(&source.id);
            if source.folder.is_some() {
                ui.weak(format!(
                    "{} sounds, read in place: nothing copied",
                    usage.sounds
                ));
            } else {
                ui.weak(format!(
                    "{} sounds, {} downloaded, {} on this computer",
                    usage.sounds,
                    usage.downloaded,
                    size(usage.bytes)
                ));
            }
        });
        if source.is_self_hosted() {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                "Noodle can't vouch for what this source contains.",
            );
        }
        if let Some(folder) = &source.folder {
            ui.horizontal(|ui| {
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
        if !source.is_built_in()
            && ui
                .button("Remove")
                .on_hover_text(if source.folder.is_some() {
                    "Stop listing this folder. Its files are not touched"
                } else {
                    "Forget this source and its login. Sounds already downloaded stay until Clean up"
                })
                .clicked()
        {
            todo.push(Do::Remove(source.id.clone()));
        }
    }

    fn add_form(&mut self, ui: &mut Ui) {
        ui.label(RichText::new("Add a source").strong());
        ui.colored_label(ui.visuals().warn_fg_color, NO_PROMISE);
        ui.weak(
            "A source is a folder of files on any web host: an index.json, a manifest for each \
             pack and the audio.",
        );
        egui::Grid::new("add source").num_columns(2).show(ui, |ui| {
            ui.label("Name");
            ui.add(
                egui::TextEdit::singleline(&mut self.form.name)
                    .hint_text("Optional")
                    .desired_width(260.0),
            );
            ui.end_row();
            ui.label("Address");
            ui.add(
                egui::TextEdit::singleline(&mut self.form.url)
                    .hint_text("https://example.com/sounds")
                    .desired_width(260.0),
            );
            ui.end_row();
            ui.label("Login");
            egui::ComboBox::from_id_salt("source login")
                .selected_text(match self.form.login {
                    LoginChoice::None => "None",
                    LoginChoice::Bearer => "Token",
                    LoginChoice::Basic => "User and password",
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.form.login, LoginChoice::None, "None");
                    ui.selectable_value(&mut self.form.login, LoginChoice::Bearer, "Token");
                    ui.selectable_value(
                        &mut self.form.login,
                        LoginChoice::Basic,
                        "User and password",
                    );
                });
            ui.end_row();
            match self.form.login {
                LoginChoice::None => {}
                LoginChoice::Bearer => {
                    ui.label("Token");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.form.token)
                            .password(true)
                            .desired_width(260.0),
                    );
                    ui.end_row();
                }
                LoginChoice::Basic => {
                    ui.label("User");
                    ui.add(egui::TextEdit::singleline(&mut self.form.user).desired_width(260.0));
                    ui.end_row();
                    ui.label("Password");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.form.password)
                            .password(true)
                            .desired_width(260.0),
                    );
                    ui.end_row();
                }
            }
        });
        if self.form.login != LoginChoice::None {
            ui.weak(format!(
                "The login is kept in {}, and sent only to this address.",
                self.vault.home().name()
            ));
        }
        if let Some(error) = &self.form.error {
            ui.colored_label(ui.visuals().error_fg_color, error);
        }
        ui.horizontal(|ui| {
            if ui.button("Add source").clicked() {
                let login = self.form.login();
                match self.add_remote(&self.form.name.clone(), &self.form.url.clone(), login) {
                    Ok(_) => self.form = Form::default(),
                    Err(why) => self.form.error = Some(why),
                }
            }
            if ui
                .button("Add folder…")
                .on_hover_text("Use a folder of audio files as a source, read where it is")
                .clicked()
                && let Some(path) = crate::picker::pick_folder()
                && let Err(why) = self.add_folder(path)
            {
                self.form.error = Some(why);
            }
        });
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
