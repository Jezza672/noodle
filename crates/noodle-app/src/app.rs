//! The window: menus, the toolbar, the panels, and keyboard shortcuts.

use std::path::PathBuf;

use egui::{Key, KeyboardShortcut, Modifiers};

use crate::editor::{self, EditorState};
use crate::session::Session;
use crate::{properties, theme};

const UNDO: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::Z);
const REDO: KeyboardShortcut =
    KeyboardShortcut::new(Modifiers::COMMAND.plus(Modifiers::SHIFT), Key::Z);
const OPEN: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::O);
const SAVE: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::S);
const SAVE_AS: KeyboardShortcut =
    KeyboardShortcut::new(Modifiers::COMMAND.plus(Modifiers::SHIFT), Key::S);
const NEW: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::N);

pub struct App {
    session: Session,
    editor: EditorState,
    /// The title last sent to the window, so it's only sent when it changes.
    title: String,
}

/// Something that needs a file dialog, which the UI pass can't open itself.
enum FileAction {
    Open,
    SaveAs,
}

impl App {
    pub fn new(session: Session) -> Self {
        Self {
            session,
            editor: EditorState::default(),
            title: String::new(),
        }
    }

    #[cfg(test)]
    pub fn session(&self) -> &Session {
        &self.session
    }

    /// Draws the whole window. Separate from [`eframe::App`] so tests can
    /// drive it without a window.
    pub fn show(&mut self, ui: &mut egui::Ui) {
        self.session.maintain();
        let mut file_action = self.shortcuts(ui.ctx());

        egui::Panel::top("menu").show(ui, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                file_action = file_action.take().or_else(|| self.menus(ui));
                ui.separator();
                self.transport(ui);
            });
        });

        egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui));

        let active = self.editor.active;
        egui::Panel::right("properties")
            .default_size(theme::PROPERTIES_WIDTH)
            .show(ui, |ui| {
                ui.add_space(4.0);
                let edits = properties::show(ui, &self.session, active);
                self.session.edit(edits);
            });

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(theme::CANVAS))
            .show(ui, |ui| {
                let edits = editor::show(ui, &mut self.editor, &self.session);
                self.session.edit(edits);
            });

        self.editor.retain_existing(&self.session);
        self.update_title(ui.ctx());
        if self.session.is_playing() {
            // Keeps health checks and plan freeing going while idle.
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(100));
        }
        if let Some(action) = file_action {
            self.run_file_action(action);
        }
    }

    fn shortcuts(&mut self, ctx: &egui::Context) -> Option<FileAction> {
        let mut action = None;
        ctx.input_mut(|input| {
            // Redo first: its shortcut contains undo's.
            if input.consume_shortcut(&REDO) {
                self.session.redo();
            }
            if input.consume_shortcut(&UNDO) {
                self.session.undo();
            }
            if input.consume_shortcut(&SAVE_AS) {
                action = Some(FileAction::SaveAs);
            }
            if input.consume_shortcut(&SAVE) && !self.session.save() {
                action = Some(FileAction::SaveAs);
            }
            if input.consume_shortcut(&OPEN) {
                action = Some(FileAction::Open);
            }
            if input.consume_shortcut(&NEW) {
                self.new_project();
            }
        });
        // Space toggles playback, unless a text field has it.
        if !ctx.egui_wants_keyboard_input() && ctx.input(|i| i.key_pressed(Key::Space)) {
            self.toggle_playback();
        }
        action
    }

    fn menus(&mut self, ui: &mut egui::Ui) -> Option<FileAction> {
        let mut action = None;
        let ctx = ui.ctx().clone();
        ui.menu_button("File", |ui| {
            if ui.add(button("New", &NEW, &ctx)).clicked() {
                self.new_project();
            }
            if ui.add(button("Open…", &OPEN, &ctx)).clicked() {
                action = Some(FileAction::Open);
            }
            if ui.add(button("Save", &SAVE, &ctx)).clicked() && !self.session.save() {
                action = Some(FileAction::SaveAs);
            }
            if ui.add(button("Save As…", &SAVE_AS, &ctx)).clicked() {
                action = Some(FileAction::SaveAs);
            }
        });
        ui.menu_button("Edit", |ui| {
            let undo = button("Undo", &UNDO, &ctx);
            if ui.add_enabled(self.session.can_undo(), undo).clicked() {
                self.session.undo();
            }
            let redo = button("Redo", &REDO, &ctx);
            if ui.add_enabled(self.session.can_redo(), redo).clicked() {
                self.session.redo();
            }
        });
        action
    }

    fn transport(&mut self, ui: &mut egui::Ui) {
        let label = if self.session.is_playing() {
            "⏹ Stop"
        } else {
            "▶ Play"
        };
        if ui
            .button(label)
            .on_hover_text("Play through the default output device (Space)")
            .clicked()
        {
            self.toggle_playback();
        }
    }

    fn status_bar(&self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let problems = self.session.diagnostics().len();
            match problems {
                0 => ui.weak("No problems"),
                1 => ui.colored_label(ui.visuals().warn_fg_color, "1 problem"),
                n => ui.colored_label(ui.visuals().warn_fg_color, format!("{n} problems")),
            }
            .on_hover_ui(|ui| {
                for diagnostic in self.session.diagnostics() {
                    ui.label(diagnostic.to_string());
                }
            });
            if let Some(message) = self.session.message() {
                ui.separator();
                ui.label(message);
            }
        });
    }

    fn toggle_playback(&mut self) {
        if self.session.is_playing() {
            self.session.stop();
        } else {
            self.session.play();
        }
    }

    fn new_project(&mut self) {
        let registry = crate::registry();
        let playing = self.session.is_playing();
        self.session = Session::new(registry);
        self.editor = EditorState::default();
        if playing {
            self.session.play();
        }
    }

    fn run_file_action(&mut self, action: FileAction) {
        let dialog = rfd::FileDialog::new().add_filter("Noodle project", &["ron"]);
        match action {
            FileAction::Open => {
                if let Some(path) = dialog.pick_file() {
                    self.session.load(&path);
                    self.editor = EditorState::default();
                }
            }
            FileAction::SaveAs => {
                let name = format!("{}.ron", self.session.name());
                if let Some(path) = dialog.set_file_name(name).save_file() {
                    self.session.save_as(&with_extension(path));
                }
            }
        }
    }

    fn update_title(&mut self, ctx: &egui::Context) {
        let dirty = if self.session.is_dirty() { " •" } else { "" };
        let title = format!("{}{dirty} — Noodle", self.session.name());
        if title != self.title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.title = title;
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.show(ui);
    }
}

