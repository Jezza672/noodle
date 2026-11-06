//! The command palette and the keymap.
//!
//! Every action of the menus and the transport is registered once, in
//! [`ENTRIES`], with a name. The palette (Cmd+K) searches them by name, so a
//! feature that is not a node can be found by typing what it does, and the
//! keymap binds a key chord to any of them. The keymap is a preference:
//! only the chords the user changed are saved, so a new default reaches
//! everyone who did not touch that command.

use std::collections::BTreeMap;

use egui::{Key, KeyboardShortcut, Modifiers};

use super::Action;
use super::SelectedClipAction;
use crate::session::convert::Detect;
use crate::timeline::ClipAction;

/// A named action the palette can run and a key can be bound to.
pub struct Entry {
    /// What a saved keymap calls it. Never changes.
    pub id: &'static str,
    /// What the palette searches and lists.
    pub name: &'static str,
    pub action: Action,
    pub default: Option<KeyboardShortcut>,
    /// Whether the app catches the default itself. A view that handles its
    /// own keys with the pointer over it (the arrangement's Cmd+C, the
    /// canvas's F) keeps doing so, so a user's binding for it is caught
    /// here as well.
    pub global: bool,
}

const fn chord(modifiers: Modifiers, key: Key) -> Option<KeyboardShortcut> {
    Some(KeyboardShortcut::new(modifiers, key))
}

const CMD: Modifiers = Modifiers::COMMAND;
const NONE: Modifiers = Modifiers::NONE;
const CMD_SHIFT: Modifiers = Modifiers::COMMAND.plus(Modifiers::SHIFT);

const fn entry(
    id: &'static str,
    name: &'static str,
    action: Action,
    default: Option<KeyboardShortcut>,
    global: bool,
) -> Entry {
    Entry {
        id,
        name,
        action,
        default,
        global,
    }
}

