//! The window: menus, the toolbar, the panels, and keyboard shortcuts.

use std::path::PathBuf;

use egui::{Key, KeyboardShortcut, Modifiers};
use noodle_core::{Command, NodeId};
use noodle_engine::OUTPUT_ID;

use crate::clip_editor::{self, ClipEditor};
use crate::clip_panel::{self, ClipPanel};
use crate::devices::DevicePicker;
use crate::editor::{self, EditorState};
use crate::export_dialog::ExportDialog;
use crate::outputs::OutputsView;
use crate::piano_roll::{self, PianoRoll};
use crate::plugin_browser::PluginBrowser;
use crate::session::{Edit, Saved, Session};
use crate::timeline::{self, TimelineState};
use crate::{metronome, mixer, pill_buttons, properties, theme};

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
/// Shown in the View menu. The canvas handles the key itself, with the
/// pointer over it, so this is not in [`SHORTCUTS`].
const FIT: KeyboardShortcut = KeyboardShortcut::new(Modifiers::NONE, Key::F);
/// Shown in the Edit menu. The canvas handles the key itself, with the
/// pointer over it, so this is not in [`SHORTCUTS`].
const DELETE: KeyboardShortcut = KeyboardShortcut::new(Modifiers::NONE, Key::X);
/// Turns the computer keyboard into a MIDI source, as in Ableton.
const KEYBOARD: KeyboardShortcut = KeyboardShortcut::new(Modifiers::NONE, Key::M);
const PLAY: KeyboardShortcut = KeyboardShortcut::new(Modifiers::NONE, Key::Space);

/// Longer shortcuts first: Cmd+Z would also match Cmd+Shift+Z.
const SHORTCUTS: [(KeyboardShortcut, Action); 9] = [
    (REDO, Action::Redo),
    (UNDO, Action::Undo),
    (SAVE_AS, Action::SaveAs),
    (SAVE, Action::Save),
    (OPEN, Action::Open),
    (NEW, Action::New),
    (SETTINGS, Action::AudioSettings),
    (PLAY, Action::TogglePlayback),
    (KEYBOARD, Action::ToggleKeyboard),
];

pub struct App {
    session: Session,
    editor: EditorState,
    timeline: TimelineState,
    piano_roll: PianoRoll,
    clip_editor: ClipEditor,
    sample_view: crate::sample_view::SampleView,
    clip_panel: ClipPanel,
    /// The track the piano roll last played keys on, to let them go.
    audition_track: Option<noodle_core::NodeId>,
    devices: DevicePicker,
    export: ExportDialog,
    /// An action waiting for the user to decide what to do with unsaved
    /// changes.
    confirming: Option<Action>,
    /// An action to run once Save As succeeds, when the user chose to save an
    /// untitled project before it.
    after_save: Option<Action>,
    /// An action the user chose to run without saving, run once the dialog
    /// that asked is done drawing (a file dialog can't open inside it).
    discarding: Option<Action>,
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
    plugin_browser: PluginBrowser,
    outputs: OutputsView,
    /// The computer keyboard as a MIDI source. Off at start and not saved.
    keyboard: crate::keyboard::KeyboardMidi,
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
    ToggleKeyboard,
    /// How the MIDI of the next take is snapped.
    SetRecordQuantise(crate::midi_take::Quantise),
    /// Turns the audio engine on or off.
    ToggleEngine,
    AudioSettings,
    ToggleMixer,
    ToggleMetronome,
    /// Sets the session tempo (in hundredths of a beat per minute) where the
    /// playhead is. `drag`: part of a drag of the field, which ends with
    /// [`Action::EndTempoDrag`].
    SetTempo {
        centi_bpm: u32,
        drag: bool,
    },
    EndTempoDrag,
    SetSignature(u8, u8),
    ToggleTempoLane,
    /// Presses a button in the transport pill.
    PressButton(NodeId),
    /// Adds a button wired to the switch at this place in the menu's list.
    BindButton(usize),
    ToggleScope,
    ToggleOutputs,
    TogglePlugins,
    ToggleCpu,
    ImportAudio,
    ExportAudio,
    /// Asks where the export the dialog chose is to be written.
    StartExport,
    DeleteSelection,
    ArrangeNodes,
    /// Fits the node editor's view around the whole graph at this level.
    FitGraph,
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
        matches!(
            self,
            Self::TogglePlayback | Self::ToggleKeyboard | Self::Undo | Self::Redo
        )
    }
}

impl App {
    pub fn new(session: Session) -> Self {
        Self {
            session,
            editor: EditorState::default(),
            timeline: TimelineState::default(),
            piano_roll: PianoRoll::default(),
            clip_editor: ClipEditor::default(),
            sample_view: Default::default(),
            clip_panel: ClipPanel::default(),
            audition_track: None,
            devices: DevicePicker::default(),
            export: ExportDialog::default(),
            confirming: None,
            after_save: None,
            discarding: None,
            closing: false,
            title: String::new(),
            dragging: false,
            mixer_open: false,
            mixer_view: None,
            scope_open: false,
            scope_view: None,
            outputs_open: false,
            plugin_browser: PluginBrowser::default(),
            outputs: OutputsView::default(),
            keyboard: crate::keyboard::KeyboardMidi::default(),
        }
    }

    #[cfg(test)]
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
        // A double-click on a plugin node opens the plugin's own editor.
        if let Some(node) = self.editor.take_open_request()
            && self.session.plugin_of(node).is_some()
        {
            let open = !self.session.plugin_editor_open(node);
            self.session.edit([Edit::PluginEditor { node, open }]);
        }
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
            let combo = crate::widgets::Dropdown::from_id_salt("scope view")
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
        if self.session.watching_midi() {
            ui.ctx()
                .request_repaint_after(crate::session::MIDI_WATCH_INTERVAL);
        }
        // Before the shortcuts, so the note keys are not also delete and the
        // like.
        self.keyboard_midi(ui.ctx());
        // Before the panels, so a focused button doesn't also see Space.
        let mut actions = self.shortcuts(ui.ctx());