/// A menu item showing its shortcut.
fn button<'a>(text: &'a str, shortcut: &KeyboardShortcut, ctx: &egui::Context) -> egui::Button<'a> {
    egui::Button::new(text).shortcut_text(ctx.format_shortcut(shortcut))
}

/// Adds `.ron` if the save dialog didn't.
fn with_extension(path: PathBuf) -> PathBuf {
    if path.extension().is_some() {
        path
    } else {
        path.with_extension("ron")
    }
}

#[cfg(test)]
mod tests {
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;
    use noodle_core::{Command, Node};

    use super::*;
    use crate::session::Edit;

    fn harness(app: App) -> Harness<'static, App> {
        Harness::new_ui_state(|ui, app: &mut App| app.show(ui), app)
    }

    fn empty() -> App {
        App::new(Session::new(crate::registry()))
    }

    #[test]
    fn an_empty_project_says_so() {
        let mut harness = harness(empty());
        harness.run();
        harness.get_by_label("No nodes yet");
        harness.get_by_label("No problems");
        harness.get_by_label("▶ Play");
    }

    #[test]
    fn undo_and_redo_shortcuts_reach_the_project() {
        let mut app = empty();
        let node = Node::new("noodle.osc.sine");
        let id = noodle_core::NodeId(1);
        app.session
            .edit([Edit::Apply(Command::AddNode { id, node })]);
        let mut harness = harness(app);
        harness.run();

        harness.key_press_modifiers(Modifiers::COMMAND, Key::Z);
        harness.run();
        assert_eq!(
            harness.state().session().project().graph().nodes().count(),
            0
        );

        harness.key_press_modifiers(Modifiers::COMMAND | Modifiers::SHIFT, Key::Z);
        harness.run();
        assert_eq!(
            harness.state().session().project().graph().nodes().count(),
            1
        );
    }

    #[test]
    fn problems_are_counted_in_the_status_bar() {
        let mut app = empty();
        let node = Node::new("no.such.type");
        let id = noodle_core::NodeId(1);
        app.session
            .edit([Edit::Apply(Command::AddNode { id, node })]);
        let mut harness = harness(app);
        harness.run();
        harness.get_by_label("1 problem");
    }

    #[test]
    fn selecting_a_node_shows_it_in_the_properties() {
        let mut app = empty();
        let node = Node::new("noodle.osc.sine").with_param("frequency", 220.0);
        let id = noodle_core::NodeId(1);
        app.session
            .edit([Edit::Apply(Command::AddNode { id, node })]);
        let mut harness = harness(app);
        harness.run();
        harness.get_by_label("Select a node to see its properties.");

        harness.get_by_label("Sine (#1)").click();
        harness.run();
        harness.get_by_label("frequency: 220");
    }

    #[test]
    fn saving_without_a_file_asks_where() {
        assert!(!empty().session.save());
        assert_eq!(with_extension(PathBuf::from("a")), PathBuf::from("a.ron"));
        assert_eq!(
            with_extension(PathBuf::from("a.ron")),
            PathBuf::from("a.ron")
        );
    }
}