pub static ENTRIES: &[Entry] = &[
    entry(
        "file.new",
        "New Project",
        Action::New,
        chord(CMD, Key::N),
        true,
    ),
    entry(
        "file.open",
        "Open Project…",
        Action::Open,
        chord(CMD, Key::O),
        true,
    ),
    entry("file.save", "Save", Action::Save, chord(CMD, Key::S), true),
    entry(
        "file.save_as",
        "Save As…",
        Action::SaveAs,
        chord(CMD_SHIFT, Key::S),
        true,
    ),
    entry(
        "file.save_template",
        "Save as Project Template…",
        Action::SaveTemplate,
        None,
        false,
    ),
    entry(
        "file.import",
        "Import Audio…",
        Action::ImportAudio,
        None,
        false,
    ),
    entry(
        "file.export",
        "Export Audio…",
        Action::ExportAudio,
        None,
        false,
    ),
    entry(
        "file.settings",
        "Audio Settings…",
        Action::AudioSettings,
        chord(CMD, Key::Comma),
        true,
    ),
    entry("edit.undo", "Undo", Action::Undo, chord(CMD, Key::Z), true),
    entry(
        "edit.redo",
        "Redo",
        Action::Redo,
        chord(CMD_SHIFT, Key::Z),
        true,
    ),
    entry(
        "edit.cut",
        "Cut",
        Action::Clips(ClipAction::Cut),
        chord(CMD, Key::X),
        false,
    ),
    entry(
        "edit.copy",
        "Copy",
        Action::Clips(ClipAction::Copy),
        chord(CMD, Key::C),
        false,
    ),
    entry(
        "edit.paste",
        "Paste",
        Action::Clips(ClipAction::Paste),
        chord(CMD, Key::V),
        false,
    ),
    entry(
        "edit.duplicate",
        "Duplicate",
        Action::Clips(ClipAction::Duplicate),
        chord(CMD, Key::D),
        false,
    ),
    entry(
        "edit.delete",
        "Delete Selection",
        Action::DeleteSelection,
        chord(NONE, Key::X),
        false,
    ),
    entry(
        "edit.arrange",
        "Arrange Nodes",
        Action::ArrangeNodes,
        chord(CMD, Key::L),
        false,
    ),
    entry(
        "view.fit",
        "Fit Graph",
        Action::FitGraph,
        chord(NONE, Key::F),
        false,
    ),
    entry(
        "view.palette",
        "Command Palette…",
        Action::OpenPalette,
        chord(CMD, Key::K),
        true,
    ),
    entry(
        "view.mixer",
        "Show or Hide Mixer",
        Action::ToggleMixer,
        None,
        false,
    ),
    entry(
        "view.scope",
        "Show or Hide Scope",
        Action::ToggleScope,
        None,
        false,
    ),
    entry(
        "view.outputs",
        "Show or Hide Outputs",
        Action::ToggleOutputs,
        None,
        false,
    ),
    entry(
        "view.plugins",
        "Show or Hide Plugins",
        Action::TogglePlugins,
        None,
        false,
    ),
    entry(
        "view.cache",
        "Show or Hide Playback Cache",
        Action::ToggleCache,
        None,
        false,
    ),
    entry(
        "view.cpu",
        "Show or Hide CPU Meters",
        Action::ToggleCpu,
        None,
        false,
    ),
    entry(
        "view.tempo_lane",
        "Show or Hide Tempo Lane",
        Action::ToggleTempoLane,
        None,
        false,
    ),
    entry(
        "tracks.fold",
        "Fold All Folders",
        Action::FoldFolders(true),
        None,
        false,
    ),
    entry(
        "clip.stems",
        "Split Selected Clip to Stems",
        Action::OnSelectedClip(SelectedClipAction::SplitToStems),
        None,
        false,
    ),
    entry(
        "clip.slice",
        "Slice Selected Clip to Sampler",
        Action::OnSelectedClip(SelectedClipAction::SliceToSampler),
        None,
        false,
    ),
    entry(
        "clip.pin",
        "Pin Transients on Selected Clip",
        Action::OnSelectedClip(SelectedClipAction::PinTransients),
        None,
        false,
    ),
    entry(
        "clip.midi",
        "Convert Selected Clip to MIDI",
        Action::OnSelectedClip(SelectedClipAction::ConvertToMidi(Detect::Normal)),
        None,
        false,
    ),
    entry(
        "export.version",
        "Add Mix Version (export the song in several mixes)",
        Action::AddMixVersion,
        None,
        false,
    ),
    entry(
        "tracks.unfold",
        "Unfold All Folders",
        Action::FoldFolders(false),
        None,
        false,
    ),
    entry(
        "transport.play",
        "Play or Stop",
        Action::TogglePlayback,
        chord(NONE, Key::Space),
        true,
    ),
    entry("transport.rewind", "Rewind", Action::Rewind, None, false),
    entry(
        "transport.record",
        "Record",
        Action::ToggleRecord,
        None,
        false,
    ),
    entry(
        "transport.metronome",
        "Metronome",
        Action::ToggleMetronome,
        None,
        false,
    ),
    entry(
        "transport.engine",
        "Audio Engine On or Off",
        Action::ToggleEngine,
        None,
        false,
    ),
    entry(
        "keyboard.midi",
        "Computer Keyboard as MIDI",
        Action::ToggleKeyboard,
        chord(NONE, Key::M),
        true,
    ),
];

/// A chord as text, such as `Cmd+Shift+S`, for the saved keymap. `Cmd` is
/// Ctrl on Windows and Linux.
pub fn format(shortcut: &KeyboardShortcut) -> String {
    let m = shortcut.modifiers;
    let mut text = String::new();
    for (on, part) in [(m.command, "Cmd"), (m.alt, "Alt"), (m.shift, "Shift")] {
        if on {
            text.push_str(part);
            text.push('+');
        }
    }
    text.push_str(shortcut.logical_key.name());
    text
}

pub fn parse(text: &str) -> Option<KeyboardShortcut> {
    let mut modifiers = Modifiers::NONE;
    let mut parts = text.split('+').peekable();
    let mut key = None;
    while let Some(part) = parts.next() {
        if parts.peek().is_some() {
            match part {
                "Cmd" => modifiers.command = true,
                "Alt" => modifiers.alt = true,
                "Shift" => modifiers.shift = true,
                _ => return None,
            }
        } else {
            key = Key::from_name(part);
        }
    }
    Some(KeyboardShortcut::new(modifiers, key?))
}

/// The chords the user has changed from the defaults.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Keymap {
    /// Entry id to chord text. Empty text is a command with no key.
    overrides: BTreeMap<String, String>,
}

impl Keymap {
    pub fn from_saved(saved: &BTreeMap<String, String>) -> Self {
        let known = |id: &String| ENTRIES.iter().any(|e| e.id == id);
        Self {
            overrides: saved
                .iter()
                .filter(|(id, text)| known(id) && (text.is_empty() || parse(text).is_some()))
                .map(|(id, text)| (id.clone(), text.clone()))
                .collect(),
        }
    }

