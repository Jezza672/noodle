//! The window: menus, the toolbar, the panels, and keyboard shortcuts.

use std::path::PathBuf;

use egui::{Key, KeyboardShortcut, Modifiers};

use crate::devices::DevicePicker;
use crate::editor::{self, EditorState};
use crate::session::{Saved, Session};
use crate::{properties, theme};

const UNDO: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::Z);
const REDO: KeyboardShortcut =
    KeyboardShortcut::new(Modifiers::COMMAND.plus(Modifiers::SHIFT), Key::Z);
const NEW: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::N);
const OPEN: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::O);
const SAVE: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::S);
const SAVE_AS: KeyboardShortcut =
    KeyboardShortcut::new(Modifiers::COMMAND.plus(Modifiers::SHIFT), Key::S);
const SETTINGS: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::Comma);
const PLAY: KeyboardShortcut = KeyboardShortcut::new(Modifiers::NONE, Key::Space);

/// Longer shortcuts first: Cmd+Z would also match Cmd+Shift+Z.
const SHORTCUTS: [(KeyboardShortcut, Action); 8] = [
    (REDO, Action::Redo),
    (UNDO, Action::Undo),
    (SAVE_AS, Action::SaveAs),
    (SAVE, Action::Save),
    (OPEN, Action::Open),
    (NEW, Action::New),
    (SETTINGS, Action::AudioSettings),
    (PLAY, Action::TogglePlayback),
];

pub struct App {
    session: Session,
    editor: EditorState,
    devices: DevicePicker,
    /// An action waiting for the user to decide what to do with unsaved
    /// changes.
    confirming: Option<Action>,
    /// An action to run once Save As succeeds, when the user chose to save an
    /// untitled project before it.
    after_save: Option<Action>,
    /// Set once the user has agreed to close despite unsaved changes.
    closing: bool,
    /// The title last sent to the window, so it's only sent when it changes.
    title: String,
}

/// Something the user asked for, from a menu or a shortcut. Collected during
/// the frame and run afterwards, so nothing slow (file dialogs, file and
/// device I/O) happens while egui is mid-frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    New,
    Open,
    Save,
    SaveAs,
    Undo,
    Redo,
    TogglePlayback,
    AudioSettings,
    Close,
}

impl Action {
    /// Whether it throws the current project away.
    fn discards(self) -> bool {
        matches!(self, Self::New | Self::Open | Self::Close)
    }
}

impl App {
    pub fn new(session: Session) -> Self {
        Self {
            session,
            editor: EditorState::default(),
            devices: DevicePicker::default(),
            confirming: None,
            after_save: None,
            closing: false,
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
        // Before the panels, so a focused button doesn't also see Space.
        let mut actions = self.shortcuts(ui.ctx());

        egui::Panel::top("menu").show(ui, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                self.menus(ui, &mut actions);
                ui.separator();
                self.transport(ui, &mut actions);
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

        self.confirm_dialog(ui.ctx(), &mut actions);
        if let Some(config) = self.devices.show(ui.ctx()) {
            self.session.set_audio_config(config);
            crate::prefs::remember_audio(self.session.audio_config());
        }
        self.editor.retain_existing(&self.session);
        self.update_title(ui.ctx());
        if self.session.is_playing() {
            // Keeps health checks and plan freeing going while idle.
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(100));
        }
        for action in actions {
            self.request(ui.ctx(), action);
        }
    }

    fn shortcuts(&mut self, ctx: &egui::Context) -> Vec<Action> {
        // A text field gets plain keys like Space.
        let typing = ctx.egui_wants_keyboard_input();
        // A dialog has the user's attention; shortcuts would act behind it.
        let dialog = self.devices.is_open() || self.confirming.is_some();
        ctx.input_mut(|input| {
            let mut actions = Vec::new();
            let shortcuts = if dialog { &[][..] } else { &SHORTCUTS[..] };
            for &(shortcut, action) in shortcuts {
                if (action != Action::TogglePlayback || !typing)
                    && input.consume_shortcut(&shortcut)
                {
                    actions.push(action);
                }
            }
            if input.viewport().close_requested() && !self.closing {
                actions.push(Action::Close);
            }
            actions
        })
    }

    fn menus(&self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let ctx = ui.ctx().clone();
        let mut item = |ui: &mut egui::Ui, text, shortcut: &KeyboardShortcut, enabled, action| {
            let button = egui::Button::new(text).shortcut_text(ctx.format_shortcut(shortcut));
            if ui.add_enabled(enabled, button).clicked() {
                actions.push(action);
            }
        };
        ui.menu_button("File", |ui| {
            item(ui, "New", &NEW, true, Action::New);
            item(ui, "Open…", &OPEN, true, Action::Open);
            item(ui, "Save", &SAVE, true, Action::Save);
            item(ui, "Save As…", &SAVE_AS, true, Action::SaveAs);
            ui.separator();
            item(
                ui,
                "Audio Settings…",
                &SETTINGS,
                true,
                Action::AudioSettings,
            );
        });
        ui.menu_button("Edit", |ui| {
            item(ui, "Undo", &UNDO, self.session.can_undo(), Action::Undo);
            item(ui, "Redo", &REDO, self.session.can_redo(), Action::Redo);
        });
    }

    fn transport(&self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let label = if self.session.is_playing() {
            "⏹ Stop"
        } else {
            "▶ Play"
        };
        if ui
            .button(label)
            .on_hover_text("Play through the output device chosen in Audio Settings (Space)")
            .clicked()
        {
            actions.push(Action::TogglePlayback);
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
            if let Some(problem) = self.session.input_problem() {
                ui.separator();
                ui.colored_label(ui.visuals().warn_fg_color, "No input")
                    .on_hover_text(problem);
            }
            if let Some(message) = self.session.message() {
                ui.separator();
                ui.label(message);
            }
        });
    }