        egui::Panel::top("menu").show(ui, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                self.menus(ui, &mut actions);
                ui.separator();
                self.transport(ui, &mut actions);
                ui.separator();
                if let Some(step) = crate::snap::menu(ui, self.session.snap()) {
                    self.session.set_snap(step);
                }
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
            .frame(egui::Frame::side_top_panel(ui.style()).fill(theme::PANEL))
            .show(ui, |ui| {
                // A panel is remembered at the size of its contents, so
                // short contents (nothing selected) would shrink it, and
                // the next node would open it at its default width.
                ui.set_min_width(ui.available_width());
                ui.add_space(4.0);
                let mut edits = Vec::new();
                // The selected audio clip's own properties, above the node's.
                let picked = self
                    .clip_editor
                    .clip()
                    .or_else(|| self.timeline.selected_clip())
                    .filter(|&id| {
                        self.session
                            .project()
                            .clip(id)
                            .is_some_and(|clip| clip.as_audio().is_some())
                    });
                if let Some(id) = picked {
                    let loaded = self.clip_source(ui.ctx(), id);
                    edits.extend(clip_panel::show(
                        ui,
                        &mut self.clip_panel,
                        &self.session,
                        id,
                        loaded.as_ref(),
                    ));
                    ui.separator();
                }
                let editor = &self.editor;
                edits.extend(properties::show(
                    ui,
                    &self.session,
                    active,
                    &|node, key| editor.param_live(node, key),
                    &|node| editor.spectrum(node),
                ));
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
                    self.clip_editor.close();
                    self.sample_view.close();
                    self.piano_roll.open(clip);
                }
                if let Some(clip) = out.open_audio {
                    self.piano_roll.close();
                    self.sample_view.close();
                    self.clip_editor.open(clip);
                }
                for (track, kind, on) in out.arm {
                    self.session.arm_kind(track, kind, on);
                }
            });

        // A double-click on a Sampler's preview in the properties panel.
        if let Some(node) = crate::sample_view::take_request(ui.ctx()) {
            self.piano_roll.close();
            self.clip_editor.close();
            self.sample_view.open(node);
        }
        self.sample_view.retain_existing(&self.session);
        if self.sample_view.node().is_some() {
            egui::Panel::top("sample view")
                .resizable(true)
                .default_size(crate::sample_view::DEFAULT_HEIGHT)
                .frame(egui::Frame::NONE.fill(theme::PANEL))
                .show(ui, |ui| {
                    let edits = crate::sample_view::show(ui, &mut self.sample_view, &self.session);
                    self.session.edit(edits);
                });
        }

        if let Some(clip) = self.piano_roll.clip() {
            egui::Panel::top("piano roll")
                .resizable(true)
                .default_size(piano_roll::DEFAULT_HEIGHT)
                .frame(egui::Frame::NONE.fill(theme::PANEL))
                .show(ui, |ui| {
                    let playhead = Some(self.session.playhead());
                    let out = piano_roll::show(ui, &mut self.piano_roll, &self.session, playhead);
                    self.session.edit(out.edits);
                    if let Some(track) = self.session.project().clip(clip).map(|c| c.node) {
                        self.audition_track = Some(track);
                        for (key, on) in out.audition {
                            self.session.audition(track, key, on);
                        }
                    }
                });
        }
        if let Some(clip) = self.clip_editor.clip() {
            egui::Panel::top("clip editor")
                .resizable(true)
                .default_size(clip_editor::DEFAULT_HEIGHT)
                .frame(egui::Frame::NONE.fill(theme::PANEL))
                .show(ui, |ui| {
                    let loaded = self.clip_source(ui.ctx(), clip);
                    let playhead = Some(self.session.playhead());
                    let out = clip_editor::show(
                        ui,
                        &mut self.clip_editor,
                        &self.session,
                        loaded.as_ref(),
                        playhead,
                    );
                    self.session.edit(out.edits);
                });
        }
        // A key still down when the roll closed or its clip went.
        if let Some(key) = self.piano_roll.take_release()
            && let Some(track) = self.audition_track
        {
            self.session.audition(track, key, false);
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(theme::CANVAS))
            .show(ui, |ui| {
                let edits = editor::show(ui, &mut self.editor, &self.session);
                self.session.edit(edits);
                self.open_requested_view();
            });
        if let Some(dropped) = self.editor.take_dropped() {
            self.dropped_on_graph(dropped);
        }