    pub fn saved(&self) -> &BTreeMap<String, String> {
        &self.overrides
    }

    /// The chord that runs `entry`: the user's, or else its default.
    pub fn chord(&self, entry: &Entry) -> Option<KeyboardShortcut> {
        match self.overrides.get(entry.id) {
            Some(text) => parse(text),
            None => entry.default,
        }
    }

    /// Whether the user has changed `entry`'s key.
    pub fn is_changed(&self, entry: &Entry) -> bool {
        self.overrides.contains_key(entry.id)
    }

    /// Binds `chord` to `id`, or unbinds it with `None`. A chord belongs to
    /// one command: the one that had it loses it.
    pub fn bind(&mut self, id: &str, chord: Option<KeyboardShortcut>) {
        let Some(entry) = ENTRIES.iter().find(|e| e.id == id) else {
            return;
        };
        if let Some(chord) = chord {
            let losers: Vec<&Entry> = ENTRIES
                .iter()
                .filter(|e| e.id != id && self.chord(e).as_ref() == Some(&chord))
                .collect();
            for loser in losers {
                self.overrides.insert(loser.id.to_owned(), String::new());
            }
        }
        let text = chord.as_ref().map(format).unwrap_or_default();
        if chord == entry.default {
            self.overrides.remove(id);
        } else {
            self.overrides.insert(id.to_owned(), text);
        }
    }

    /// Puts `id` back to its default key, which takes it from whoever holds
    /// it now.
    pub fn reset(&mut self, id: &str) {
        let Some(entry) = ENTRIES.iter().find(|e| e.id == id) else {
            return;
        };
        self.overrides.remove(id);
        if let Some(default) = entry.default {
            self.bind(id, Some(default));
        }
    }

    /// The chords the app catches itself, with the command each runs: the
    /// ones with more modifiers first, since Cmd+Z would also match
    /// Cmd+Shift+Z.
    pub fn global(&self) -> Vec<(KeyboardShortcut, &'static Entry)> {
        let mut bound: Vec<(KeyboardShortcut, &'static Entry)> = ENTRIES
            .iter()
            .filter(|e| self.is_changed(e) || e.global)
            .filter_map(|e| Some((self.chord(e)?, e)))
            .collect();
        let weight =
            |m: Modifiers| usize::from(m.command) + usize::from(m.alt) + usize::from(m.shift);
        bound.sort_by_key(|(c, _)| std::cmp::Reverse(weight(c.modifiers)));
        bound
    }

    #[cfg(test)]
    pub fn find(&self, id: &str) -> Option<&'static Entry> {
        ENTRIES.iter().find(|e| e.id == id)
    }
}

/// Whether `query` finds `name`: every letter of it, in order, with matches
/// that run together or start words ranked first. `None` is no match.
pub fn score(query: &str, name: &str) -> Option<usize> {
    let name = name.to_lowercase();
    let mut at = 0;
    let mut cost = 0;
    for letter in query.to_lowercase().chars().filter(|c| !c.is_whitespace()) {
        let found = name[at..].find(letter)?;
        let start_of_word = at + found == 0 || !name[..at + found].ends_with(char::is_alphanumeric);
        cost += if found == 0 || start_of_word {
            0
        } else {
            1 + found
        };
        at += found + letter.len_utf8();
    }
    Some(cost)
}

/// What the palette is doing.
#[derive(Debug, Default)]
pub struct Palette {
    open: bool,
    query: String,
    selected: usize,
    /// The list of keys, to rebind, instead of the list to run.
    keys: bool,
    /// The command waiting for the next chord pressed.
    capturing: Option<&'static str>,
}

/// What the palette asks the app to do.
#[derive(Default)]
pub struct Out {
    pub run: Option<Action>,
    /// The keymap was changed; save it.
    pub keymap_changed: bool,
}

/// A command in the list: a registered entry, or one made for the moment,
/// such as a template to start from.
struct Row<'a> {
    name: String,
    action: Action,
    id: Option<&'a Entry>,
}

impl Palette {
    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn open(&mut self) {
        *self = Self {
            open: true,
            ..Self::default()
        };
    }

    pub fn close(&mut self) {
        self.open = false;
        self.capturing = None;
    }

