//! The window: menus, the toolbar, the panels, and keyboard shortcuts.

use std::path::PathBuf;

use egui::{Key, KeyboardShortcut, Modifiers};
use noodle_core::NodeId;
use noodle_engine::OUTPUT_ID;

use crate::devices::DevicePicker;
use crate::editor::{self, EditorState};
use crate::outputs::OutputsView;
use crate::piano_roll::{self, PianoRoll};
use crate::session::{Edit, Saved, Session};
use crate::timeline::{self, TimelineState};
use crate::{metronome, mixer, properties, theme};

const UNDO: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::Z);
const REDO: KeyboardShortcut =
    KeyboardShortcut::new(Modifiers::COMMAND.plus(Modifiers::SHIFT), Key::Z);
const NEW: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::N);
const OPEN: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::O);
const SAVE: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::S);
const SAVE_AS: KeyboardShortcut =
    KeyboardShortcut::new(Modifiers::COMMAND.plus(Modifiers::SHIFT), Key::S);
const SETTINGS: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::Comma);
/// Shown in the Edit menu. The node editor handles the key itself, so this
/// is not in [`SHORTCUTS`].
const ARRANGE: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::L);
/// Shown in the Edit menu. The canvas handles the key itself, with the
/// pointer over it, so this is not in [`SHORTCUTS`].
const DELETE: KeyboardShortcut = KeyboardShortcut::new(Modifiers::NONE, Key::X);
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
    timeline: TimelineState,
    piano_roll: PianoRoll,
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
    /// Whether a widget was being dragged last frame. See [`App::show`].
    dragging: bool,
    /// Whether the mixer panel is showing.
    mixer_open: bool,
    /// The mixer node the mixer shows; `None` is one strip per track.
    mixer_view: Option<NodeId>,
    scope_open: bool,
    /// The scope node the scope view shows; `None` is the first one.
    scope_view: Option<NodeId>,
    /// Whether the outputs view is showing.
    outputs_open: bool,
    outputs: OutputsView,
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
    Rewind,
    ToggleRecord,
    TogglePause,
    AudioSettings,
    ToggleMixer,
    ToggleMetronome,
    ToggleScope,
    ToggleOutputs,
    ImportAudio,
    DeleteSelection,
    ArrangeNodes,
    Close,
}

impl Action {
    /// Whether it throws the current project away.
    fn discards(self) -> bool {
        matches!(self, Self::New | Self::Open | Self::Close)
    }

    /// Whether a focused text field gets its key first. Space types a
    /// space, and Cmd+Z undoes the text, not the project edit before it. The
    /// others (New, Open, Save and the rest) are chords a text field has no
    /// use for; a new shortcut should ask which kind it is.
    fn yields_to_text(self) -> bool {
        matches!(self, Self::TogglePlayback | Self::Undo | Self::Redo)
    }
}

impl App {
    pub fn new(session: Session) -> Self {
        Self {
            session,
            editor: EditorState::default(),
            timeline: TimelineState::default(),
            piano_roll: PianoRoll::default(),
            devices: DevicePicker::default(),
            confirming: None,
            after_save: None,
            closing: false,
            title: String::new(),
            dragging: false,
            mixer_open: false,
            mixer_view: None,
            scope_open: false,
            scope_view: None,
            outputs_open: false,
            outputs: OutputsView::default(),
        }
    }

    #[cfg(all(test, target_os = "linux"))]
    pub fn session_mut(&mut self) -> &mut Session {
        &mut self.session
    }

    #[cfg(test)]
    pub fn timeline(&self) -> &TimelineState {
        &self.timeline
    }

    #[cfg(test)]
    pub fn session(&self) -> &Session {
        &self.session
    }

    #[cfg(test)]
    pub fn editor(&self) -> &EditorState {
        &self.editor
    }

    /// Opens the view a double-click on a mixer or scope node asked for, on
    /// that node.
    fn open_requested_view(&mut self) {
        let Some(node) = self.editor.take_view_request() else {
            return;
        };
        match self
            .session
            .project()
            .graph()
            .node(node)
            .map(|n| n.type_id.as_str())
        {
            Some(noodle_core::spare::MIXER) => {
                self.mixer_open = true;
                self.mixer_view = Some(node);
            }
            Some(_) => {
                self.scope_open = true;
                self.scope_view = Some(node);
            }
            None => {}
        }
    }