        self.confirm_dialog(ui.ctx(), &mut actions);
        let edits = self.plugin_browser.show(ui.ctx(), &mut self.session);
        self.session.edit(edits);
        self.export.show(ui.ctx(), &mut self.session);
        crate::stems_dialog::show(ui.ctx(), &mut self.session);
        if self.export.has_choice() {
            actions.push(Action::StartExport);
        }
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
        crate::carry::expire(ui.ctx());
        self.update_title(ui.ctx());
        if self.session.engine_running() {
            // Keeps health checks and plan freeing going while idle, and the
            // playhead moving at about 60 frames a second while it does.
            let wait = if self.session.transport_running() {
                16
            } else {
                100
            };
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(wait));
        }
        if let Some(action) = self.discarding.take() {
            self.run(ui.ctx(), action);
        }
        for action in actions {
            self.request(ui.ctx(), action);
        }
    }

    /// Turns the keys the computer keyboard plays into MIDI on the session's
    /// bus, and takes them from the frame's input so nothing else acts on
    /// them. A text field, a dialog or the window losing focus lets go of
    /// what is held instead.
    fn keyboard_midi(&mut self, ctx: &egui::Context) {
        if !self.keyboard.is_on() {
            return;
        }
        let typing = ctx.egui_wants_keyboard_input()
            && ctx.memory(|m| m.focused()) != Some(crate::editor::canvas_id());
        let dialog = self.devices.is_open() || self.export.is_open() || self.confirming.is_some();
        let focused = ctx.input(|i| i.focused);
        let mut messages = Vec::new();
        if typing || dialog || !focused {
            messages.extend(self.keyboard.release_all());
        } else {
            let keyboard = &mut self.keyboard;
            ctx.input_mut(|input| {
                input.events.retain(|event| {
                    let egui::Event::Key {
                        key,
                        pressed,
                        repeat,
                        modifiers,
                        ..
                    } = event
                    else {
                        return true;
                    };
                    if !crate::keyboard::is_key(*key) {
                        return true;
                    }
                    if *pressed {
                        // Chords such as Cmd+C and Shift+A are the app's
                        // shortcuts, not notes. Only bare keys play.
                        if modifiers.any() {
                            return true;
                        }
                        if !repeat {
                            messages.extend(keyboard.press(*key));
                        }
                    } else {
                        messages.extend(keyboard.release(*key));
                    }
                    false
                });
            });
        }
        for message in messages {
            self.session.midi_bus().send(message);
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
        let dialog = self.devices.is_open() || self.export.is_open() || self.confirming.is_some();
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
            item(ui, "Export Audio…", None, true, Action::ExportAudio);
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
                .add(egui::Button::new("Fit Graph").shortcut_text(ui.ctx().format_shortcut(&FIT)))
                .on_hover_text("Centre the node editor on the whole graph at this level")
                .clicked()
            {
                actions.push(Action::FitGraph);
                ui.close();
            }
            ui.separator();
            if ui
                .selectable_label(self.scope_open, "Scope")
                .on_hover_text("A larger view of a Scope node")
                .clicked()
            {
                actions.push(Action::ToggleScope);
            }
            if ui
                .selectable_label(self.timeline.tempo_lane(), "Tempo lane")
                .on_hover_text(
                    "Show the session's tempo under the ruler, to change it along the timeline",
                )
                .clicked()
            {
                actions.push(Action::ToggleTempoLane);
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
            if ui
                .selectable_label(self.plugin_browser.is_open(), "Plugins")
                .on_hover_text("Find and add CLAP plugins")
                .clicked()
            {
                actions.push(Action::TogglePlugins);
            }
            if ui
                .selectable_label(self.editor.show_cpu(), "CPU meters")
                .on_hover_text("Show how much of the audio thread's time each node takes")
                .clicked()
            {
                actions.push(Action::ToggleCpu);
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
        // The transport moves whether or not the engine is on: playing turns
        // the engine on, and stopping leaves it running.
        let playing = self.session.transport_running();
        if ui.button("⏮").on_hover_text("Back to the start").clicked() {
            actions.push(Action::Rewind);
        }
        let (label, tip) = if playing {
            (
                "⏹",
                "Stop the timeline (Space). Stopping again goes back to the start",
            )
        } else {
            ("▶", "Play the timeline from the playhead (Space)")
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
        self.engine_button(ui, actions);
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
                "Record the input onto the armed tracks (RA and RM on a track header), from the playhead",
            )
            .clicked()
        {
            actions.push(Action::ToggleRecord);
        }
        self.quantise_menu(ui, actions);
        self.keyboard_button(ui, actions);
        self.metronome_button(ui, actions);
        self.tempo_fields(ui, actions);
        self.extra_buttons(ui, actions);
        let at = self
            .session
            .project()
            .tempo_map()
            .position(self.session.playhead());
        let position = format!("{}.{}.{:03}", at.bar + 1, at.beat + 1, at.tick);
        ui.monospace(position).on_hover_text("Bar.beat.tick");
    }

    /// The session's tempo and time signature. The tempo is the one that holds
    /// at the playhead, so where the tempo lane has made changes, this edits
    /// the stretch the playhead is in.
    fn tempo_fields(&self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let map = self.session.project().tempo_map();
        let mut bpm = map.bpm_at(self.session.playhead());
        let response = ui
            .add(
                egui::DragValue::new(&mut bpm)
                    .range(noodle_core::TempoMap::MIN_BPM..=noodle_core::TempoMap::MAX_BPM)
                    .speed(0.2)
                    .max_decimals(2)
                    .suffix(" bpm"),
            )
            .on_hover_text(if map.tempos().len() > 1 {
                "The tempo where the playhead is (the tempo lane has changes)"
            } else {
                "The session's tempo"
            });
        if response.changed() {
            actions.push(Action::SetTempo {
                centi_bpm: (bpm * 100.0).round() as u32,
                drag: response.dragged(),
            });
        }
        if response.drag_stopped() {
            actions.push(Action::EndTempoDrag);
        }
        let current = map.signatures()[0].signature;
        crate::widgets::Dropdown::from_id_salt("time_signature")
            .width(44.0)
            .selected_text(current.to_string())
            .show_ui(ui, |ui| {
                for (numerator, denominator) in crate::tempo::SIGNATURES {
                    let choice = noodle_core::TimeSignature {
                        numerator,
                        denominator,
                    };
                    if ui
                        .selectable_label(choice == current, choice.to_string())
                        .clicked()
                    {
                        actions.push(Action::SetSignature(numerator, denominator));
                    }
                }
            })
            .response
            .on_hover_text("The time signature");
    }

    /// How recorded MIDI is snapped to the beat. What is heard while
    /// recording is never snapped, only the clip the take becomes.
    fn quantise_menu(&self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let current = self.session.record_quantise();
        crate::widgets::Dropdown::from_id_salt("record_quantise")
            .width(48.0)
            .selected_text(format!("Q {}", current.label()))
            .show_ui(ui, |ui| {
                for choice in crate::midi_take::Quantise::ALL {
                    if ui
                        .selectable_label(choice == current, choice.label())
                        .clicked()
                    {
                        actions.push(Action::SetRecordQuantise(choice));
                    }
                }
            })
            .response
            .on_hover_text("Snap the notes of recorded MIDI to this grid");
    }

    /// The computer keyboard as a MIDI source. While on it shows the octave
    /// and velocity the Z X C V keys set.
    /// The audio engine's switch. Lit while it runs, amber when it runs on
    /// something other than what was chosen (the hover says what).
    fn engine_button(&self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let on = self.session.engine_running();
        let warning = self.session.engine_warning();
        let (label, colour) = match (on, warning) {
            (true, Some(_)) => ("⚠ Audio", theme::WARNING),
            (true, None) => ("Audio", theme::ACCENT),
            (false, _) => ("Audio", theme::timeline::TEXT_WEAK),
        };
        let tip = match (on, warning) {
            (true, Some(warning)) => format!(
                "The audio engine is on, but not as chosen: {warning}. Click to turn it off"
            ),
            (true, None) => "The audio engine is on: the graph sounds, and live MIDI plays \
                 through the armed tracks, even with the timeline stopped. Click to turn it off"
                .to_owned(),
            (false, _) => "The audio engine is off, so nothing sounds. Click to turn it on".into(),
        };
        let label = egui::RichText::new(label).color(colour);
        if ui
            .add(egui::Button::new(label).selected(on))
            .on_hover_text(tip)
            .clicked()
        {
            actions.push(Action::ToggleEngine);
        }
    }

    fn keyboard_button(&self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let on = self.keyboard.is_on();
        let label = if on {
            format!(
                "⌨ {} · {}",
                self.keyboard.octave_label(),
                self.keyboard.velocity()
            )
        } else {
            "⌨".to_owned()
        };
        let label = egui::RichText::new(label).color(if on {
            theme::ACCENT
        } else {
            theme::timeline::TEXT
        });
        if ui
            .add(egui::Button::new(label).selected(on))
            .on_hover_text(
                "Play the MIDI In nodes, and record, from the computer keyboard (M). \
                 A W S E D F T G Y H U J K are the notes, Z and X change the octave, \
                 C and V the velocity.",
            )
            .clicked()
        {
            actions.push(Action::ToggleKeyboard);
        }
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

    /// The other Button nodes, lit while on, and a menu to bind a new one
    /// to a switch on any node.
    fn extra_buttons(&self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let project = self.session.project();
        let registry = self.session.registry();
        for extra in pill_buttons::extras(project, registry) {
            let colour = if extra.on {
                theme::ACCENT
            } else {
                theme::timeline::TEXT
            };
            let tip = if extra.target.is_empty() {
                "A button nothing is wired to".to_owned()
            } else {
                extra.target.clone()
            };
            let button =
                egui::Button::new(egui::RichText::new("⏻").color(colour)).selected(extra.on);
            if ui.add(button).on_hover_text(tip).clicked() {
                actions.push(Action::PressButton(extra.button));
            }
        }
        ui.menu_button("＋", |ui| {
            let switches = pill_buttons::switches(project, registry);
            if switches.is_empty() {
                ui.weak("No switches to bind a button to");
            }
            for (n, switch) in switches.iter().enumerate() {
                if ui.button(&switch.label).clicked() {
                    actions.push(Action::BindButton(n));
                    ui.close();
                }
            }
        })
        .response
        .on_hover_text("Add a button here that switches a node's On/Off setting");
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
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
            if let Some((fraction, name)) = self.session.split_progress() {
                ui.separator();
                ui.label(format!("Splitting {name} into stems"));
                ui.add(
                    egui::ProgressBar::new(fraction)
                        .desired_width(120.0)
                        .show_percentage(),
                );
                if ui.small_button("Cancel").clicked() {
                    self.session.cancel_split();
                }
                // The bar moves without anything else repainting.
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(100));
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
                        // Clean now, so the action runs without asking again.
                        Saved::Yes => {
                            self.confirming = None;
                            actions.push(action);
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
                    self.discarding = Some(action);
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
                if self.session.transport_running() {
                    self.session.stop();
                } else {
                    self.session.play();
                }
            }
            Action::ToggleEngine => {
                if self.session.engine_running() {
                    self.session.stop_engine();
                } else {
                    self.session.start_engine();
                }
            }
            Action::Rewind => self.session.rewind(),
            Action::SetRecordQuantise(quantise) => self.session.set_record_quantise(quantise),
            Action::ToggleKeyboard => {
                for message in self.keyboard.set_on(!self.keyboard.is_on()) {
                    self.session.midi_bus().send(message);
                }
            }
            Action::ToggleRecord => {
                if self.session.is_recording() {
                    self.session.stop_recording();
                } else {
                    self.session.record();
                }
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
            Action::PressButton(button) => {
                if let Some(command) = pill_buttons::press(self.session.project(), button) {
                    self.session.edit([Edit::Apply(command)]);
                }
            }
            Action::BindButton(n) => {
                let switches =
                    pill_buttons::switches(self.session.project(), self.session.registry());
                if let Some(switch) = switches.get(n) {
                    let button = self.session.new_node_id();
                    let command = pill_buttons::bind(
                        self.session.project(),
                        self.session.registry(),
                        switch,
                        button,
                    );
                    self.session.edit([Edit::Apply(command)]);
                }
            }
            Action::ToggleScope => self.scope_open = !self.scope_open,
            Action::TogglePlugins => self.plugin_browser.toggle(),
            Action::SetTempo { centi_bpm, drag } => {
                let project = self.session.project();
                if let Some(map) = crate::tempo::with_tempo_at(
                    project.tempo_map(),
                    self.session.playhead(),
                    f64::from(centi_bpm) / 100.0,
                ) {
                    let command = Command::SetTempoMap(map);
                    self.session.edit([if drag {
                        Edit::Drag(command)
                    } else {
                        Edit::Apply(command)
                    }]);
                }
            }
            Action::EndTempoDrag => self.session.edit([Edit::EndDrag]),
            Action::SetSignature(numerator, denominator) => {
                let signature = noodle_core::TimeSignature {
                    numerator,
                    denominator,
                };
                if let Some(map) =
                    crate::tempo::with_signature(self.session.project().tempo_map(), signature)
                {
                    self.session.edit([Edit::Apply(Command::SetTempoMap(map))]);
                }
            }
            Action::ToggleTempoLane => self.timeline.set_tempo_lane(!self.timeline.tempo_lane()),
            Action::ToggleCpu => self.editor.set_show_cpu(!self.editor.show_cpu()),
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
            Action::ExportAudio => self.export.open(&self.session),
            Action::StartExport => {
                if let Some(choice) = self.export.take_choice() {
                    self.start_export(choice);
                }
            }
            Action::ArrangeNodes => {
                self.editor.request_arrange();
                ctx.request_repaint();
            }
            Action::FitGraph => {
                self.editor.request_fit();
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
        // The plugins' timers, and what other threads asked of them, when
        // this thread hosts them (macOS, Windows).
        if let Some(wait) = noodle_plugins::pump() {
            ui.ctx().request_repaint_after(wait);
        }
        self.show(ui);
    }
}

impl App {
    /// Asks where to write the export and starts it.
    fn start_export(&mut self, choice: crate::session::ExportChoice) {
        let name = format!("{}.{}", self.session.name(), choice.format.extension());
        let mut dialog = rfd::FileDialog::new().set_file_name(name);
        if let Some(dir) = self.session.directory() {
            dialog = dialog.set_directory(dir);
        }
        let Some(path) = dialog
            .add_filter(choice.format.label(), &[choice.format.extension()])
            .save_file()
        else {
            self.export.declined();
            return;
        };
        // Some platforms' dialogs don't add the extension.
        let path = if path.extension().is_none() {
            path.with_extension(choice.format.extension())
        } else {
            path
        };
        match self.session.start_export(path, choice) {
            Ok(()) => self.export.started(),
            Err(why) => {
                self.export.declined();
                self.session.notify(why);
            }
        }
    }

    /// Asks which audio files to add, and adds them as clips at `target`.
    /// What is known about the audio file of clip `id`.
    fn clip_source(
        &mut self,
        ctx: &egui::Context,
        id: noodle_core::ClipId,
    ) -> Option<timeline::Loaded> {
        let name = self.session.project().clip(id)?.as_audio()?.source.clone();
        let directory = self.session.directory().map(std::path::Path::to_path_buf);
        self.timeline.source(ctx, directory.as_deref(), &name)
    }

    /// A clip or a track carried out of the timeline and let go of over the
    /// graph. A clip dropped on a Sampler gives it the clip's part of the
    /// file; on the canvas it becomes a Sample node holding the same.
    /// A track is placed where it lands, if it is at the level being edited.
    fn dropped_on_graph(&mut self, dropped: editor::Dropped) {
        use crate::carry::Item;
        use noodle_core::{Command, Config, Node, Position, Value};
        match dropped.item {
            Item::Clip(clip) => {
                let sample = match crate::sample_drop::from_clip(&self.session, clip) {
                    Ok(sample) => sample,
                    Err(notice) => return self.session.notify(notice),
                };
                match dropped.on {
                    editor::DropOn::Sampler(node) => {
                        let command = crate::sample_drop::onto_sampler(node, &sample);
                        self.session.edit([crate::session::Edit::Apply(command)]);
                    }
                    editor::DropOn::Canvas(at) => {
                        let id = self.session.new_node_id();
                        let mut node = Node::new(noodle_nodes::SAMPLE_INPUT_ID)
                            .with_config(Config::new().with(
                                noodle_nodes::SAMPLE_KEY,
                                Value::Text(sample.reference.to_text()),
                            ))
                            .at(at.x, at.y);
                        node.parent = dropped.group;
                        self.session
                            .edit([crate::session::Edit::Apply(Command::AddNode { id, node })]);
                        self.editor.select_only([id]);
                    }
                }
            }
            Item::Track(group) => {
                let at = match dropped.on {
                    editor::DropOn::Canvas(at) => at,
                    editor::DropOn::Sampler(node) => {
                        let p = self
                            .session
                            .project()
                            .graph()
                            .node(node)
                            .map(|n| n.position);
                        p.map_or(egui::Pos2::ZERO, |p| egui::pos2(p.x, p.y))
                    }
                };
                let graph = self.session.project().graph();
                if graph.node(group).map(|n| n.parent) != Some(dropped.group) {
                    return self.session.notify(
                        "That track is in another group. Open its level to place it.".into(),
                    );
                }
                let position = Position { x: at.x, y: at.y };
                self.session
                    .edit([crate::session::Edit::Apply(Command::MoveNode {
                        node: group,
                        position,
                    })]);
                self.editor.select_only([group]);
            }
        }
    }

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

    use crate::testkit::virtual_audio as virtual_output;

    #[test]
    fn the_audio_button_turns_the_engine_on_and_off_and_play_leaves_it_alone() {
        let mut app = empty();
        app.session.set_audio_config(virtual_output());
        app.session.start_engine();
        let mut harness = harness(app);
        harness.run_steps(3);
        assert!(harness.state().session.engine_running());
        assert!(!harness.state().session.transport_running());

        // Play and stop move the transport, never the engine.
        harness.get_by_label("▶").click();
        harness.run_steps(3);
        assert!(harness.state().session.transport_running());
        harness.get_by_label("⏹").click();
        harness.run_steps(3);
        assert!(!harness.state().session.transport_running());
        assert!(harness.state().session.engine_running());

        harness.get_by_label("Audio").click();
        harness.run_steps(3);
        assert!(!harness.state().session.engine_running());
        // Playing with the engine off turns it on.
        harness.get_by_label("▶").click();
        harness.run_steps(3);
        assert!(harness.state().session.engine_running());
        assert!(harness.state().session.transport_running());
    }

    #[test]
    fn the_audio_button_shows_when_the_engine_fell_back() {
        let mut app = empty();
        app.session.set_audio_config(noodle_io::AudioConfig {
            output: Some("alsa:no such device".into()),
            ..noodle_io::AudioConfig::default()
        });
        app.session.start_engine();
        let mut harness = harness(app);
        harness.run_steps(3);
        assert!(harness.state().session.engine_warning().is_some());
        harness.get_by_label("⚠ Audio");
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
    fn the_tempo_lane_is_switched_on_from_the_view_menu() {
        let mut harness = harness(empty());
        harness.run();
        assert!(!harness.state().timeline().tempo_lane());
        harness.get_by_label("View").click();
        harness.run();
        harness.get_by_label("Tempo lane").click();
        harness.run();
        assert!(harness.state().timeline().tempo_lane());
        harness.get_by_label("View").click();
        harness.run();
        harness.get_by_label("Tempo lane").click();
        harness.run();
        assert!(!harness.state().timeline().tempo_lane());
    }

    #[test]
    fn the_transport_tempo_and_signature_edit_the_session() {
        let mut harness = harness(empty());
        harness.run();
        let ctx = harness.ctx.clone();
        let app = harness.state_mut();
        app.run(
            &ctx,
            Action::SetTempo {
                centi_bpm: 9050,
                drag: false,
            },
        );
        let map = app.session.project().tempo_map();
        assert_eq!(map.tempos().len(), 1);
        assert_eq!(map.bpm_at(noodle_core::Tick::ZERO), 90.5);
        // A drag is one undo step, closed by its end.
        for centi_bpm in [10000, 11000, 12500] {
            app.run(
                &ctx,
                Action::SetTempo {
                    centi_bpm,
                    drag: true,
                },
            );
        }
        app.run(&ctx, Action::EndTempoDrag);
        assert_eq!(
            app.session
                .project()
                .tempo_map()
                .bpm_at(noodle_core::Tick::ZERO),
            125.0
        );
        app.session.undo();
        assert_eq!(
            app.session
                .project()
                .tempo_map()
                .bpm_at(noodle_core::Tick::ZERO),
            90.5
        );
        app.run(&ctx, Action::SetSignature(6, 8));
        assert_eq!(
            app.session.project().tempo_map().signatures()[0]
                .signature
                .to_string(),
            "6/8"
        );
        // A change down the timeline is edited where the playhead is.
        app.session.edit([Edit::Apply(Command::SetTempoMap(
            crate::tempo::with_change(
                app.session.project().tempo_map(),
                noodle_core::Tick(3840),
                100.0,
            )
            .unwrap()
            .0,
        ))]);
        app.session.seek(noodle_core::Tick(4000));
        app.run(
            &ctx,
            Action::SetTempo {
                centi_bpm: 7000,
                drag: false,
            },
        );
        let map = app.session.project().tempo_map();
        assert_eq!(map.bpm_at(noodle_core::Tick(0)), 90.5);
        assert_eq!(map.bpm_at(noodle_core::Tick(4000)), 70.0);
    }

    #[test]
    fn export_audio_opens_a_dialog_from_the_file_menu() {
        let mut harness = harness(empty());
        harness.run();
        assert!(harness.query_by_label("Export Audio").is_none());
        harness.get_by_label("File").click();
        harness.run();
        harness.get_by_label("Export Audio…").click();
        harness.run();
        harness.get_by_label("Export Audio");
        harness.get_by_label("Format");
        // Nothing to export in an empty project, and it says so.
        harness.get_by_label("There is nothing to export yet");
        harness.get_by_label("Close").click();
        harness.run();
        assert!(harness.query_by_label("Export Audio").is_none());
    }

    #[test]
    fn the_export_dialog_takes_a_range_and_refuses_a_backwards_one() {
        let mut harness = harness(empty());
        harness.run();
        harness.get_by_label("File").click();
        harness.run();
        harness.get_by_label("Export Audio…").click();
        harness.run();
        harness.get_by_label("Range").click();
        harness.run();
        harness.get_by_label("From");
        harness.get_by_label("To");
        // An empty project's range runs from 0 to 0: nothing to write.
        harness.get_by_label("The range ends before it starts");
    }

    #[test]
    fn the_plugin_browser_opens_from_the_view_menu() {
        let mut harness = harness(empty());
        harness.run();
        assert!(harness.query_by_label("Rescan").is_none());
        harness.get_by_label("View").click();
        harness.run();
        harness.get_by_label("Plugins").click();
        harness.run();
        harness.get_by_label("Rescan");
        harness.get_by_label_contains("0 of 0 plugins");
    }

    #[test]
    fn running_plugins_in_separate_processes_is_switched_from_the_plugin_browser_and_remembered() {
        let dir = tempfile::tempdir().unwrap();
        let prefs = dir.path().join("prefs.ron");
        crate::prefs::init(prefs.clone());
        let mut harness = harness(empty());
        harness.run();
        harness.get_by_label("View").click();
        harness.run();
        harness.get_by_label("Plugins").click();
        harness.run();
        assert!(!harness.state().session().sandbox_plugins());
        harness
            .get_by_label("Run plugins in separate processes")
            .click();
        harness.run();
        assert!(harness.state().session().sandbox_plugins());
        assert!(crate::prefs::load(&prefs).unwrap().sandbox_plugins);
        harness
            .get_by_label("Run plugins in separate processes")
            .click();
        harness.run();
        assert!(!harness.state().session().sandbox_plugins());
        assert!(!crate::prefs::load(&prefs).unwrap().sandbox_plugins);
        assert!(!noodle_plugins::sandbox_enabled());
    }

    #[test]
    fn cpu_meters_are_switched_on_and_off_from_the_view_menu() {
        let mut harness = harness(empty());
        harness.run();
        assert!(!harness.state().editor().show_cpu());
        for expected in [true, false] {
            harness.get_by_label("View").click();
            harness.run();
            harness.get_by_label("CPU meters").click();
            harness.run();
            assert_eq!(harness.state().editor().show_cpu(), expected);
        }
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
    fn a_toggle_parameter_is_named_once_in_the_properties() {
        let mut app = empty();
        let id = noodle_core::NodeId(1);
        app.session.edit([Edit::Apply(Command::AddNode {
            id,
            node: Node::new("noodle.instrument.sampler"),
        })]);
        let mut harness = harness(app);
        harness.run();
        let before = harness.query_all_by_label("Keytrack").count();
        harness.state_mut().editor.selected.insert(id);
        harness.state_mut().editor.active = Some(id);
        harness.run();
        // The panel adds its own, and only one.
        assert_eq!(harness.query_all_by_label("Keytrack").count(), before + 1);
    }

    #[test]
    fn opening_a_node_without_an_editor_window_does_nothing() {
        let mut app = empty();
        let id = noodle_core::NodeId(1);
        app.session.edit([Edit::Apply(Command::AddNode {
            id,
            node: Node::new("noodle.osc.sine"),
        })]);
        app.editor.request_open_for_test(id);
        app.open_requested_view();
        assert!(app.editor.take_open_request().is_none());
        assert!(!app.session.plugin_editor_open(id));
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
        assert!(!session.engine_running());
        let saved = crate::prefs::load(&prefs).unwrap();
        assert_eq!(saved.audio.buffer_size, Some(256));
    }

    #[test]
    fn the_keyboard_button_plays_notes_and_takes_the_keys_from_everything_else() {
        let mut app = empty();
        let node = Node::new("noodle.osc.sine");
        let id = noodle_core::NodeId(1);
        app.session
            .edit([Edit::Apply(Command::AddNode { id, node })]);
        let mut heard = app.session.midi_bus().subscribe();
        let mut harness = harness(app);
        harness.run();

        // Off: the keys are ordinary keys.
        harness.key_press(Key::A);
        harness.run();
        assert_eq!(heard.pop(), None);

        harness.key_press(Key::M);
        harness.run();
        assert!(harness.state().keyboard.is_on());
        harness.key_down(Key::A);
        harness.run();
        assert_eq!(heard.pop(), Some([0x90, 60, 100]));
        harness.key_up(Key::A);
        harness.run();
        assert_eq!(heard.pop(), Some([0x80, 60, 0]));

        // X is a note-keyboard key now, not delete.
        harness.key_press(Key::X);
        harness.run();
        assert_eq!(harness.state().keyboard.octave(), 4);
        assert_eq!(
            harness.state().session().project().graph().nodes().count(),
            1
        );

        // Turning it off lets go of what is held.
        harness.key_down(Key::D);
        harness.run();
        assert_eq!(heard.pop(), Some([0x90, 76, 100]));
        harness.key_press(Key::M);
        harness.run();
        assert_eq!(heard.pop(), Some([0x80, 76, 0]));
    }

    #[test]
    fn note_keys_are_taken_from_the_frame_so_nothing_else_acts_on_them() {
        let mut app = empty();
        app.keyboard.set_on(true);
        let ctx = egui::Context::default();
        let key = |key, pressed| egui::Event::Key {
            key,
            physical_key: Some(key),
            pressed,
            repeat: false,
            modifiers: Modifiers::NONE,
        };
        let input = egui::RawInput {
            events: vec![key(Key::A, true), key(Key::Delete, true)],
            ..Default::default()
        };
        let mut seen = (true, false);
        let mut output = ctx.run_ui(input, |ui| {
            app.keyboard_midi(ui.ctx());
            seen = ui
                .ctx()
                .input(|i| (i.key_pressed(Key::A), i.key_pressed(Key::Delete)));
        });
        output.textures_delta.clear();
        assert_eq!(seen, (false, true), "A is taken, Delete is not");
    }

    #[test]
    fn modified_keys_stay_shortcuts_with_the_keyboard_on() {
        let mut app = empty();
        app.keyboard.set_on(true);
        let ctx = egui::Context::default();
        let mut heard = app.session.midi_bus().subscribe();
        for modifiers in [
            Modifiers::SHIFT,
            Modifiers::CTRL,
            Modifiers::ALT,
            Modifiers::COMMAND,
        ] {
            let input = egui::RawInput {
                events: vec![egui::Event::Key {
                    key: Key::A,
                    physical_key: Some(Key::A),
                    pressed: true,
                    repeat: false,
                    modifiers,
                }],
                ..Default::default()
            };
            let mut seen = false;
            let mut output = ctx.run_ui(input, |ui| {
                app.keyboard_midi(ui.ctx());
                seen = ui.ctx().input(|i| i.key_pressed(Key::A));
            });
            output.textures_delta.clear();
            assert!(seen, "{modifiers:?}+A is left for the app");
        }
        assert_eq!(heard.pop(), None);
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

    #[test]
    fn the_plus_menu_binds_a_new_pill_button_that_presses_a_switch() {
        let mut harness = harness(empty());
        harness.run();
        // The metronome's own button makes a metronome with a switch left.
        harness.get_by_label("♩").click();
        harness.run();
        harness.get_by_label("＋").click();
        harness.run();
        harness.get_by_label("Metronome · Accent").click();
        harness.run();
        let shown = || {
            let state = harness.state();
            pill_buttons::extras(state.session.project(), state.session.registry())
        };
        assert_eq!(shown().len(), 1);
        assert!(shown()[0].on);
        harness.get_by_label("⏻").click();
        harness.run();
        let state = harness.state();
        let extras = pill_buttons::extras(state.session.project(), state.session.registry());
        assert!(!extras[0].on);
    }

    /// A Sampler holding a one second sample, selected, in a harness.
    fn sampler_rig() -> (Harness<'static, App>, noodle_core::NodeId) {
        let mut app = empty();
        let hash = app.session.samples().insert(&noodle_io::Audio {
            samples: (0..48_000).map(|i| (i as f32 * 0.05).sin()).collect(),
            channels: 1,
            sample_rate: 48_000,
        });
        let id = noodle_core::NodeId(1);
        app.session.edit([Edit::Apply(Command::AddNode {
            id,
            node: Node::new(noodle_nodes::SAMPLER_ID),
        })]);
        app.session.edit([Edit::Apply(Command::SetConfig {
            node: id,
            key: noodle_nodes::SAMPLE_KEY.to_owned(),
            value: Some(noodle_core::Value::Text(hash)),
        })]);
        app.editor.selected.insert(id);
        app.editor.active = Some(id);
        // Frames short enough for two clicks to make a double click.
        let mut h = Harness::builder()
            .with_step_dt(1.0 / 60.0)
            .build_ui_state(|ui, app: &mut App| app.show(ui), app);
        h.run();
        (h, id)
    }

    #[test]
    fn the_samplers_properties_show_its_sample_and_a_double_click_opens_the_view() {
        use crate::testkit::double_click;
        let (mut h, id) = sampler_rig();
        assert!(h.state().sample_view.node().is_none());
        let at = h.get_by_label("Sample waveform").rect().center();
        double_click(&mut h, at);
        h.run();
        assert_eq!(h.state().sample_view.node(), Some(id));
        h.get_by_label("Sample · 1.00 s at 48000 Hz");
        // Close shuts it again.
        h.get_by_label("Close").click();
        h.run();
        assert!(h.state().sample_view.node().is_none());
    }

    #[test]
    fn dragging_the_start_marker_in_the_sample_view_sets_the_start_parameter() {
        use crate::testkit::{double_click, drag_between};
        let (mut h, id) = sampler_rig();
        let at = h.get_by_label("Sample waveform").rect().center();
        double_click(&mut h, at);
        h.run();
        let body = h
            .ctx
            .data(|d| d.get_temp::<egui::Rect>(egui::Id::new("sample view body")))
            .expect("the view has been drawn");
        let from = egui::Pos2::new(body.left() + 1.0, body.center().y);
        let to = egui::Pos2::new(body.left() + body.width() * 0.25, body.center().y);
        drag_between(&mut h, egui::Modifiers::NONE, from, to);
        let start = h.state().session.project().graph().node(id).unwrap().params["start"];
        assert!((start - 0.25).abs() < 0.01, "{start}");
        // One undo step.
        h.state_mut().session.undo();
        let node = h.state().session.project().graph().node(id).unwrap();
        assert!(!node.params.contains_key("start"));
    }

    /// An app saved in a temp folder with one track holding a clip of a
    /// 440 Hz tone, from a quarter of the file for a quarter of it, and the
    /// folder to keep alive. Returns the clip and the window it should give.
    fn clip_rig() -> (
        Harness<'static, App>,
        noodle_core::ClipId,
        tempfile::TempDir,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let tone: Vec<f32> = (0..48_000)
            .map(|i| 0.5 * (std::f32::consts::TAU * 440.0 * i as f32 / 48_000.0).sin())
            .collect();
        noodle_io::write_wav(&dir.path().join("tone.wav"), &tone, 1, 48_000).unwrap();
        let mut app = empty();
        assert!(app.session.save_as(&dir.path().join("p.ron")));
        let track = noodle_core::NodeId(1);
        app.session.edit([Edit::Apply(Command::AddNode {
            id: track,
            node: Node::new(noodle_nodes::TRACK_INPUT_ID),
        })]);
        let id = app.session.project().next_clip_id();
        let mut clip = noodle_core::Clip::audio(track, noodle_core::Tick(0), "tone.wav", 12_000);
        if let noodle_core::ClipContent::Audio(audio) = &mut clip.content {
            audio.offset = 12_000;
        }
        app.session
            .edit([Edit::Apply(Command::AddClip { id, clip })]);
        let mut h = Harness::builder()
            .with_size(egui::vec2(1100.0, 800.0))
            .with_step_dt(1.0 / 60.0)
            .build_ui_state(|ui, app: &mut App| app.show(ui), app);
        h.run();
        (h, id, dir)
    }

    /// Drags the clip out of the timeline to `to`.
    fn carry_clip(h: &mut Harness<'static, App>, id: noodle_core::ClipId, to: egui::Pos2) {
        let from = h
            .state()
            .timeline
            .clip_rect(id)
            .expect("the clip is drawn")
            .center();
        crate::testkit::drag_between(h, egui::Modifiers::NONE, from, to);
        h.run();
    }

    fn on_canvas(h: &Harness<'static, App>, graph: egui::Pos2) -> egui::Pos2 {
        let editor = &h.state().editor;
        editor.view.on(editor.canvas).to_screen(graph)
    }

    #[test]
    fn a_clip_dragged_onto_the_canvas_becomes_a_sample_node_and_stays_put() {
        let (mut h, clip, _dir) = clip_rig();
        let before = h.state().session.project().clip(clip).unwrap().clone();
        let to = on_canvas(&h, egui::pos2(200.0, 150.0));
        carry_clip(&mut h, clip, to);
        let project = h.state().session.project();
        assert_eq!(project.clip(clip), Some(&before), "the clip did not move");
        let (_, node) = project
            .graph()
            .nodes()
            .find(|(_, n)| n.type_id == noodle_nodes::SAMPLE_INPUT_ID)
            .expect("a Sample node");
        let reference = match node.config.get(noodle_nodes::SAMPLE_KEY) {
            Some(noodle_core::Value::Text(t)) => noodle_nodes::SampleRef::parse(t),
            other => panic!("{other:?}"),
        };
        assert!((reference.start - 0.25).abs() < 1e-4, "{reference:?}");
        assert!((reference.end - 0.5).abs() < 1e-4, "{reference:?}");
        assert!((node.position.x - 200.0).abs() < 8.0 && (node.position.y - 150.0).abs() < 8.0);
    }

    #[test]
    fn a_clip_dragged_onto_a_sampler_sets_its_sample_window_and_root_in_one_undo_step() {
        let (mut h, clip, _dir) = clip_rig();
        let sampler = noodle_core::NodeId(10);
        h.state_mut().session.edit([Edit::Apply(Command::AddNode {
            id: sampler,
            node: Node::new(noodle_nodes::SAMPLER_ID).at(300.0, 200.0),
        })]);
        h.run();
        let to = on_canvas(&h, egui::pos2(300.0, 200.0)) + egui::vec2(20.0, 10.0);
        carry_clip(&mut h, clip, to);
        let node = h
            .state()
            .session
            .project()
            .graph()
            .node(sampler)
            .unwrap()
            .clone();
        assert!(matches!(
            node.config.get(noodle_nodes::SAMPLE_KEY),
            Some(noodle_core::Value::Text(t)) if t.len() == 64
        ));
        assert!(
            (node.params["start"] - 0.25).abs() < 1e-4,
            "{:?}",
            node.params
        );
        assert!((node.params["end"] - 0.5).abs() < 1e-4, "{:?}", node.params);
        // A 440 Hz tone is key 69.
        assert!(
            (node.params["root"] - 69.0).abs() < 0.1,
            "{:?}",
            node.params
        );
        assert!(
            !h.state()
                .session
                .project()
                .graph()
                .nodes()
                .any(|(_, n)| n.type_id == noodle_nodes::SAMPLE_INPUT_ID)
        );
        h.state_mut().session.undo();
        let node = h.state().session.project().graph().node(sampler).unwrap();
        assert!(node.params.is_empty() && node.config.get(noodle_nodes::SAMPLE_KEY).is_none());
    }

    #[test]
    fn a_clip_let_go_inside_the_timeline_is_an_ordinary_move() {
        let (mut h, clip, _dir) = clip_rig();
        let from = h.state().timeline.clip_rect(clip).unwrap().center();
        crate::testkit::drag_between(
            &mut h,
            egui::Modifiers::NONE,
            from,
            from + egui::vec2(40.0, 0.0),
        );
        h.run();
        let project = h.state().session.project();
        assert!(project.clip(clip).unwrap().start > noodle_core::Tick(0));
        assert!(
            !project
                .graph()
                .nodes()
                .any(|(_, n)| n.type_id == noodle_nodes::SAMPLE_INPUT_ID)
        );
    }

    #[test]
    fn a_track_dragged_onto_the_canvas_is_placed_there() {
        let (mut h, _, _dir) = clip_rig();
        h.state_mut().session.edit([Edit::Apply(
            noodle_core::group::create_track(None, noodle_core::Position { x: 0.0, y: 0.0 }, {
                let mut next = 20;
                move || {
                    next += 1;
                    noodle_core::NodeId(next)
                }
            })
            .1,
        )]);
        h.run();
        let group = h
            .state()
            .session
            .project()
            .graph()
            .nodes()
            .find(|(_, n)| n.type_id == noodle_core::group::GROUP)
            .map(|(id, _)| id)
            .expect("a track group");
        // The rig's own bare track is the first lane; the group is the second.
        let header = h.get_by_label("Track 2").rect();
        let to = on_canvas(&h, egui::pos2(400.0, 250.0));
        crate::testkit::drag_between(
            &mut h,
            egui::Modifiers::NONE,
            header.left_bottom() + egui::vec2(4.0, 12.0),
            to,
        );
        h.run();
        let at = h
            .state()
            .session
            .project()
            .graph()
            .node(group)
            .unwrap()
            .position;
        assert!(
            (at.x - 400.0).abs() < 8.0 && (at.y - 250.0).abs() < 8.0,
            "{at:?}"
        );
    }

    #[test]
    fn a_clip_dragged_onto_the_sample_in_the_properties_panel_sets_the_selected_sampler() {
        let (mut h, clip, _dir) = clip_rig();
        let sampler = noodle_core::NodeId(10);
        h.state_mut().session.edit([Edit::Apply(Command::AddNode {
            id: sampler,
            node: Node::new(noodle_nodes::SAMPLER_ID).at(300.0, 200.0),
        })]);
        h.state_mut().editor.selected.insert(sampler);
        h.state_mut().editor.active = Some(sampler);
        h.run();
        // Selecting the clip puts its own panel above the Sampler's settings,
        // so click it first and aim at where the settings are then.
        let at = h.state().timeline.clip_rect(clip).unwrap().center();
        crate::testkit::click(&mut h, at, egui::Modifiers::NONE);
        h.run();
        h.state_mut().editor.selected.insert(sampler);
        h.state_mut().editor.active = Some(sampler);
        h.run();
        let to = h.get_by_label("Load sample…").rect().center();
        carry_clip(&mut h, clip, to);
        let node = h.state().session.project().graph().node(sampler).unwrap();
        assert!(
            (node.params["start"] - 0.25).abs() < 1e-4,
            "{:?}",
            node.params
        );
        assert!(
            (node.params["root"] - 69.0).abs() < 0.1,
            "{:?}",
            node.params
        );
    }
}