    /// Asks what to do with unsaved changes before an action that would
    /// lose them.
    fn confirm_dialog(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        let Some(action) = self.confirming else {
            return;
        };
        let modal = egui::Modal::new(egui::Id::new("unsaved")).show(ctx, |ui| {
            ui.label(format!(
                "Save the changes to {} first?",
                self.session.name()
            ));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("Save").clicked() {
                    match self.session.save() {
                        Saved::Yes => {
                            self.confirming = None;
                            self.run(ctx, action);
                        }
                        // The dialog stays open, with the error in the
                        // status bar, so nothing is lost.
                        Saved::Failed => {}
                        // The file dialog can't open mid-frame.
                        Saved::NoFile => {
                            self.confirming = None;
                            self.after_save = Some(action);
                            actions.push(Action::SaveAs);
                        }
                    }
                }
                if ui.button("Don't Save").clicked() {
                    self.confirming = None;
                    self.run(ctx, action);
                }
                if ui.button("Cancel").clicked() {
                    self.confirming = None;
                }
            });
        });
        if modal.should_close() {
            self.confirming = None;
        }
    }

    /// Runs an action, first asking about unsaved changes if it would lose
    /// them.
    fn request(&mut self, ctx: &egui::Context, action: Action) {
        if action.discards() && self.session.is_dirty() {
            if action == Action::Close {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            }
            self.confirming = Some(action);
        } else {
            self.run(ctx, action);
        }
    }

    fn run(&mut self, ctx: &egui::Context, action: Action) {
        match action {
            Action::New => {
                self.session.new_project();
                self.editor = EditorState::default();
            }
            Action::Open => {
                if let Some(path) = dialog().pick_file()
                    && self.session.load(&path)
                {
                    self.editor = EditorState::default();
                }
            }
            Action::Save => {
                if self.session.save() == Saved::NoFile {
                    self.run(ctx, Action::SaveAs);
                }
            }
            Action::SaveAs => {
                let then = self.after_save.take();
                let name = format!("{}.ron", self.session.name());
                if let Some(path) = dialog().set_file_name(name).save_file()
                    && self.session.save_as(&with_extension(path))
                    && let Some(then) = then
                {
                    self.run(ctx, then);
                }
            }
            Action::Undo => self.session.undo(),
            Action::Redo => self.session.redo(),
            Action::TogglePlayback => {
                if self.session.is_playing() {
                    self.session.stop();
                } else {
                    self.session.play();
                }
            }
            Action::AudioSettings => self.devices.open(self.session.audio_config()),
            Action::Close => {
                self.closing = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
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

fn dialog() -> rfd::FileDialog {
    rfd::FileDialog::new().add_filter("Noodle project", &["ron"])
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
        App::new(Session::new(crate::session::Nodes::all()))
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
        harness.get_by_label("Frequency");
    }

    #[test]
    fn new_asks_before_discarding_unsaved_changes() {
        let mut app = empty();
        let node = Node::new("noodle.osc.sine");
        let id = noodle_core::NodeId(1);
        app.session
            .edit([Edit::Apply(Command::AddNode { id, node })]);
        let mut harness = harness(app);
        harness.run();

        harness.key_press_modifiers(Modifiers::COMMAND, Key::N);
        harness.run();
        harness.get_by_label("Save the changes to Untitled first?");
        harness.get_by_label("Cancel").click();
        harness.run();
        assert_eq!(
            harness.state().session().project().graph().nodes().count(),
            1
        );

        harness.key_press_modifiers(Modifiers::COMMAND, Key::N);
        harness.run();
        harness.get_by_label("Don't Save").click();
        harness.run();
        assert_eq!(
            harness.state().session().project().graph().nodes().count(),
            0
        );
    }

    #[test]
    fn a_failed_save_keeps_the_work_and_the_question() {
        let dir = std::env::temp_dir().join(format!("noodle-app-{}-gone", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("project.ron");
        let mut app = empty();
        assert!(app.session.save_as(&path));
        std::fs::remove_dir_all(&dir).unwrap();
        let node = Node::new("noodle.osc.sine");
        let id = noodle_core::NodeId(1);
        app.session
            .edit([Edit::Apply(Command::AddNode { id, node })]);
        let mut harness = harness(app);
        harness.run();

        harness.key_press_modifiers(Modifiers::COMMAND, Key::N);
        harness.run();
        harness.get_by_label("Save").click();
        harness.run();
        assert_eq!(
            harness.state().session().project().graph().nodes().count(),
            1
        );
        harness.get_by_label("Don't Save");
    }

    #[test]
    fn new_without_changes_doesnt_ask() {
        let mut harness = harness(empty());
        harness.run();
        harness.key_press_modifiers(Modifiers::COMMAND, Key::N);
        harness.run();
        assert!(harness.query_by_label("Cancel").is_none());
    }

    #[test]
    fn only_new_open_and_close_discard() {
        let discarding: Vec<_> = [
            Action::New,
            Action::Open,
            Action::Save,
            Action::SaveAs,
            Action::Undo,
            Action::Redo,
            Action::TogglePlayback,
            Action::AudioSettings,
            Action::Close,
        ]
        .into_iter()
        .filter(|a| a.discards())
        .collect();
        assert_eq!(discarding, [Action::New, Action::Open, Action::Close]);
    }

    #[test]
    fn saving_without_a_file_asks_where() {
        assert_eq!(empty().session.save(), Saved::NoFile);
        assert_eq!(with_extension(PathBuf::from("a")), PathBuf::from("a.ron"));
        assert_eq!(
            with_extension(PathBuf::from("a.ron")),
            PathBuf::from("a.ron")
        );
    }

    /// An audio settings dialog that lists no devices, rather than asking
    /// the system.
    fn no_devices() -> DevicePicker {
        DevicePicker::with_lister(Vec::new, |_| Ok(noodle_io::DeviceList::default()))
    }

    #[test]
    fn audio_settings_open_from_the_shortcut_and_reach_the_session() {
        let dir = tempfile::tempdir().unwrap();
        let prefs = dir.path().join("prefs.ron");
        crate::prefs::init(prefs.clone());
        let mut app = empty();
        app.devices = no_devices();
        let mut harness = harness(app);
        harness.run();
        assert!(harness.query_by_label("Audio Settings").is_none());

        harness.key_press_modifiers(Modifiers::COMMAND, Key::Comma);
        harness.run();
        harness.get_by_label("Buffer size").click();
        harness.run();
        harness.get_by_label("256 frames").click();
        harness.run();
        harness.get_by_label("Apply").click();
        harness.run();

        assert!(harness.query_by_label("Audio Settings").is_none());
        let session = harness.state().session();
        assert_eq!(session.audio_config().buffer_size, Some(256));
        assert!(!session.is_playing());
        let saved = crate::prefs::load(&prefs).unwrap();
        assert_eq!(saved.audio.buffer_size, Some(256));
    }

    #[test]
    fn shortcuts_do_nothing_behind_a_dialog() {
        let mut app = empty();
        let node = Node::new("noodle.osc.sine");
        let id = noodle_core::NodeId(1);
        app.session
            .edit([Edit::Apply(Command::AddNode { id, node })]);
        app.devices = no_devices();
        app.devices.open(&noodle_io::AudioConfig::default());
        let mut harness = harness(app);
        harness.run();

        harness.key_press_modifiers(Modifiers::COMMAND, Key::Z);
        harness.run();
        assert_eq!(
            harness.state().session().project().graph().nodes().count(),
            1
        );
    }
}