    /// The scope view: a drop-down over the project's Scope and Output nodes
    /// and a larger drawing of the chosen one.
    fn scope_pane(&mut self, ui: &mut egui::Ui) {
        let scopes: Vec<NodeId> = self
            .session
            .project()
            .graph()
            .nodes()
            .filter(|(_, n)| matches!(n.type_id.as_str(), noodle_nodes::SCOPE_ID | OUTPUT_ID))
            .map(|(id, _)| id)
            .collect();
        if self.scope_view.is_some_and(|n| !scopes.contains(&n)) {
            self.scope_view = None;
        }
        let Some(shown) = self.scope_view.or(scopes.first().copied()) else {
            ui.weak("No Scope or Output nodes yet. Add one in the node editor.");
            return;
        };
        let graph = self.session.project().graph();
        let label = |n: NodeId| match graph.node(n).map(|node| node.type_id.as_str()) {
            Some(OUTPUT_ID) => format!("Output {}", n.0),
            _ => format!("Scope {}", n.0),
        };
        ui.horizontal(|ui| {
            let name = ui.label("Scope");
            let combo = egui::ComboBox::from_id_salt("scope view")
                .selected_text(label(shown))
                .show_ui(ui, |ui| {
                    for &n in &scopes {
                        ui.selectable_value(&mut self.scope_view, Some(n), label(n));
                    }
                });
            combo.response.labelled_by(name.id);
        });
        let (rect, _) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), theme::SCOPE_VIEW_HEIGHT),
            egui::Sense::hover(),
        );
        editor::draw_scope(ui.painter(), rect, self.editor.scope_view(shown));
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

        if self.mixer_open {
            egui::Panel::bottom("mixer")
                .resizable(false)
                .show(ui, |ui| {
                    let edits = mixer::show(
                        ui,
                        self.session.project(),
                        &mut self.mixer_view,
                        &|mixer, channel| self.editor.input_level(mixer, channel),
                    );
                    self.session.edit(edits);
                });
        }

        if self.outputs_open {
            egui::Panel::bottom("outputs")
                .resizable(false)
                .show(ui, |ui| {
                    let edits = self.outputs.show(ui, &self.session);
                    self.session.edit(edits);
                });
        }

        if self.scope_open {
            egui::Panel::bottom("scope")
                .resizable(false)
                .show(ui, |ui| self.scope_pane(ui));
        }

        let active = self.editor.active;
        egui::Panel::right("properties")
            .default_size(theme::PROPERTIES_WIDTH)
            .show(ui, |ui| {
                // A panel is remembered at the size of its contents, so
                // short contents (nothing selected) would shrink it, and
                // the next node would open it at its default width.
                ui.set_min_width(ui.available_width());
                ui.add_space(4.0);
                let editor = &self.editor;
                let edits = properties::show(ui, &self.session, active, &|node, key| {
                    editor.param_live(node, key)
                });
                self.session.edit(edits);
            });

        egui::Panel::top("arrangement")
            .resizable(true)
            .default_size(theme::timeline::DEFAULT_HEIGHT)
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                let playhead = Some(self.session.playhead());
                let out = timeline::show(ui, &mut self.timeline, &self.session, playhead);
                self.session.edit(out.edits);
                if let Some(tick) = out.seek {
                    self.session.seek(tick);
                }
                if let Some(notice) = out.notice {
                    self.session.notify(notice);
                }
                if let Some(target) = out.pick {
                    self.import_audio(target);
                }
                if let Some(clip) = out.open_midi {
                    self.piano_roll.open(clip);
                }
                for (track, on) in out.arm {
                    self.session.arm(track, on);
                }
            });

        if self.piano_roll.clip().is_some() {
            egui::Panel::top("piano roll")
                .resizable(true)
                .default_size(piano_roll::DEFAULT_HEIGHT)
                .frame(egui::Frame::NONE.fill(theme::PANEL))
                .show(ui, |ui| {
                    let playhead = Some(self.session.playhead());
                    let out = piano_roll::show(ui, &mut self.piano_roll, &self.session, playhead);
                    self.session.edit(out.edits);
                    for (key, on) in out.audition {
                        self.session.audition(key, on);
                    }
                });
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(theme::CANVAS))
            .show(ui, |ui| {
                let edits = editor::show(ui, &mut self.editor, &self.session);
                self.session.edit(edits);
                self.open_requested_view();
            });

        self.confirm_dialog(ui.ctx(), &mut actions);
        if let Some(config) = self.devices.show(ui.ctx()) {
            self.session.set_audio_config(config);
            crate::prefs::remember_audio(self.session.audio_config());
        }
        self.editor.retain_existing(&self.session);
        // A safety net for undo grouping: a drag's edits are one undo step,
        // closed by its widget's `EndDrag`. A widget that stops being drawn
        // mid-drag never sends it, and the next gesture would join the group.
        // Ending a group that isn't open does nothing.
        let dragging = ui.ctx().dragged_id().is_some();
        if self.dragging && !dragging {
            self.session.edit([Edit::EndDrag]);
        }
        self.dragging = dragging;
        self.update_title(ui.ctx());
        if self.session.is_playing() {
            // Keeps health checks and plan freeing going while idle, and the
            // playhead moving at about 60 frames a second.
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(16));
        }
        for action in actions {
            self.request(ui.ctx(), action);
        }
    }

    fn shortcuts(&mut self, ctx: &egui::Context) -> Vec<Action> {
        // A text field keeps the keys it uses itself.
        // The editor canvas holds focus so it can receive Tab, and isn't a
        // text field.
        let typing = ctx.egui_wants_keyboard_input()
            && ctx.memory(|m| m.focused()) != Some(crate::editor::canvas_id());
        // A dialog has the user's attention; shortcuts would act behind it.
        // The editor's own keys are safe too, since they need the pointer
        // over the canvas and a modal's backdrop covers it. Keep it so.
        let dialog = self.devices.is_open() || self.confirming.is_some();
        ctx.input_mut(|input| {
            let mut actions = Vec::new();
            let shortcuts = if dialog { &[][..] } else { &SHORTCUTS[..] };
            for &(shortcut, action) in shortcuts {
                if !(typing && action.yields_to_text()) && input.consume_shortcut(&shortcut) {
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
        let mut item =
            |ui: &mut egui::Ui, text, shortcut: Option<&KeyboardShortcut>, enabled, action| {
                let mut button = egui::Button::new(text);
                if let Some(shortcut) = shortcut {
                    button = button.shortcut_text(ctx.format_shortcut(shortcut));
                }
                if ui.add_enabled(enabled, button).clicked() {
                    actions.push(action);
                }
            };
        ui.menu_button("File", |ui| {
            item(ui, "New", Some(&NEW), true, Action::New);
            item(ui, "Open…", Some(&OPEN), true, Action::Open);
            item(ui, "Save", Some(&SAVE), true, Action::Save);
            item(ui, "Save As…", Some(&SAVE_AS), true, Action::SaveAs);
            ui.separator();
            item(ui, "Import Audio…", None, true, Action::ImportAudio);
            ui.separator();
            item(
                ui,
                "Audio Settings…",
                Some(&SETTINGS),
                true,
                Action::AudioSettings,
            );
        });
        ui.menu_button("Edit", |ui| {
            item(
                ui,
                "Undo",
                Some(&UNDO),
                self.session.can_undo(),
                Action::Undo,
            );
            item(
                ui,
                "Redo",
                Some(&REDO),
                self.session.can_redo(),
                Action::Redo,
            );
            ui.separator();
            let selected = self.editor.has_selection();
            item(
                ui,
                "Delete",
                Some(&DELETE),
                selected,
                Action::DeleteSelection,
            );
            item(
                ui,
                "Arrange Nodes",
                Some(&ARRANGE),
                true,
                Action::ArrangeNodes,
            );
        });
        ui.menu_button("View", |ui| {
            if ui
                .selectable_label(self.scope_open, "Scope")
                .on_hover_text("A larger view of a Scope node")
                .clicked()
            {
                actions.push(Action::ToggleScope);
            }
            if ui
                .selectable_label(self.mixer_open, "Mixer")
                .on_hover_text("Gain, mute and solo for each track")
                .clicked()
            {
                actions.push(Action::ToggleMixer);
            }
            if ui
                .selectable_label(self.outputs_open, "Outputs")
                .on_hover_text("Which audio device each Output node plays on")
                .clicked()
            {
                actions.push(Action::ToggleOutputs);
            }
        });
    }

    /// The transport pill: rewind, play, pause, record, the position and the
    /// metronome, in a rounded capsule like the Canvas design's.
    fn transport(&self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        egui::Frame::NONE
            .fill(theme::TRANSPORT_FILL)
            .stroke(egui::Stroke::new(1.0, theme::TRANSPORT_OUTLINE))
            .corner_radius(egui::CornerRadius::same(theme::PILL_RADIUS))
            .inner_margin(egui::Margin::symmetric(8, 1))
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                self.transport_buttons(ui, actions);
            });
    }

    fn transport_buttons(&self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let playing = self.session.is_playing();
        let open = playing;
        ui.add_enabled_ui(open, |ui| {
            if ui.button("⏮").on_hover_text("Back to the start").clicked() {
                actions.push(Action::Rewind);
            }
        });
        let (label, tip) = if playing {
            ("⏹", "Stop (Space)")
        } else {
            (
                "▶",
                "Play through the output device chosen in Audio Settings (Space)",
            )
        };
        let label = egui::RichText::new(label).color(if playing {
            theme::ACCENT
        } else {
            theme::timeline::TEXT
        });
        if ui
            .add(egui::Button::new(label).selected(playing))
            .on_hover_text(tip)
            .clicked()
        {
            actions.push(Action::TogglePlayback);
        }
        ui.add_enabled_ui(open, |ui| {
            let pause = if self.session.transport_running() {
                "⏸"
            } else {
                "⏵"
            };
            if ui
                .button(pause)
                .on_hover_text("Pause or resume the timeline")
                .clicked()
            {
                actions.push(Action::TogglePause);
            }
        });
        let recording = self.session.is_recording();
        let record = egui::Button::new(egui::RichText::new("⏺").color(if recording {
            theme::RECORD
        } else {
            theme::timeline::TEXT
        }))
        .selected(recording);
        if ui
            .add(record)
            .on_hover_text(
                "Record the input onto the armed tracks (R on a track header), from the playhead",
            )
            .clicked()
        {
            actions.push(Action::ToggleRecord);
        }
        self.metronome_button(ui, actions);
        let at = self
            .session
            .project()
            .tempo_map()
            .position(self.session.playhead());
        let position = format!("{}.{}.{:03}", at.bar + 1, at.beat + 1, at.tick);
        ui.monospace(position).on_hover_text("Bar.beat.tick");
    }

    /// Lit while the bound button is on. A metronome that is missing shows
    /// dimmed (pressing adds one), and a broken binding shows as a problem.
    fn metronome_button(&self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let binding = metronome::binding(self.session.project());
        let (colour, selected, tip) = match &binding {
            metronome::Binding::Bound { on, .. } => (
                if *on {
                    theme::ACCENT
                } else {
                    theme::timeline::TEXT
                },
                *on,
                "Metronome".to_owned(),
            ),
            metronome::Binding::Missing => (
                theme::editor::TEXT_WEAK,
                false,
                "Add a metronome to the project".to_owned(),
            ),
            metronome::Binding::Broken(why) => (theme::editor::PROBLEM, false, why.clone()),
        };
        let button = egui::Button::new(egui::RichText::new("♩").color(colour)).selected(selected);
        if ui.add(button).on_hover_text(tip).clicked() {
            actions.push(Action::ToggleMetronome);
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
            let clips = self.session.clip_problems();
            if !clips.is_empty() {
                ui.separator();
                let what = match clips.len() {
                    1 => "1 clip can't play".to_owned(),
                    n => format!("{n} clips can't play"),
                };
                ui.colored_label(ui.visuals().warn_fg_color, what)
                    .on_hover_ui(|ui| {
                        for problem in clips {
                            ui.label(&problem.message);
                        }
                    });
            }
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
            Action::Rewind => self.session.rewind(),
            Action::ToggleRecord => {
                if self.session.is_recording() {
                    self.session.stop_recording();
                } else {
                    self.session.record();
                }
            }
            Action::TogglePause => {
                let running = self.session.transport_running();
                self.session.set_transport_running(!running);
            }
            Action::AudioSettings => self.devices.open(self.session.audio_config()),
            Action::ToggleMixer => self.mixer_open = !self.mixer_open,
            Action::ToggleMetronome => {
                let ids = || std::array::from_fn(|_| self.session.new_node_id());
                match metronome::press(self.session.project(), ids) {
                    Some(command) => self.session.edit([Edit::Apply(command)]),
                    None => {
                        let why = match metronome::binding(self.session.project()) {
                            metronome::Binding::Broken(why) => why,
                            _ => "The metronome can't be pressed".to_owned(),
                        };
                        self.session.notify(why);
                    }
                }
            }
            Action::ToggleScope => self.scope_open = !self.scope_open,
            Action::ToggleOutputs => {
                self.outputs_open = !self.outputs_open;
                if self.outputs_open {
                    self.outputs
                        .refresh(self.session.audio_config().host.as_deref());
                }
            }
            Action::ImportAudio => {
                let playhead = self.session.playhead();
                match self
                    .timeline
                    .import_target(self.session.project(), playhead)
                {
                    Some(target) => self.import_audio(target),
                    None => self
                        .session
                        .notify("Add a track before importing audio".to_string()),
                }
            }
            Action::ArrangeNodes => {
                self.editor.request_arrange();
                ctx.request_repaint();
            }
            Action::DeleteSelection => {
                if let Some(edit) = self.editor.delete_selection() {
                    self.session.edit([edit]);
                }
            }
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

impl App {
    /// Asks which audio files to add, and adds them as clips at `target`.
    fn import_audio(&mut self, target: timeline::Target) {
        let picked = rfd::FileDialog::new()
            .add_filter("Audio", &["wav", "flac", "mp3", "ogg", "m4a", "aac"])
            .pick_files();
        let Some(files) = picked else {
            return;
        };
        let added = timeline::import::import(&self.session, &files, target.track, target.at);
        if let Some(command) = added.command {
            self.session.edit([crate::session::Edit::Apply(command)]);
        }
        if let Some(notice) = added.notice {
            self.session.notify(notice);
        }
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

    fn properties_width(harness: &Harness<'static, App>) -> f32 {
        egui::containers::panel::PanelState::load(&harness.ctx, egui::Id::new("properties"))
            .expect("the panel has been shown")
            .size()
            .x
    }

    #[test]
    fn the_properties_panel_keeps_its_width_when_a_node_is_selected() {
        let mut app = empty();
        let id = noodle_core::NodeId(1);
        app.session.edit([Edit::Apply(Command::AddNode {
            id,
            node: Node::new("noodle.osc.sine"),
        })]);
        let mut harness = harness(app);
        harness.run();
        // The user drags the panel wider than its default.
        let wanted = properties_width(&harness) + 120.0;
        let state = egui::containers::panel::PanelState {
            outer_rect: egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(wanted, 100.0)),
        };
        harness
            .ctx
            .data_mut(|d| d.insert_persisted(egui::Id::new("properties"), state));
        harness.run();
        assert_eq!(
            properties_width(&harness),
            wanted,
            "kept with nothing selected"
        );

        let editor = &mut harness.state_mut().editor;
        editor.selected.insert(id);
        editor.active = Some(id);
        harness.run();
        assert_eq!(
            properties_width(&harness),
            wanted,
            "kept when a node is selected"
        );

        harness.state_mut().editor.active = None;
        harness.state_mut().editor.selected.clear();
        harness.run();
        assert_eq!(
            properties_width(&harness),
            wanted,
            "kept when it is deselected"
        );
    }

    #[test]
    fn importing_from_the_file_menu_needs_a_track() {
        let mut harness = harness(empty());
        harness.run();
        harness.get_by_label("File").click();
        harness.run();
        harness.get_by_label("Import Audio…").click();
        harness.run();
        harness.get_by_label("Add a track before importing audio");
    }

    #[test]
    fn edit_delete_removes_the_selected_nodes() {
        let mut app = empty();
        let id = noodle_core::NodeId(1);
        app.session.edit([Edit::Apply(Command::AddNode {
            id,
            node: Node::new("noodle.osc.sine"),
        })]);
        app.editor.selected.insert(id);
        app.editor.active = Some(id);
        let mut harness = harness(app);
        harness.run();
        harness.get_by_label("Edit").click();
        harness.run();
        harness.get_by_label_contains("Delete").click();
        harness.run();
        assert!(harness.state().session.project().graph().node(id).is_none());
    }

    #[test]
    fn edit_arrange_nodes_lays_the_graph_out_left_to_right() {
        let mut app = empty();
        let (a, b) = (noodle_core::NodeId(1), noodle_core::NodeId(2));
        app.session.edit([
            Edit::Apply(Command::AddNode {
                id: a,
                node: Node::new("noodle.util.gain").at(0.0, 0.0),
            }),
            Edit::Apply(Command::AddNode {
                id: b,
                node: Node::new("noodle.osc.sine").at(400.0, 300.0),
            }),
            Edit::Apply(Command::Connect(noodle_core::Connection {
                from: noodle_core::Endpoint::new(b, "out"),
                to: noodle_core::Endpoint::new(a, "in"),
            })),
        ]);
        let mut harness = harness(app);
        harness.run();
        harness.get_by_label("Edit").click();
        harness.run();
        harness.get_by_label_contains("Arrange Nodes").click();
        harness.run();
        let graph = harness.state().session.project().graph();
        assert!(graph.node(b).unwrap().position.x < graph.node(a).unwrap().position.x);
    }

    #[test]
    fn an_empty_project_says_so() {
        let mut harness = harness(empty());
        harness.run();
        harness.get_by_label("No nodes yet. Shift+A adds one.");
        harness.get_by_label("No problems");
        harness.get_by_label("▶");
    }

    #[test]
    fn the_mixer_opens_from_the_view_menu_and_shows_a_mixer() {
        let mut app = empty();
        app.session.edit([Edit::Apply(Command::AddNode {
            id: noodle_core::NodeId(1),
            node: Node::new(noodle_core::spare::MIXER),
        })]);
        let mut harness = harness(app);
        harness.run();
        assert!(
            harness
                .query_by_label("Nothing is wired into this mixer.")
                .is_none()
        );
        harness.get_by_label("View").click();
        harness.run();
        harness.get_by_label("Mixer").click();
        harness.run();
        harness.get_by_label("Nothing is wired into this mixer.");
        // And closes again.
        harness.get_by_label("View").click();
        harness.run();
        harness.get_by_label("Mixer").click();
        harness.run();
        assert!(
            harness
                .query_by_label("Nothing is wired into this mixer.")
                .is_none()
        );
    }

    #[test]
    fn the_scope_view_opens_from_the_view_menu() {
        let mut harness = harness(empty());
        harness.run();
        assert!(harness.query_by_label("Scope").is_none());
        harness.get_by_label("View").click();
        harness.run();
        harness.get_by_label("Scope").click();
        harness.run();
        harness.get_by_label_contains("No Scope or Output nodes yet");
    }

    #[test]
    fn the_outputs_view_opens_from_the_view_menu_and_lists_output_nodes() {
        let mut app = empty();
        app.session.edit([Edit::Apply(Command::AddNode {
            id: noodle_core::NodeId(1),
            node: Node::new(noodle_engine::OUTPUT_ID),
        })]);
        let mut harness = harness(app);
        harness.run();
        assert!(harness.query_by_label("Output 1").is_none());
        harness.get_by_label("View").click();
        harness.run();
        harness.get_by_label("Outputs").click();
        harness.run();
        harness.get_by_label("Output 1");
        harness.get_by_label("Add output");
    }

    #[test]
    fn the_scope_view_can_show_an_output_nodes_scope() {
        let mut app = empty();
        app.session.edit([Edit::Apply(Command::AddNode {
            id: noodle_core::NodeId(1),
            node: Node::new(noodle_engine::OUTPUT_ID),
        })]);
        let mut harness = harness(app);
        harness.run();
        harness.get_by_label("View").click();
        harness.run();
        harness.get_by_label("Scope").click();
        harness.run();
        assert_eq!(
            harness.get_by_label("Scope").value().as_deref(),
            Some("Output 1")
        );
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
        // The node's own field.
        assert_eq!(harness.query_all_by_label("Frequency").count(), 1);

        harness.state_mut().editor.selected.insert(id);
        harness.state_mut().editor.active = Some(id);
        harness.run();
        // And now the properties panel's.
        assert_eq!(harness.query_all_by_label("Frequency").count(), 2);
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
            Action::ToggleMixer,
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

    /// Two sines, the first active, in a harness big enough for both panels.
    fn two_placed_sines() -> Harness<'static, App> {
        let mut app = empty();
        for (n, x) in [(1, 0.0), (2, 300.0)] {
            let node = Node::new("noodle.osc.sine").at(x, 100.0);
            let id = noodle_core::NodeId(n);
            app.session
                .edit([Edit::Apply(Command::AddNode { id, node })]);
        }
        app.editor.selected.insert(noodle_core::NodeId(1));
        app.editor.active = Some(noodle_core::NodeId(1));
        let mut harness = Harness::builder()
            .with_size(egui::Vec2::new(1100.0, 700.0))
            .with_step_dt(1.0 / 60.0)
            .build_ui_state(|ui, app: &mut App| app.show(ui), app);
        harness.run();
        harness
    }

    /// An app with two sines, each added as its own undo step, and the
    /// first selected so the properties panel shows it.
    fn two_sines() -> Harness<'static, App> {
        let mut app = empty();
        for n in 1..=2 {
            app.session.edit([Edit::Apply(Command::AddNode {
                id: noodle_core::NodeId(n),
                node: Node::new("noodle.osc.sine"),
            })]);
        }
        let mut harness = harness(app);
        harness
            .state_mut()
            .editor
            .selected
            .insert(noodle_core::NodeId(1));
        harness.state_mut().editor.active = Some(noodle_core::NodeId(1));
        harness.run();
        harness
    }

    fn param(h: &Harness<'_, App>, node: u64) -> Option<f32> {
        let graph = h.state().session().project().graph();
        let node = graph.node(noodle_core::NodeId(node)).unwrap();
        node.params.get("frequency").copied()
    }

    /// Where a node's title is on screen.
    fn title(h: &Harness<'_, App>, node: u64) -> egui::Pos2 {
        let editor = h.state().editor();
        let graph = h.state().session().project().graph();
        let at = graph.node(noodle_core::NodeId(node)).unwrap().position;
        let p = egui::Pos2::new(at.x + 80.0, at.y + 8.0);
        editor.to_screen(p)
    }

    /// The properties panel's Frequency field: the rightmost one.
    fn panel_field(h: &Harness<'_, App>) -> egui::Pos2 {
        h.get_all_by_label("Frequency")
            .map(|n| n.rect())
            .max_by(|a, b| a.min.x.total_cmp(&b.min.x))
            .unwrap()
            .center()
    }

    fn press_at(h: &Harness<'_, App>, pos: egui::Pos2, pressed: bool) {
        h.event(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        });
    }

    fn move_to(h: &mut Harness<'_, App>, pos: egui::Pos2) {
        h.event(egui::Event::PointerMoved(pos));
        h.step();
    }

    #[test]
    fn keys_do_nothing_while_a_node_is_dragged() {
        let mut h = two_placed_sines();
        let start = title(&h, 1);
        move_to(&mut h, start);
        press_at(&h, start, true);
        h.step();
        move_to(&mut h, start + egui::vec2(20.0, 0.0));
        move_to(&mut h, start + egui::vec2(40.0, 0.0));
        // X would delete the node being dragged.
        h.key_press(Key::X);
        h.step();
        move_to(&mut h, start + egui::vec2(60.0, 0.0));
        press_at(&h, start + egui::vec2(60.0, 0.0), false);
        h.run();
        let session = h.state().session();
        assert_eq!(session.project().graph().nodes().count(), 2);
        assert_eq!(session.message(), None);
        let moved = session.project().graph().node(noodle_core::NodeId(1));
        assert_eq!(moved.unwrap().position.x, 60.0);
    }

    #[test]
    fn select_all_mid_drag_leaves_the_panel_slider_alone() {
        let mut h = two_placed_sines();
        let start = panel_field(&h);
        move_to(&mut h, start);
        press_at(&h, start, true);
        h.step();
        move_to(&mut h, start + egui::vec2(10.0, 0.0));
        move_to(&mut h, start + egui::vec2(20.0, 0.0));
        // Over the canvas, where the editor's keys would be live.
        let canvas = title(&h, 2);
        move_to(&mut h, canvas);
        h.key_press(Key::A);
        h.step();
        press_at(&h, canvas, false);
        h.run();
        let editor = h.state().editor();
        assert_eq!(editor.selected.len(), 1, "A didn't select all");
        assert_eq!(editor.active, Some(noodle_core::NodeId(1)));
    }

    #[test]
    fn a_drag_whose_widget_disappears_still_ends_its_undo_step() {
        let mut h = two_placed_sines();
        let start = panel_field(&h);
        move_to(&mut h, start);
        press_at(&h, start, true);
        h.step();
        move_to(&mut h, start + egui::vec2(10.0, 0.0));
        move_to(&mut h, start + egui::vec2(20.0, 0.0));
        assert!(param(&h, 1).is_some());
        // The panel switches to the other node, so the dragged field goes.
        h.state_mut().editor.active = Some(noodle_core::NodeId(2));
        h.step();
        press_at(&h, start + egui::vec2(20.0, 0.0), false);
        h.run();

        // So a node move afterwards is an undo step of its own.
        let at = title(&h, 2);
        move_to(&mut h, at);
        press_at(&h, at, true);
        h.step();
        move_to(&mut h, at + egui::vec2(20.0, 0.0));
        move_to(&mut h, at + egui::vec2(40.0, 0.0));
        press_at(&h, at + egui::vec2(40.0, 0.0), false);
        h.run();
        h.state_mut().session.undo();
        let moved = h.state().session().project().graph();
        assert_eq!(
            moved.node(noodle_core::NodeId(2)).unwrap().position.x,
            300.0
        );
        assert!(param(&h, 1).is_some(), "only the move was undone");
    }

    fn node_count(harness: &Harness<'_, App>) -> usize {
        harness.state().session().project().graph().nodes().count()
    }

    #[test]
    fn undo_and_redo_belong_to_a_text_field_while_typing() {
        let mut harness = two_sines();
        // One edit is undone already, so there's something to redo.
        harness.key_press_modifiers(Modifiers::COMMAND, Key::Z);
        harness.run();
        assert_eq!(node_count(&harness), 1);
        assert!(harness.state().session().can_redo());

        // The properties panel's field is the rightmost one.
        let field = harness
            .query_all_by_label("Frequency")
            .max_by(|a, b| a.rect().center().x.total_cmp(&b.rect().center().x))
            .unwrap();
        field.click();
        harness.run();
        harness.key_press_modifiers(Modifiers::COMMAND, Key::A);
        harness.event(egui::Event::Text("1000".into()));
        harness.run();

        harness.key_press_modifiers(Modifiers::COMMAND | Modifiers::SHIFT, Key::Z);
        harness.run();
        assert_eq!(node_count(&harness), 1, "Cmd+Shift+Z redid a project edit");
        harness.key_press_modifiers(Modifiers::COMMAND, Key::Z);
        harness.run();
        assert_eq!(node_count(&harness), 1, "Cmd+Z undid a project edit");
        assert!(harness.state().session().can_redo());

        // Escape leaves the field, and then both are the project's again.
        harness.key_press(Key::Escape);
        harness.run();
        harness.key_press_modifiers(Modifiers::COMMAND | Modifiers::SHIFT, Key::Z);
        harness.run();
        assert_eq!(node_count(&harness), 2);
        harness.key_press_modifiers(Modifiers::COMMAND, Key::Z);
        harness.run();
        assert_eq!(node_count(&harness), 1);
    }

    #[test]
    fn the_metronome_button_adds_a_metronome_then_toggles_it() {
        let mut harness = harness(empty());
        harness.run();
        harness.get_by_label("♩").click();
        harness.run();
        assert!(matches!(
            metronome::binding(harness.state().session.project()),
            metronome::Binding::Bound { on: true, .. }
        ));
        harness.get_by_label("♩").click();
        harness.run();
        assert!(matches!(
            metronome::binding(harness.state().session.project()),
            metronome::Binding::Bound { on: false, .. }
        ));
    }
}
