//! The plugin browser: every plugin found, searchable, with a rescan button.
//!
//! It only asks: choosing a plugin returns the edit that adds its node.

use egui::{RichText, Ui};
use noodle_core::{Command, Node};

use crate::session::{Edit, Session};

#[derive(Default)]
pub struct PluginBrowser {
    open: bool,
    query: String,
    /// Plugins added so far, to stagger where the next one lands.
    added: usize,
}

/// The plugins matching every word of `query` by name, vendor, ID or format, with
/// those whose name matches first.
fn matches<'a>(
    plugins: impl Iterator<Item = &'a noodle_plugins::Descriptor>,
    query: &str,
) -> Vec<&'a noodle_plugins::Descriptor> {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    let mut found: Vec<_> = plugins
        .filter(|p| {
            let haystack = format!(
                "{} {} {} {} {}",
                p.name,
                p.vendor,
                p.id,
                p.format.name(),
                if p.instrument { "instrument" } else { "effect" }
            )
            .to_lowercase();
            words.iter().all(|w| haystack.contains(w.as_str()))
        })
        .collect();
    found.sort_by_key(|p| {
        let name = p.name.to_lowercase();
        let by_name = words.iter().all(|w| name.contains(w.as_str()));
        (!by_name, name)
    });
    found
}

impl PluginBrowser {
    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn toggle(&mut self) {
        self.open = !self.open;
    }

    pub fn show(&mut self, ctx: &egui::Context, session: &mut Session) -> Vec<Edit> {
        let mut edits = Vec::new();
        if !self.open {
            return edits;
        }
        let mut open = true;
        egui::Window::new("Plugins")
            .open(&mut open)
            .default_width(380.0)
            .default_height(420.0)
            .show(ctx, |ui| self.contents(ui, session, &mut edits));
        self.open = open;
        edits
    }

    fn contents(&mut self, ui: &mut Ui, session: &mut Session, edits: &mut Vec<Edit>) {
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.query)
                    .hint_text("Search plugins")
                    .desired_width(ui.available_width() - 90.0),
            );
            let scanning = session.plugin_scanning();
            let rescan = ui
                .add_enabled(!scanning, egui::Button::new("Rescan"))
                .on_hover_text(
                    "Look for new and changed plugins. Shift-click scans every file again.",
                );
            if rescan.clicked() {
                let everything = ui.input(|i| i.modifiers.shift);
                session.rescan_plugins(everything);
            }
        });
        let mut sandboxed = session.sandbox_plugins();
        if ui
            .checkbox(&mut sandboxed, "Run plugins in separate processes")
            .on_hover_text(
                "A plugin that crashes or hangs then plays silence and is started \
                 again, instead of taking the app with it. Costs a little CPU for each \
                 plugin. Editors of plugins in separate processes only work on Linux \
                 so far.",
            )
            .changed()
        {
            session.set_sandbox_plugins(sandboxed);
            crate::prefs::remember_sandbox(sandboxed);
        }
        if session.plugin_scanning() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.weak("Scanning…");
            });
        }
        let found = matches(
            session.plugins().iter().map(|p| p.descriptor()),
            &self.query,
        );
        ui.weak(format!(
            "{} of {} plugins",
            found.len(),
            session.plugins().len()
        ));
        let mut chosen = None;
        egui::ScrollArea::vertical()
            .max_height(ui.available_height() - 60.0)
            .show(ui, |ui| {
                for plugin in &found {
                    ui.horizontal(|ui| {
                        if ui.button("Add").clicked() {
                            chosen =
                                Some((plugin.node_id(), noodle_plugins::initial_params(plugin)));
                        }
                        ui.vertical(|ui| {
                            ui.label(RichText::new(&plugin.name).strong());
                            ui.weak(format!(
                                "{} · {} · {}",
                                plugin.format.name(),
                                if plugin.vendor.is_empty() {
                                    "unknown vendor"
                                } else {
                                    &plugin.vendor
                                },
                                if plugin.instrument {
                                    "instrument"
                                } else {
                                    "effect"
                                }
                            ));
                        });
                    });
                    ui.separator();
                }
            });
        drop(found);
        if let Some((type_id, initial)) = chosen {
            let id = session.new_node_id();
            let offset = 30.0 * (self.added % 8) as f32;
            self.added += 1;
            let mut node = Node::new(type_id).at(60.0 + offset, 60.0 + offset);
            // A synth with hundreds of parameters starts with none on the node.
            if let Some(initial) = initial {
                node.config.set(
                    noodle_plugins::PARAMS_KEY.to_owned(),
                    noodle_core::Value::Text(initial),
                );
            }
            edits.push(Edit::Apply(Command::AddNode { id, node }));
        }
        let failed: Vec<_> = session.failed_plugin_files().collect();
        if !failed.is_empty() {
            ui.collapsing(
                format!("{} file(s) couldn't be scanned", failed.len()),
                |ui| {
                    for (path, why) in failed {
                        ui.label(path.display().to_string()).on_hover_text(why);
                        ui.weak(why);
                    }
                },
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_plugins::Descriptor;

    fn plugin(id: &str, name: &str, vendor: &str, instrument: bool) -> Descriptor {
        Descriptor {
            format: noodle_plugins::PluginFormat::Clap,
            id: id.into(),
            name: name.into(),
            vendor: vendor.into(),
            instrument,
            audio_inputs: Vec::new(),
            audio_outputs: Vec::new(),
            note_inputs: 0,
            note_outputs: 0,
            params: Vec::new(),
        }
    }

    fn names<'a>(found: &[&'a Descriptor]) -> Vec<&'a str> {
        found.iter().map(|p| p.name.as_str()).collect()
    }

    #[test]
    fn search_matches_every_word_by_name_vendor_id_or_kind() {
        let all = [
            plugin("org.surge.xt", "Surge XT", "Surge Synth Team", true),
            plugin("com.acme.verb", "Big Verb", "Acme", false),
            plugin("com.acme.surgeon", "Acme Surgeon", "Acme", false),
        ];
        assert_eq!(
            names(&matches(all.iter(), "")),
            ["Acme Surgeon", "Big Verb", "Surge XT"]
        );
        // Names that match come before ones that match only by vendor.
        assert_eq!(
            names(&matches(all.iter(), "surge")),
            ["Acme Surgeon", "Surge XT"]
        );
        assert_eq!(
            names(&matches(all.iter(), "acme effect")),
            ["Acme Surgeon", "Big Verb"]
        );
        assert_eq!(names(&matches(all.iter(), "instrument")), ["Surge XT"]);
        assert!(matches(all.iter(), "nothing like this").is_empty());
    }
}