    /// The commands the query finds, best first. `extra` are the commands
    /// that exist only now (the project templates).
    fn found<'a>(&self, extra: &[(String, Action)]) -> Vec<Row<'a>> {
        let mut rows: Vec<(usize, Row<'a>)> = ENTRIES
            .iter()
            .map(|e| Row {
                name: e.name.to_owned(),
                action: e.action,
                id: Some(e),
            })
            .chain(extra.iter().map(|(name, action)| Row {
                name: name.clone(),
                action: *action,
                id: None,
            }))
            .filter_map(|row| Some((score(&self.query, &row.name)?, row)))
            .collect();
        rows.sort_by_key(|(cost, _)| *cost);
        rows.into_iter().map(|(_, row)| row).collect()
    }

    pub fn show(
        &mut self,
        ctx: &egui::Context,
        keymap: &mut Keymap,
        extra: &[(String, Action)],
    ) -> Out {
        let mut out = Out::default();
        if !self.open {
            return out;
        }
        // The next chord pressed is the new binding, whatever it is.
        if let Some(id) = self.capturing {
            let pressed = ctx.input(|i| {
                i.events.iter().find_map(|event| match event {
                    egui::Event::Key {
                        key,
                        pressed: true,
                        repeat: false,
                        modifiers,
                        ..
                    } => Some((*key, *modifiers)),
                    _ => None,
                })
            });
            if let Some((key, modifiers)) = pressed {
                self.capturing = None;
                if key != Key::Escape {
                    let modifiers = Modifiers {
                        command: modifiers.command,
                        alt: modifiers.alt,
                        shift: modifiers.shift,
                        ..Modifiers::NONE
                    };
                    keymap.bind(id, Some(KeyboardShortcut::new(modifiers, key)));
                    out.keymap_changed = true;
                }
                return out;
            }
        }
        let rows = self.found(extra);
        self.selected = self.selected.min(rows.len().saturating_sub(1));
        let (up, down, enter, escape) = if self.capturing.is_some() {
            (false, false, false, false)
        } else {
            ctx.input_mut(|i| {
                (
                    i.consume_key(Modifiers::NONE, Key::ArrowUp),
                    i.consume_key(Modifiers::NONE, Key::ArrowDown),
                    i.consume_key(Modifiers::NONE, Key::Enter),
                    i.consume_key(Modifiers::NONE, Key::Escape),
                )
            })
        };
        if up {
            self.selected = self.selected.saturating_sub(1);
        }
        if down {
            self.selected = (self.selected + 1).min(rows.len().saturating_sub(1));
        }
        if escape {
            self.close();
            return out;
        }
        if enter && !self.keys {
            if let Some(row) = rows.get(self.selected) {
                out.run = Some(row.action);
            }
            self.close();
            return out;
        }
        egui::Modal::new(egui::Id::new("command palette")).show(ctx, |ui| {
            ui.set_width(420.0);
            ui.horizontal(|ui| {
                ui.heading(if self.keys {
                    "Keyboard shortcuts"
                } else {
                    "Commands"
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let label = if self.keys {
                        "Back to commands"
                    } else {
                        "Edit shortcuts"
                    };
                    if ui.button(label).clicked() {
                        self.keys = !self.keys;
                        self.capturing = None;
                    }
                });
            });
            let field = ui.add(
                egui::TextEdit::singleline(&mut self.query)
                    .hint_text("Type what you want to do")
                    .desired_width(f32::INFINITY),
            );
            if !field.has_focus() && self.capturing.is_none() {
                field.request_focus();
            }
            if field.changed() {
                self.selected = 0;
            }
            ui.add_space(4.0);
            egui::ScrollArea::vertical()
                .max_height(320.0)
                .show(ui, |ui| {
                    if rows.is_empty() {
                        ui.weak("No command matches");
                    }
                    for (n, row) in rows.iter().enumerate() {
                        let chord = row.id.and_then(|e| keymap.chord(e));
                        let shown = chord.map_or(String::new(), |c| ui.ctx().format_shortcut(&c));
                        if self.keys {
                            self.key_row(ui, row, &shown, keymap, &mut out);
                        } else {
                            let response = ui
                                .horizontal(|ui| {
                                    let r = ui.selectable_label(n == self.selected, &row.name);
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| ui.weak(shown),
                                    );
                                    r
                                })
                                .inner;
                            if n == self.selected {
                                response.scroll_to_me(None);
                            }
                            if response.clicked() {
                                out.run = Some(row.action);
                            }
                        }
                    }
                });
        });
        if out.run.is_some() {
            self.close();
        }
        out
    }

    /// One row of the list of keys: the key, and buttons to change it.
    fn key_row(
        &mut self,
        ui: &mut egui::Ui,
        row: &Row<'_>,
        shown: &str,
        keymap: &mut Keymap,
        out: &mut Out,
    ) {
        let Some(entry) = row.id else {
            return;
        };
        ui.horizontal(|ui| {
            ui.label(&row.name);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if self.capturing == Some(entry.id) {
                    ui.label("Press the new key (Esc cancels)");
                    return;
                }
                if keymap.is_changed(entry) && ui.small_button("Reset").clicked() {
                    keymap.reset(entry.id);
                    out.keymap_changed = true;
                }
                if keymap.chord(entry).is_some() && ui.small_button("Clear").clicked() {
                    keymap.bind(entry.id, None);
                    out.keymap_changed = true;
                }
                if ui.small_button("Set").clicked() {
                    self.capturing = Some(entry.id);
                }
                ui.weak(if shown.is_empty() { "none" } else { shown });
            });
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_entry_has_its_own_id_and_name_and_no_two_share_a_default_key() {
        for (i, a) in ENTRIES.iter().enumerate() {
            for b in &ENTRIES[i + 1..] {
                assert_ne!(a.id, b.id);
                assert_ne!(a.name, b.name);
                if a.default.is_some() {
                    assert_ne!(a.default, b.default, "{} and {}", a.id, b.id);
                }
            }
        }
    }

    #[test]
    fn a_chord_round_trips_through_text() {
        for entry in ENTRIES {
            if let Some(chord) = entry.default {
                assert_eq!(parse(&format(&chord)), Some(chord), "{}", entry.id);
            }
        }
        assert_eq!(parse("Cmd+Nonsense"), None);
        assert_eq!(parse("Wat+K"), None);
    }

    #[test]
    fn the_query_finds_letters_in_order_and_prefers_words_that_start_with_them() {
        assert!(score("fold", "Fold All Folders").is_some());
        assert!(score("faf", "Fold All Folders").is_some());
        assert_eq!(score("zzz", "Fold All Folders"), None);
        assert!(score("save", "Save").unwrap() < score("save", "Open Project…").unwrap_or(99));
        // A word that starts with the letters beats a scatter of them.
        assert!(
            score("mix", "Show or Hide Mixer") < score("mix", "Computer Keyboard as MIDI Extra")
        );
    }

    #[test]
    fn rebinding_takes_the_chord_from_its_old_owner_and_reset_gives_it_back() {
        let mut keymap = Keymap::default();
        let save = keymap.find("file.save").unwrap();
        let chord = save.default.unwrap();
        keymap.bind("file.new", Some(chord));
        assert_eq!(keymap.chord(keymap.find("file.new").unwrap()), Some(chord));
        assert_eq!(keymap.chord(save), None, "Save lost it");
        keymap.reset("file.new");
        keymap.reset("file.save");
        assert_eq!(keymap, Keymap::default());
    }

    #[test]
    fn an_unknown_or_unreadable_saved_binding_is_ignored() {
        let saved = BTreeMap::from([
            ("file.save".to_owned(), "Cmd+J".to_owned()),
            ("gone".to_owned(), "Cmd+K".to_owned()),
            ("file.new".to_owned(), "garbage+".to_owned()),
        ]);
        let keymap = Keymap::from_saved(&saved);
        assert_eq!(keymap.saved().len(), 1);
    }

    #[test]
    fn a_rebound_command_is_caught_by_the_app_and_longer_chords_come_first() {
        let mut keymap = Keymap::default();
        keymap.bind("view.mixer", Some(KeyboardShortcut::new(CMD, Key::M)));
        let global = keymap.global();
        assert!(global.iter().any(|(_, e)| e.id == "view.mixer"));
        let redo = global
            .iter()
            .position(|(_, e)| e.id == "edit.redo")
            .unwrap();
        let undo = global
            .iter()
            .position(|(_, e)| e.id == "edit.undo")
            .unwrap();
        assert!(redo < undo);
        // Cut is handled by the arrangement, so it is not caught here until
        // the user binds it.
        assert!(!global.iter().any(|(_, e)| e.id == "edit.cut"));
    }
}
