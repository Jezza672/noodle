//! The window: menus, the toolbar, the panels, and keyboard shortcuts.

use std::path::{Path, PathBuf};

use egui::{Key, KeyboardShortcut, Modifiers};
use noodle_core::{Command, NodeId};
use noodle_engine::OUTPUT_ID;

pub(crate) mod commands;

use crate::cache_panel::CachePanel;
use crate::clip_editor::{self, ClipEditor};
use crate::clip_panel::{self, ClipPanel};
use crate::devices::DevicePicker;
use crate::editor::{self, EditorState};
use crate::export_dialog::ExportDialog;
use crate::midi_ports::MidiPortsView;
use crate::outputs::OutputsView;
use crate::piano_roll::{self, PianoRoll};
use crate::plugin_browser::PluginBrowser;
use crate::session::{CacheStatus, Edit, Saved, Session};
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
/// Shown in the Edit menu. The arrangement handles the keys itself, with the
/// pointer over it, so these are not in [`SHORTCUTS`].
const CUT: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::X);
const COPY: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::C);
const PASTE: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::V);
const DUPLICATE: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::D);
/// Opens the command palette.
const PALETTE: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::K);
/// Taps the tempo. Handled apart from [`SHORTCUTS`], because T is a note on
/// the computer keyboard while that is on.
const TAP: KeyboardShortcut = KeyboardShortcut::new(Modifiers::NONE, Key::T);

/// Where the properties panel's body is scrolled to, in egui's own state.
fn properties_scroll() -> egui::Id {
    egui::Id::new("properties scroll")
}

/// The global shortcuts, for the usability scorecard to check that it knows
/// each one.
#[cfg(test)]
pub(crate) fn shortcut_chords() -> Vec<KeyboardShortcut> {
    commands::ENTRIES
        .iter()
        .filter(|e| e.global)
        .filter_map(|e| e.default)
        .collect()
}

pub struct App {
    /// The view the Edit menu's Cut, Copy, Paste and Duplicate act on: the
    /// last one clicked in.
    focus: Focus,
    session: Session,
    /// Changes to the mix version list made in the toolbar this frame.
    version_edits: std::cell::RefCell<Vec<noodle_core::Command>>,
    editor: EditorState,
    timeline: TimelineState,
    piano_roll: PianoRoll,
    clip_editor: ClipEditor,
    clip_panel: ClipPanel,
    /// The track the piano roll last played keys on, to let them go.
    audition_track: Option<noodle_core::NodeId>,
    /// The clip that was selected on the timeline last frame, so the open
    /// piano roll follows a change of selection and nothing else.
    roll_followed: Option<noodle_core::ClipId>,
    devices: DevicePicker,
    display: crate::display::Display,
    display_dialog: crate::display::DisplayDialog,
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
    cache_panel: CachePanel,
    outputs: OutputsView,
    /// Whether the MIDI ports view is showing.
    midi_ports_open: bool,
    midi_ports: MidiPortsView,
    /// The computer keyboard as a MIDI source. Off at start and not saved.
    keyboard: crate::keyboard::KeyboardMidi,
    /// Writes the unsaved project out now and then. `None` in tests and when
    /// there is no data directory.
    autosave: Option<crate::autosave::Autosave>,
    /// An autosave a crashed run left, waiting for the user to recover or
    /// discard it. Nothing is autosaved meanwhile, so it isn't overwritten.
    recovery: Option<crate::autosave::Recovery>,
    /// The command palette, and the keys the user bound.
    palette: commands::Palette,
    keymap: commands::Keymap,
    /// The saved project templates, listed when the File menu opens.
    templates: Vec<(String, PathBuf)>,
    /// A template name being typed, once "Save as Project Template" is chosen.
    template_name: Option<String>,
}

/// Something the user asked for, from a menu or a shortcut. Collected during
/// the frame and run afterwards, so nothing slow (file dialogs, file and
/// device I/O) happens while egui is mid-frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Focus {
    Arrangement,
    Notes,
    Graph,
}

/// What the palette can do to the selected audio clip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SelectedClipAction {
    SplitToStems,
    SliceToSampler,
    PinTransients,
    ConvertToMidi(crate::session::convert::Detect),
}

/// The width of the tempo field, which holds "120.00 bpm" and "120.00" alike.
const BPM_WIDTH: f32 = 84.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    New,
    Open,
    Save,
    SaveAs,
    Undo,
    Redo,
    TogglePlayback,
    /// Counts a tap of the tempo (the Tap button or `T`).
    TapTempo,
    SetTempoSource(crate::tempo_source::TempoSource),
    /// Makes the live tempo a tempo change in the map at the playhead.
    WriteTempoToMap,
    Rewind,
    /// Moves the playhead: a position typed or scrubbed in the transport.
    Seek(noodle_core::Tick),
    ToggleRecord,
    ToggleKeyboard,
    /// The octave and the velocity the computer keyboard plays at.
    SetKeyboardOctave(i8),
    SetKeyboardVelocity(u8),
    /// Turns the playback cache's own picks on or off.
    SetAutoCache(bool),
    /// How the MIDI of the next take is snapped.
    SetRecordQuantise(crate::midi_take::Quantise),
    SetRecordMidi(crate::midi_take::MidiRecord),
    /// Bars of count-in and of pre-roll before a recording.
    SetCountIn(u8),
    SetPreRoll(u8),
    SetAutomationMode(crate::session::record_automation::AutomationMode),
    /// Turns the audio engine on or off.
    ToggleEngine,
    /// Stops listening to monitored nodes (Escape).
    StopMonitor,
    /// Stops listening for MIDI learn.
    StopLearningMidi,
    /// Chooses the device the monitor plays on: the nth listed output, or
    /// the main output for `None`.
    SetMonitorDevice(Option<usize>),
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
    ToggleMidiPorts,
    TogglePlugins,
    ToggleCache,
    ToggleCpu,
    DisplaySettings,
    ImportAudio,
    ExportAudio,
    /// Asks where the export the dialog chose is to be written.
    StartExport,
    DeleteSelection,
    /// Cuts, copies, pastes or duplicates arrangement clips.
    Clips(timeline::ClipAction),
    /// Does an audio-clip action to the selected audio clip, so the
    /// palette can reach what the clip's right-click menu and panel offer.
    OnSelectedClip(SelectedClipAction),
    /// Starts the project's list of mix versions, or adds one to it.
    AddMixVersion,
    ArrangeNodes,
    /// Fits the node editor's view around the whole graph at this level.
    FitGraph,
    Close,
    /// Opens the command palette.
    OpenPalette,
    /// Asks for a name and saves the project as a template.
    SaveTemplate,
    /// Starts a project from the nth template listed.
    NewFromTemplate(usize),
    /// Folds or unfolds every folder of tracks.
    FoldFolders(bool),
    /// Turns the punch range on or off.
    TogglePunch,
    /// Moves the punch-in or punch-out to the playhead.
    SetPunchIn,
    SetPunchOut,
    /// Names the playhead's place as a marker.
    AddMarker,
    /// Says where a feature that works on a selection lives. The palette has
    /// no selection to act on, so this is its entry for those.
    Guide(&'static str),
}

impl Action {
    /// Whether it throws the current project away.
    fn discards(self) -> bool {
        matches!(
            self,
            Self::New | Self::Open | Self::Close | Self::NewFromTemplate(_)
        )
    }

    /// Whether a focused text field gets its key first. Space types a
    /// space, and Cmd+Z undoes the text, not the project edit before it. The
    /// others (New, Open, Save and the rest) are chords a text field has no
    /// use for; a new shortcut should ask which kind it is.
    fn yields_to_text(self) -> bool {
        matches!(
            self,
            Self::TogglePlayback | Self::TapTempo | Self::ToggleKeyboard | Self::Undo | Self::Redo
        )
    }
}

/// The Automation node an edit from the sequencer adds, if it adds one.
fn added_automation_node(edits: &[Edit]) -> Option<noodle_core::NodeId> {
    fn find(command: &Command) -> Option<noodle_core::NodeId> {
        match command {
            Command::AddNode { id, node } if node.type_id == noodle_nodes::AUTOMATION_NODE_ID => {
                Some(*id)
            }
            Command::Batch(commands) => commands.iter().find_map(find),
            _ => None,
        }
    }
    edits.iter().find_map(|edit| match edit {
        Edit::Apply(command) | Edit::Drag(command) => find(command),
        _ => None,
    })
}

impl App {
    pub fn new(session: Session) -> Self {
        let templates = session.templates();
        let mut app = Self {
            session,
            version_edits: std::cell::RefCell::new(Vec::new()),
            editor: EditorState::default(),
            timeline: TimelineState::default(),
            focus: Focus::Graph,
            piano_roll: PianoRoll::default(),
            clip_editor: ClipEditor::default(),
            clip_panel: ClipPanel::default(),
            audition_track: None,
            roll_followed: None,
            devices: DevicePicker::default(),
            display: crate::prefs::display(),
            display_dialog: Default::default(),
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
            cache_panel: CachePanel::default(),
            outputs: OutputsView::default(),
            midi_ports_open: false,
            midi_ports: MidiPortsView::default(),
            keyboard: crate::keyboard::KeyboardMidi::default(),
            autosave: None,
            recovery: None,
            palette: commands::Palette::default(),
            keymap: commands::Keymap::from_saved(&crate::prefs::keymap()),
            templates,
            template_name: None,
        };
        app.editor.set_display(app.display);
        app
    }

    /// Turns autosave on, and offers whatever a crashed run left in it.
    pub fn set_autosave(&mut self, autosave: crate::autosave::Autosave) {
        self.recovery = autosave.pending();
        self.autosave = Some(autosave);
    }

    #[cfg(test)]
    pub fn session_mut(&mut self) -> &mut Session {
        &mut self.session
    }

    #[cfg(test)]
    pub fn timeline_mut(&mut self) -> &mut TimelineState {
        &mut self.timeline
    }

    #[cfg(test)]
    pub fn timeline(&self) -> &TimelineState {
        &self.timeline
    }

    /// The export dialog's ticks (see [`ExportDialog::ticked`]) and whether it is open.
    #[cfg(test)]
    pub fn export_ticks(&self) -> (bool, bool, Vec<String>) {
        let (mixdown, groups) = self.export.ticked();
        (self.export.is_open(), mixdown, groups)
    }

    #[cfg(test)]
    pub fn session(&self) -> &Session {
        &self.session
    }

    #[cfg(test)]
    pub fn piano_roll(&self) -> &PianoRoll {
        &self.piano_roll
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
        #[cfg(test)]
        crate::testkit::clear_views();
        self.session.maintain();
        // The monitor menu lists devices from the Outputs view's listing.
        self.outputs
            .ensure_listed(self.session.audio_config().host.as_deref());
        // Before the shortcuts, so the note keys are not also delete and the
        // like.
        self.keyboard_midi(ui.ctx());
        // Before the panels, so a focused button doesn't also see Space.
        let mut actions = self.shortcuts(ui.ctx());

        egui::Panel::top("menu").show(ui, |ui| {
            #[cfg(test)]
            crate::testkit::mark_view(ui, "menu");
            egui::MenuBar::new().ui(ui, |ui| self.top_bar(ui, &mut actions));
        });

        egui::Panel::bottom("status").show(ui, |ui| {
            #[cfg(test)]
            crate::testkit::mark_view(ui, "status");
            self.status_bar(ui)
        });

        if self.mixer_open {
            egui::Panel::bottom("mixer")
                .resizable(false)
                .show(ui, |ui| {
                    #[cfg(test)]
                    crate::testkit::mark_view(ui, "mixer");
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
                    #[cfg(test)]
                    crate::testkit::mark_view(ui, "outputs");
                    let edits = self.outputs.show(ui, &self.session);
                    self.session.edit(edits);
                });
        }

        if self.midi_ports_open {
            egui::Panel::bottom("midi ports")
                .resizable(false)
                .show(ui, |ui| {
                    #[cfg(test)]
                    crate::testkit::mark_view(ui, "midi ports");
                    let edits = self
                        .midi_ports
                        .show(ui, self.session.project(), || self.session.new_node_id());
                    self.session.edit(edits);
                });
        }

        if self.scope_open {
            egui::Panel::bottom("scope")
                .resizable(false)
                .show(ui, |ui| {
                    #[cfg(test)]
                    crate::testkit::mark_view(ui, "scope");
                    self.scope_pane(ui)
                });
        }

        let active = self.editor.active;
        let properties_edits = egui::Panel::right("properties")
            .default_size(theme::PROPERTIES_WIDTH)
            .frame(egui::Frame::side_top_panel(ui.style()).fill(theme::PANEL))
            .show(ui, |ui| {
                #[cfg(test)]
                crate::testkit::mark_view(ui, "properties");
                // A panel is remembered at the size of its contents, so
                // short contents (nothing selected) would shrink it, and
                // the next node would open it at its default width.
                ui.set_min_width(ui.available_width());
                ui.add_space(4.0);
                // The panel is as tall as the window, so a node with more
                // properties than fit would draw them off screen, out of
                // reach. The body scrolls instead. The offset is ours, kept
                // in egui's own state, so it survives from frame to frame and
                // from one selected node to the next.
                // The scroll bar is hidden: a solid one would take width
                // from the fields, and a floating one would lie over them.
                // The wheel over the panel scrolls it.
                let offset = ui
                    .ctx()
                    .data_mut(|d| d.get_temp::<f32>(properties_scroll()))
                    .unwrap_or(0.0);
                let body = egui::ScrollArea::vertical()
                    .id_salt("properties body")
                    .auto_shrink([false, false])
                    .vertical_scroll_offset(offset)
                    .scroll_bar_visibility(
                        egui::containers::scroll_area::ScrollBarVisibility::AlwaysHidden,
                    )
                    .scroll_source(egui::containers::scroll_area::ScrollSource {
                        // A drag belongs to the fields, not to the panel.
                        drag: egui::containers::scroll_area::DragScroll::Never,
                        ..Default::default()
                    })
                    .show(ui, |ui| {
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
                        edits
                    });
                if body.state.offset.y != offset {
                    // The wheel moved it, or what is under it no longer
                    // reaches that far down, so it was drawn at the wrong
                    // place. Keep the new offset and draw it again.
                    ui.ctx()
                        .data_mut(|d| d.insert_temp(properties_scroll(), body.state.offset.y));
                    ui.ctx().request_repaint();
                }
                body.inner
            })
            .inner;
        self.session.edit(properties_edits);

        let arrangement = egui::Panel::top("arrangement")
            .resizable(true)
            .default_size(theme::timeline::DEFAULT_HEIGHT)
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                #[cfg(test)]
                crate::testkit::mark_view(ui, "timeline");
                let playhead = Some(self.session.playhead());
                let out = timeline::show(ui, &mut self.timeline, &self.session, playhead);
                let automation = added_automation_node(&out.edits);
                self.session.edit(out.edits);
                if let Some(tick) = out.seek {
                    self.session.seek(tick);
                }
                if let Some(notice) = out.notice {
                    self.session.notify(notice);
                }
                #[cfg(test)]
                ui.data_mut(|d| {
                    d.insert_temp(
                        egui::Id::new("arrangement content height"),
                        ui.min_rect().height(),
                    );
                });
                if let Some(node) = automation {
                    self.editor.reveal_top_level(node);
                }
                if let Some(region) = out.set_loop {
                    self.session.set_loop(region);
                }
                if !ui.input(|i| i.pointer.any_down()) {
                    self.session.settle_loop();
                }
                if let Some(target) = out.pick {
                    self.import_audio(target);
                }
                if let Some(clip) = out.open_midi {
                    self.clip_editor.close();
                    self.piano_roll.open(clip);
                }
                if let Some(audition) = out.audition {
                    self.session.set_take_audition(audition);
                }
                if let Some(clip) = out.open_audio {
                    self.piano_roll.close();
                    self.clip_editor.open(clip);
                }
                for (track, kind, on) in out.arm {
                    self.session.arm_kind(track, kind, on);
                }
            });

        // A double-click on a Sampler's preview in the properties panel.
        if let Some(node) = crate::sample_view::take_request(ui.ctx()) {
            self.piano_roll.close();
            self.clip_editor.open_sampler(node);
        }

        // "Edit" on one of a Pattern Player's slots in the properties panel.
        if let Some(clip) = crate::pattern_panel::take_request(ui.ctx()) {
            self.clip_editor.close();
            self.piano_roll.open(clip);
        }

        // An open roll follows the clip selected on the timeline, not only the
        // one double-clicked.
        let selected = self.timeline.selected_clip();
        if selected != self.roll_followed {
            self.roll_followed = selected;
            if let Some(clip) = selected
                && self.piano_roll.clip().is_some()
                && self
                    .session
                    .project()
                    .clip(clip)
                    .is_some_and(|c| c.as_midi().is_some())
            {
                self.piano_roll.open(clip);
            }
        }

        let mut roll_rect = None;
        if let Some(clip) = self.piano_roll.clip() {
            roll_rect = Some(
                egui::Panel::top("piano roll")
                    .resizable(true)
                    .default_size(piano_roll::DEFAULT_HEIGHT)
                    .frame(egui::Frame::NONE.fill(theme::PANEL))
                    .show(ui, |ui| {
                        #[cfg(test)]
                        crate::testkit::mark_view(ui, "piano roll");
                        let playhead = Some(self.session.playhead());
                        let out =
                            piano_roll::show(ui, &mut self.piano_roll, &self.session, playhead);
                        self.session.edit(out.edits);
                        if let Some(at) = out.seek {
                            self.session.seek(at);
                        }
                        if let Some(track) = self.session.project().clip(clip).map(|c| c.node) {
                            self.audition_track = Some(track);
                            for (key, on) in out.audition {
                                self.session.audition(track, key, on);
                            }
                        }
                    })
                    .response
                    .rect,
            );
        }
        self.clip_editor.retain_existing(self.session.project());
        if let Some(subject) = self.clip_editor.subject() {
            egui::Panel::top("clip editor")
                .resizable(true)
                .default_size(clip_editor::DEFAULT_HEIGHT)
                .frame(egui::Frame::NONE.fill(theme::PANEL))
                .show(ui, |ui| {
                    #[cfg(test)]
                    crate::testkit::mark_view(ui, "clip editor");
                    let loaded = match subject {
                        clip_editor::subject::Subject::Clip(clip) => {
                            self.clip_source(ui.ctx(), clip)
                        }
                        clip_editor::subject::Subject::Sampler(node) => {
                            crate::sample_view::loaded(ui.ctx(), &self.session, node)
                        }
                    };
                    let linked = self.linked_candidates(ui.ctx(), subject);
                    let playhead = Some(self.session.playhead());
                    let out = clip_editor::show(
                        ui,
                        &mut self.clip_editor,
                        &self.session,
                        loaded.as_ref(),
                        linked.as_ref().map(|(own, lanes)| (*own, lanes.as_slice())),
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

        let graph = egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(theme::CANVAS))
            .show(ui, |ui| {
                #[cfg(test)]
                crate::testkit::mark_view(ui, "node editor");
                self.switch_strip(ui, &mut actions);
                let edits = editor::show(ui, &mut self.editor, &self.session);
                self.session.edit(edits);
                self.open_requested_view();
            });
        self.track_focus(
            ui.ctx(),
            arrangement.response.rect,
            roll_rect,
            graph.response.rect,
        );
        if let Some(dropped) = self.editor.take_dropped() {
            self.dropped_on_graph(dropped);
        }
        if let Some(why) = self.editor.take_refusal() {
            self.session.notify(why);
        }
        if let Some(dropped) = self.editor.take_dropped_files() {
            self.files_dropped_on_graph(dropped);
        }

        self.confirm_dialog(ui.ctx(), &mut actions);
        self.recovery_dialog(ui.ctx());
        self.autosave_tick(ui.ctx());
        let edits = self.plugin_browser.show(ui.ctx(), &mut self.session);
        self.session.edit(edits);
        self.cache_panel.show(ui.ctx(), &mut self.session);
        self.export.show(ui.ctx(), &mut self.session);
        self.palette_and_templates(ui.ctx(), &mut actions);
        crate::stems_dialog::show(ui.ctx(), &mut self.session);
        crate::sample_set_dialog::show(ui.ctx(), &mut self.session);
        if self.export.has_choice() {
            actions.push(Action::StartExport);
        }
        if let Some(config) = self.devices.show(ui.ctx(), self.session.latency_report()) {
            self.session.set_audio_config(config);
            crate::prefs::remember_audio(self.session.audio_config());
        }
        if self.display_dialog.show(ui.ctx(), &mut self.display) {
            crate::prefs::remember_display(self.display);
            self.editor.set_display(self.display);
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
        // Sleep until something has news (jobs and scans wake the window) or
        // the session has something to look at again.
        let now = std::time::Instant::now();
        if let Some(deadline) = self.session.next_deadline(now) {
            ui.ctx()
                .request_repaint_after(deadline.saturating_duration_since(now));
        }
        if let Some(action) = self.discarding.take() {
            self.run(ui.ctx(), action);
        }
        let versions: Vec<_> = self.version_edits.borrow_mut().drain(..).collect();
        self.session
            .edit(versions.into_iter().map(Edit::Apply).collect::<Vec<_>>());
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
        let dialog = self.devices.is_open()
            || self.display_dialog.is_open()
            || self.export.is_open()
            || self.confirming.is_some()
            || self.palette.is_open()
            || self.template_name.is_some();
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
        let dialog = self.devices.is_open()
            || self.display_dialog.is_open()
            || self.export.is_open()
            || self.confirming.is_some()
            || self.palette.is_open()
            || self.template_name.is_some();
        ctx.input_mut(|input| {
            let mut actions = Vec::new();
            let keymap = if dialog {
                Vec::new()
            } else {
                self.keymap.global()
            };
            for (shortcut, entry) in keymap {
                // A bare key is a letter for a text field to type.
                let yields = entry.action.yields_to_text() || shortcut.modifiers == Modifiers::NONE;
                if !(typing && yields) && input.consume_shortcut(&shortcut) {
                    actions.push(entry.action);
                }
            }
            if !dialog && !typing && !self.keyboard.is_on() && input.consume_shortcut(&TAP) {
                actions.push(Action::TapTempo);
            }
            // Escape stops the monitor, but only while there is one, so the
            // other things Escape closes keep it otherwise.
            if !dialog
                && !typing
                && !self.session.monitored().is_empty()
                && input.consume_key(Modifiers::NONE, Key::Escape)
            {
                actions.push(Action::StopMonitor);
            }
            if !dialog
                && !typing
                && self.session.learning_midi()
                && input.consume_key(Modifiers::NONE, Key::Escape)
            {
                actions.push(Action::StopLearningMidi);
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
        let template = std::cell::Cell::new(None);
        let bind = std::cell::Cell::new(None);
        ui.menu_button("File", |ui| {
            item(ui, "New", Some(&NEW), true, Action::New);
            item(ui, "Open…", Some(&OPEN), true, Action::Open);
            item(ui, "Save", Some(&SAVE), true, Action::Save);
            item(ui, "Save As…", Some(&SAVE_AS), true, Action::SaveAs);
            ui.separator();
            item(
                ui,
                "Save as Project Template…",
                None,
                true,
                Action::SaveTemplate,
            );
            ui.add_enabled_ui(!self.templates.is_empty(), |ui| {
                ui.menu_button("New from Template", |ui| {
                    for (n, (name, _)) in self.templates.iter().enumerate() {
                        if ui.button(name).clicked() {
                            template.set(Some(n));
                            ui.close();
                        }
                    }
                });
            });
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
            let (clips, pasteable) = match self.focus {
                Focus::Arrangement => (self.timeline.can_copy(), self.timeline.can_paste()),
                Focus::Notes => (self.piano_roll.can_copy(), self.piano_roll.can_paste()),
                Focus::Graph => (self.editor.has_selection(), self.editor.can_paste()),
            };
            for (text, shortcut, enabled, action) in [
                ("Cut", &CUT, clips, timeline::ClipAction::Cut),
                ("Copy", &COPY, clips, timeline::ClipAction::Copy),
                ("Paste", &PASTE, pasteable, timeline::ClipAction::Paste),
                (
                    "Duplicate",
                    &DUPLICATE,
                    clips,
                    timeline::ClipAction::Duplicate,
                ),
            ] {
                item(ui, text, Some(shortcut), enabled, Action::Clips(action));
            }
            let ranged = self.focus == Focus::Arrangement && self.timeline.has_range();
            item(
                ui,
                "Insert Time",
                None,
                ranged,
                Action::Clips(timeline::ClipAction::InsertTime),
            );
            item(
                ui,
                "Delete Time",
                None,
                ranged,
                Action::Clips(timeline::ClipAction::DeleteTime),
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
            ui.menu_button("Add Switch Button", |ui| {
                if let Some(n) = self.switch_choice(ui) {
                    bind.set(Some(n));
                    ui.close();
                }
            })
            .response
            .on_hover_text(
                "Put a button along the top of the node editor that turns a node's \
                 On/Off setting on or off, such as a reverb you want to switch while you play",
            );
        });
        if let Some(n) = bind.get() {
            actions.push(Action::BindButton(n));
        }
        if let Some(n) = template.get() {
            actions.push(Action::NewFromTemplate(n));
        }
        ui.menu_button("View", |ui| {
            if ui
                .add(
                    egui::Button::new("Command Palette…")
                        .shortcut_text(ui.ctx().format_shortcut(&PALETTE)),
                )
                .on_hover_text("Find any command by name, and set its key")
                .clicked()
            {
                actions.push(Action::OpenPalette);
                ui.close();
            }
            ui.separator();
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
                .selectable_label(self.midi_ports_open, "MIDI ports")
                .on_hover_text("Which port each MIDI In, MIDI Control and MIDI Out node uses")
                .clicked()
            {
                actions.push(Action::ToggleMidiPorts);
            }
            if ui
                .selectable_label(self.plugin_browser.is_open(), "Plugins")
                .on_hover_text("Find and add CLAP plugins")
                .clicked()
            {
                actions.push(Action::TogglePlugins);
            }
            if ui
                .selectable_label(self.cache_panel.is_open(), "Playback cache")
                .on_hover_text("Switch the playback cache, and see what it holds")
                .clicked()
            {
                actions.push(Action::ToggleCache);
            }
            if ui
                .selectable_label(self.editor.show_cpu(), "CPU meters")
                .on_hover_text("Show how much of the audio thread's time each node takes")
                .clicked()
            {
                actions.push(Action::ToggleCpu);
            }
            if ui
                .button("Display Settings…")
                .on_hover_text("Choose what the graph shows of the sound as it plays")
                .clicked()
            {
                actions.push(Action::DisplaySettings);
                ui.close();
            }
        });
    }

    /// The whole top row. Menus and the tempo group sit on the left, the
    /// transport and the position in the middle of the window, and the
    /// engine, the keyboard and the user's switches on the right.
    ///
    /// Nothing in it changes width because of what the project is doing: a
    /// status is a dot, a toggle never changes its label, and each field has
    /// a fixed width. So a button never moves while it is being pressed.
    fn top_bar(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        use egui::{Align, Layout, UiBuilder};
        let row = ui.max_rect();
        self.menus(ui, actions);
        ui.separator();
        let snap = self.tempo_group(ui, actions);
        if let Some(step) = snap {
            self.session.set_snap(step);
        }
        let left_end = ui.cursor().min.x;
        // The right group is placed by the width it had last frame, like the
        // transport below. (A right-to-left layout would be simpler, but its
        // rect spans the whole row, and sits over the menus.)
        let right_id = egui::Id::new("top bar right width");
        let right_width = ui.data(|d| d.get_temp::<f32>(right_id)).unwrap_or(150.0);
        let right_rect = egui::Rect::from_min_max(
            egui::pos2((row.max.x - right_width).max(left_end + 10.0), row.min.y),
            row.max,
        );
        let used = ui
            .scope_builder(
                UiBuilder::new()
                    .id_salt("top bar right")
                    .max_rect(right_rect)
                    .layout(Layout::left_to_right(Align::Center)),
                |ui| self.right_group(ui, actions),
            )
            .response
            .rect
            .width();
        if (used - right_width).abs() > 0.5 {
            ui.data_mut(|d| d.insert_temp(right_id, used));
            ui.request_repaint();
        }
        let right_start = right_rect.min.x;
        // The transport is centred on the window, and gives way to the groups
        // on either side when there is no room.
        let id = egui::Id::new("top bar transport width");
        let width = ui.data(|d| d.get_temp::<f32>(id)).unwrap_or(280.0);
        let low = left_end + 10.0;
        let high = (right_start - 10.0 - width).max(low);
        let x = (row.center().x - width / 2.0).clamp(low, high);
        let rect = egui::Rect::from_min_max(
            egui::pos2(x, row.min.y),
            egui::pos2(x + width + 4.0, row.max.y),
        );
        let used = ui
            .scope_builder(
                UiBuilder::new()
                    .id_salt("top bar transport")
                    .max_rect(rect)
                    .layout(Layout::left_to_right(Align::Center)),
                |ui| self.transport(ui, actions),
            )
            .response
            .rect
            .width();
        if (used - width).abs() > 0.5 {
            ui.data_mut(|d| d.insert_temp(id, used));
            ui.request_repaint();
        }
    }

    /// The capsule round the transport: rewind, play, record with its options,
    /// and the position.
    fn transport(&self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let capsule = egui::Frame::NONE
            .fill(theme::TRANSPORT_FILL)
            .stroke(egui::Stroke::new(1.0, theme::TRANSPORT_OUTLINE))
            .corner_radius(egui::CornerRadius::same(theme::PILL_RADIUS))
            .inner_margin(egui::Margin::symmetric(8, 1))
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                self.transport_buttons(ui, actions)
            });
        // The record options open centred under the whole capsule, not under
        // the small arrow, so they read as belonging to the transport.
        egui::Popup::menu(&capsule.inner)
            .anchor(egui::PopupAnchor::ParentRect(capsule.response.rect))
            .align(egui::RectAlign::BOTTOM)
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
            .show(|ui| self.record_options(ui, actions));
    }

    /// The capsule round the tempo, the metronome, the time signature, where
    /// the tempo comes from, and the snap step. Says the snap step picked.
    fn tempo_group(&self, ui: &mut egui::Ui, actions: &mut Vec<Action>) -> Option<i64> {
        let mut snap = None;
        egui::Frame::NONE
            .fill(theme::TRANSPORT_FILL)
            .stroke(egui::Stroke::new(1.0, theme::TRANSPORT_OUTLINE))
            .corner_radius(egui::CornerRadius::same(theme::PILL_RADIUS))
            .inner_margin(egui::Margin::symmetric(8, 1))
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                self.metronome_button(ui, actions);
                self.tempo_fields(ui, actions);
                self.tempo_source_controls(ui, actions);
                ui.separator();
                snap = crate::snap::menu(ui, self.session.snap());
            });
        snap
    }

    /// The right edge: the computer keyboard, the mix version and the audio
    /// engine.
    fn right_group(&self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        self.keyboard_button(ui, actions);
        self.version_menu(ui, actions);
        self.engine_chip(ui, actions);
    }

    /// Returns the record options arrow, whose click opens the popover.
    fn transport_buttons(&self, ui: &mut egui::Ui, actions: &mut Vec<Action>) -> egui::Response {
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
        let recording = self.session.is_recording() || self.session.is_counting_in();
        // The options arrow is part of the record button, not a control of
        // its own, so it is joined to it.
        let (record, options) = crate::transport_widgets::split_button(
            ui,
            "Record options",
            |ui| {
                let button = egui::Button::new(egui::RichText::new("⏺").color(if recording {
                    theme::RECORD
                } else {
                    theme::timeline::TEXT
                }))
                .selected(recording);
                ui.add(button).on_hover_text(
                "Record the input onto the armed tracks (RA and RM on a track header), from the playhead",
            )
            },
        );
        if record.clicked() {
            actions.push(Action::ToggleRecord);
        }
        if self.session.count_in_bars() > 0 || self.session.pre_roll_bars() > 0 {
            // A count-in or a pre-roll is set, which changes what record does.
            ui.painter().circle_filled(
                record.rect.right_top() + egui::vec2(-4.0, 4.5),
                2.5,
                theme::ACCENT,
            );
        }
        let options = options.on_hover_text(
            "Record options: quantise, loop recording, count-in, pre-roll and automation",
        );
        ui.separator();
        let map = self.session.project().tempo_map();
        // Seeking ends a take, so a drag that slips sideways must not be able
        // to while one is being made.
        ui.add_enabled_ui(!recording, |ui| {
            if let Some(tick) =
                crate::transport_widgets::position_field(ui, map, self.session.playhead())
            {
                actions.push(Action::Seek(tick));
            }
        });
        options
    }

    /// The tempo source menu, which also has Tap tempo, and Write to map while a tap or a
    /// clock is in charge.
    fn tempo_source_controls(&self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let source = self.session.tempo_source();
        ui.menu_button(format!("♪ {}", source.label()), |ui| {
            if ui
                .button("Tap tempo")
                .on_hover_text("Tap the tempo (T). Three taps make a tempo")
                .clicked()
            {
                actions.push(Action::TapTempo);
                ui.close();
            }
            ui.separator();
            for choice in crate::tempo_source::TempoSource::ALL {
                if ui
                    .selectable_label(choice == source, choice.label())
                    .clicked()
                {
                    actions.push(Action::SetTempoSource(choice));
                    ui.close();
                }
            }
            if self.session.live_tempo().is_some() {
                ui.separator();
                if ui
                    .button("Write to map")
                    .on_hover_text(
                        "Keep the live tempo as a tempo change in the map at the playhead",
                    )
                    .clicked()
                {
                    actions.push(Action::WriteTempoToMap);
                    ui.close();
                }
            }
        })
        .response
        .on_hover_text("Where the tempo comes from: the tempo map, taps, or a MIDI clock");
    }

    /// The session's tempo and time signature. The tempo is the one that holds
    /// at the playhead, so where the tempo lane has made changes, this edits
    /// the stretch the playhead is in.
    fn tempo_fields(&self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let map = self.session.project().tempo_map();
        let mut bpm = map.bpm_at(self.session.playhead());
        // A tap or a clock in charge: its tempo shows in place of the map's
        // field, which would edit a tempo nothing is playing.
        let response = if let Some(live) = self.session.live_tempo() {
            ui.add_sized(
                [BPM_WIDTH, ui.spacing().interact_size.y],
                egui::Label::new(format!("{live:.1} bpm")).sense(egui::Sense::hover()),
            )
            .on_hover_text("The live tempo, from taps or the MIDI clock")
        } else {
            // A fixed width: typing a tempo drops the " bpm" suffix, and a field
            // that shrank then would move every button after it.
            ui.add_sized(
                [BPM_WIDTH, ui.spacing().interact_size.y],
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
            })
        };
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
            .arrow(false)
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

    /// The record popover: two columns. On the left, how recorded MIDI is
    /// snapped (what is heard while recording is never snapped, only the clip
    /// the take becomes). On the right, the count-in (bars of metronome before
    /// a take), the pre-roll (bars of the project before a punch-in), what a
    /// loop does, and how armed Automation nodes write.
    fn record_options(&self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        ui.horizontal_top(|ui| {
            ui.vertical(|ui| {
                ui.label(egui::RichText::new("Quantise").weak());
                let current = self.session.record_quantise();
                for choice in crate::midi_take::Quantise::ALL {
                    if ui
                        .selectable_label(choice == current, choice.label())
                        .on_hover_text("Snap the notes of recorded MIDI to this grid")
                        .clicked()
                    {
                        actions.push(Action::SetRecordQuantise(choice));
                    }
                }
            });
            ui.separator();
            ui.vertical(|ui| {
                egui::Grid::new("record options")
                    .num_columns(3)
                    .spacing([10.0, 8.0])
                    .show(ui, |ui| {
                        ui.label("Count-in");
                        let choices: Vec<_> = [0u8, 1, 2]
                            .iter()
                            .map(|n| {
                                (
                                    n.to_string(),
                                    *n,
                                    "Bars of metronome before recording starts",
                                )
                            })
                            .collect();
                        if let Some(n) = crate::transport_widgets::inline(
                            ui,
                            &choices,
                            self.session.count_in_bars(),
                        ) {
                            actions.push(Action::SetCountIn(n));
                        }
                        ui.weak("bars");
                        ui.end_row();

                        ui.label("Pre-roll");
                        let choices: Vec<_> = [0u8, 1, 2, 4]
                            .iter()
                            .map(|n| {
                                (
                                    n.to_string(),
                                    *n,
                                    "Bars of the project played before a punch-in",
                                )
                            })
                            .collect();
                        if let Some(n) = crate::transport_widgets::inline(
                            ui,
                            &choices,
                            self.session.pre_roll_bars(),
                        ) {
                            actions.push(Action::SetPreRoll(n));
                        }
                        ui.weak("bars");
                        ui.end_row();

                        ui.label("Loop MIDI");
                        let mode = self.session.record_midi();
                        let choices: Vec<_> = crate::midi_take::MidiRecord::ALL
                            .iter()
                            .map(|m| {
                                let tip = match m {
                                    crate::midi_take::MidiRecord::Takes => {
                                        "A take of every cycle, in a take folder to comp from"
                                    }
                                    crate::midi_take::MidiRecord::Merge => {
                                        "Every cycle in one clip"
                                    }
                                };
                                (m.label().to_owned(), *m, tip)
                            })
                            .collect();
                        if let Some(choice) = crate::transport_widgets::inline(ui, &choices, mode) {
                            actions.push(Action::SetRecordMidi(choice));
                        }
                        ui.label("");
                        ui.end_row();

                        ui.label("Automation");
                        let current = self.session.automation_mode();
                        let choices: Vec<_> =
                            crate::session::record_automation::AutomationMode::ALL
                                .iter()
                                .map(|m| (m.label().to_owned(), *m, m.tooltip()))
                                .collect();
                        if let Some(choice) =
                            crate::transport_widgets::inline(ui, &choices, current)
                        {
                            actions.push(Action::SetAutomationMode(choice));
                        }
                        ui.label("");
                        ui.end_row();
                    });
            });
        });
    }

    /// Which mix version plays. Shown once the project has versions (the
    /// first comes from "Vary per version" on a parameter); switching needs
    /// no rebuild, so it can be done while playing.
    fn version_menu(&self, ui: &mut egui::Ui, _actions: &mut Vec<Action>) {
        let edits = &self.version_edits;
        let project = self.session.project();
        let versions = project.versions();
        if versions.is_empty() {
            return;
        }
        let active = project.active_version().min(versions.len() - 1);
        // Stays open while a name is typed into it.
        let config = egui::containers::menu::MenuConfig::new()
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside);
        egui::containers::menu::MenuButton::new(format!("Ver {}", versions[active]))
            .config(config)
            .ui(ui, |ui| {
                for (i, name) in versions.iter().enumerate() {
                    if ui.selectable_label(i == active, name).clicked()
                        && let Ok(command) = project.select_version_command(i)
                    {
                        edits.borrow_mut().push(command);
                    }
                }
                ui.separator();
                let id = egui::Id::new("rename version");
                let mut text: String = ui
                    .data(|d| d.get_temp::<(usize, String)>(id))
                    .filter(|(i, _)| *i == active)
                    .map_or_else(|| versions[active].clone(), |(_, t)| t);
                let edit = ui.text_edit_singleline(&mut text);
                if edit.lost_focus()
                    && !text.trim().is_empty()
                    && text != versions[active]
                    && let Ok(command) = project.rename_version_command(active, text.trim())
                {
                    edits.borrow_mut().push(command);
                }
                ui.data_mut(|d| d.insert_temp(id, (active, text)));
                if ui
                    .button("Add version")
                    .on_hover_text("Every Version Value gets a slot holding this version's value")
                    .clicked()
                    && let Ok(command) =
                        project.add_version_command(&format!("Version {}", versions.len() + 1))
                {
                    edits.borrow_mut().push(command);
                }
                if ui
                    .add_enabled(versions.len() > 1, egui::Button::new("Remove this version"))
                    .clicked()
                    && let Ok(command) = project.remove_version_command(active)
                {
                    edits.borrow_mut().push(command);
                }
            })
            .0
            .on_hover_text("The mix version that plays");
    }

    /// The audio engine's switch with its options arrow. Lit while the engine
    /// runs; the dot says how the sound is being made, so the chip never
    /// changes size: grey is live, a ring is caching, lime is played from the
    /// cache, amber is the engine running on something other than what was
    /// chosen (the hover says what).
    fn engine_chip(&self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let on = self.session.engine_running();
        let warning = self.session.engine_warning();
        let status = self.session.cache_status();
        let colour = match (on, warning) {
            (true, Some(_)) => theme::WARNING,
            (true, None) => theme::ACCENT,
            (false, _) => theme::timeline::TEXT_WEAK,
        };
        let mut tip = match (on, warning) {
            (true, Some(warning)) => format!(
                "The audio engine is on, but not as chosen: {warning}. Click to turn it off"
            ),
            (true, None) => format!(
                "The audio engine is on: the graph sounds, and live MIDI plays \
                 through the armed tracks, even with the timeline stopped.{} Click to turn it off",
                self.session
                    .latency_report()
                    .map_or(String::new(), |r| format!(
                        " {}.",
                        crate::devices::latency_line(&r)
                    )),
            ),
            (false, _) => "The audio engine is off, so nothing sounds. Click to turn it on".into(),
        };
        if on && !matches!(status, CacheStatus::Off | CacheStatus::Idle) {
            tip.push_str(&format!(". {}", crate::cache_panel::status_line(status).0));
        }
        if let Some(n) = self.session.underruns().filter(|&n| n > 0) {
            let times = if n == 1 { "underrun" } else { "underruns" };
            tip.push_str(&format!(". {n} {times} since playback started"));
        }
        let (chip, options) = crate::transport_widgets::split_button(ui, "Audio options", |ui| {
            // Room on the left for the dot.
            ui.scope(|ui| {
                ui.spacing_mut().button_padding.x = 17.0;
                let label = egui::RichText::new("Audio").color(colour);
                ui.add(egui::Button::new(label).selected(on))
                    .on_hover_text(tip)
            })
            .inner
        });
        if chip.clicked() {
            actions.push(Action::ToggleEngine);
        }
        let centre = chip.rect.left_center() + egui::vec2(10.0, 0.0);
        let (colour, filled) = self.engine_dot();
        if filled {
            ui.painter().circle_filled(centre, 3.5, colour);
        } else {
            ui.painter()
                .circle_stroke(centre, 3.0, egui::Stroke::new(1.5, colour));
        }
        egui::Popup::menu(&options)
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
            .show(|ui| self.audio_options(ui, actions));
        options.on_hover_text("Where monitoring plays, the playback cache, and audio settings");
    }

    /// The colour of the Audio chip's dot, and whether it is filled: grey
    /// live, a ring while caching, lime from the cache, amber when the engine
    /// runs on something other than what was chosen, a grey ring when off.
    fn engine_dot(&self) -> (egui::Color32, bool) {
        let weak = theme::timeline::TEXT_WEAK;
        match (
            self.session.engine_running(),
            self.session.engine_warning(),
            self.session.cache_status(),
        ) {
            (false, _, _) => (weak, false),
            (true, Some(_), _) => (theme::WARNING, true),
            (true, None, CacheStatus::Cached) => (theme::ACCENT, true),
            (true, None, CacheStatus::Rendering(_)) => (theme::ACCENT, false),
            (true, None, _) => (weak, true),
        }
    }

    /// The audio chip's popover, in three parts: the engine and whether it
    /// runs as chosen, where the monitor buttons on nodes play, and the
    /// playback cache (what it is, whether it is on, and where the sound is
    /// coming from now).
    fn audio_options(&self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        use crate::transport_widgets::dot_label;
        let weak = theme::timeline::TEXT_WEAK;
        let heading = |text: &str| egui::RichText::new(text).strong();
        ui.set_width(270.0);

        // The engine.
        let on = self.session.engine_running();
        ui.horizontal(|ui| {
            ui.label(heading("Audio engine"));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let choices = [
                    ("On", true, "Turn the engine on"),
                    ("Off", false, "Turn the engine off: nothing sounds"),
                ];
                let choices: Vec<_> = choices
                    .iter()
                    .map(|(l, v, t)| (l.to_string(), *v, *t))
                    .collect();
                if crate::transport_widgets::inline(ui, &choices, on).is_some_and(|v| v != on) {
                    actions.push(Action::ToggleEngine);
                }
            });
        });
        match (on, self.session.engine_warning()) {
            (true, Some(warning)) => dot_label(
                ui,
                theme::WARNING,
                true,
                egui::RichText::new(format!("Not as chosen: {warning}")).color(theme::WARNING),
            ),
            (true, None) => dot_label(ui, theme::ACCENT, true, "Running"),
            (false, _) => dot_label(ui, weak, false, "Off, so nothing sounds"),
        }
        if let Some(report) = self.session.latency_report() {
            ui.label(egui::RichText::new(crate::devices::latency_line(&report)).color(weak));
        }
        if let Some(n) = self.session.underruns().filter(|&n| n > 0) {
            let times = if n == 1 { "underrun" } else { "underruns" };
            ui.colored_label(
                theme::WARNING,
                format!("{n} {times} since playback started"),
            );
        }

        // Monitoring.
        ui.separator();
        ui.label(heading("Monitor plays on")).on_hover_text(
            "Where a node's monitor button plays. Click it to listen to a node, \
                 Shift-click to add another, Ctrl-click to silence the other outputs on \
                 this device, Ctrl+Shift-click to silence all of them",
        );
        let current = self.session.monitor_device();
        if ui.radio(current.is_empty(), "Main output").clicked() {
            actions.push(Action::SetMonitorDevice(None));
        }
        for (n, device) in self.outputs.devices().iter().enumerate() {
            if ui.radio(current == device.id, &device.name).clicked() {
                actions.push(Action::SetMonitorDevice(Some(n)));
            }
        }
        if !self.session.monitored().is_empty()
            && ui
                .button(
                    egui::RichText::new(match self.session.monitor_mode() {
                        crate::session::MonitorMode::Sum => "Stop monitor",
                        crate::session::MonitorMode::Exclusive => "Stop monitor (solo device)",
                        crate::session::MonitorMode::SilenceOthers => "Stop monitor (solo)",
                    })
                    .color(theme::ACCENT),
                )
                .on_hover_text("Stop listening to the monitored nodes (Escape)")
                .clicked()
        {
            actions.push(Action::StopMonitor);
        }

        // The cache.
        ui.separator();
        ui.label(heading("Playback cache"));
        ui.label(
            egui::RichText::new(
                "Renders groups and tracks in the background, so playing them again \
                 plays the saved audio instead of computing it.",
            )
            .color(weak)
            .small(),
        );
        let mut auto = self.session.auto_cache();
        if ui
            .checkbox(&mut auto, "Cache automatically")
            .on_hover_text("Off plays everything live. Frozen nodes are not affected")
            .changed()
        {
            actions.push(Action::SetAutoCache(auto));
        }
        let status = self.session.cache_status();
        let (line, _) = crate::cache_panel::status_line(status);
        let (colour, filled) = self.engine_dot();
        dot_label(ui, colour, filled, line);

        ui.separator();
        ui.horizontal(|ui| {
            if ui
                .button("Cache details…")
                .on_hover_text("What the cache holds, and how much room it has left")
                .clicked()
            {
                actions.push(Action::ToggleCache);
                ui.close();
            }
            if ui.button("Audio settings…").clicked() {
                actions.push(Action::AudioSettings);
                ui.close();
            }
        });
    }

    /// The computer keyboard as a MIDI source, with an arrow for its octave and
    /// velocity. The button never changes its label.
    fn keyboard_button(&self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let on = self.keyboard.is_on();
        let (button, options) =
            crate::transport_widgets::split_button(ui, "Keyboard options", |ui| {
                crate::transport_widgets::keyboard_toggle(ui, on).on_hover_text(format!(
                    "Play the MIDI In nodes, and record, from the computer keyboard (M). \
                     A W S E D F T G Y H U J K are the notes, Z and X change the octave, \
                     C and V the velocity. Now {} at velocity {}",
                    self.keyboard.octave_label(),
                    self.keyboard.velocity()
                ))
            });
        if button.clicked() {
            actions.push(Action::ToggleKeyboard);
        }
        egui::Popup::menu(&options)
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
            .show(|ui| {
                ui.set_min_width(200.0);
                ui.label(egui::RichText::new("Computer keyboard").strong());
                let mut octave = self.keyboard.octave();
                ui.horizontal(|ui| {
                    ui.label("Octave");
                    if ui
                        .add(
                            egui::DragValue::new(&mut octave)
                                .range(crate::keyboard::OCTAVES)
                                .prefix("C"),
                        )
                        .on_hover_text("Z and X")
                        .changed()
                    {
                        actions.push(Action::SetKeyboardOctave(octave));
                    }
                });
                let mut velocity = self.keyboard.velocity();
                ui.horizontal(|ui| {
                    ui.label("Velocity");
                    if ui
                        .add(egui::DragValue::new(&mut velocity).range(1..=127))
                        .on_hover_text("C and V")
                        .changed()
                    {
                        actions.push(Action::SetKeyboardVelocity(velocity));
                    }
                });
            });
        options.on_hover_text("The keyboard's octave and velocity");
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
                "Metronome: click to turn the click on or off".to_owned(),
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

    /// The strip along the top of the node editor: the user's own switches,
    /// each a Button node lit while on and named for what it switches, and a
    /// menu to add one wired to the On/Off setting of any node. The Button
    /// nodes are ordinary nodes in the graph; this is only a place to press
    /// them. It wraps onto more rows when there are many, and is only there
    /// while there is a switch to show (Edit > Add Switch Button makes the first).
    fn switch_strip(&self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        // No strip, and no height taken from the graph, until there is a switch.
        if pill_buttons::extras(self.session.project(), self.session.registry()).is_empty() {
            return;
        }
        egui::Frame::NONE
            .fill(theme::PANEL)
            .inner_margin(egui::Margin::symmetric(8, 3))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal_wrapped(|ui| {
                    ui.label(egui::RichText::new("Switches").weak())
                        .on_hover_text(
                            "Buttons for the On/Off settings you want in reach while you play",
                        );
                    self.extra_buttons(ui, actions);
                });
            });
    }

    /// The switches in the strip, then its menu.
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
                format!("Switches {}. Click to turn it on or off", extra.target)
            };
            // The node it switches, so it can be told from the others.
            let name = extra
                .target
                .split(" · ")
                .next()
                .filter(|name| !name.is_empty())
                .unwrap_or("Switch");
            let button =
                egui::Button::new(egui::RichText::new(name).color(colour)).selected(extra.on);
            if ui.add(button).on_hover_text(tip).clicked() {
                actions.push(Action::PressButton(extra.button));
            }
        }
        ui.menu_button("+ Switch", |ui| {
            if let Some(n) = self.switch_choice(ui) {
                actions.push(Action::BindButton(n));
                ui.close();
            }
        })
        .response
        .on_hover_text(
            "Add a button here that turns a node's On/Off setting on or off, \
             such as a reverb you want to switch while you play",
        );
    }

    /// The list of On/Off settings a switch can be added for. Says which was
    /// picked, as its place in [`pill_buttons::switches`].
    fn switch_choice(&self, ui: &mut egui::Ui) -> Option<usize> {
        let switches = pill_buttons::switches(self.session.project(), self.session.registry());
        if switches.is_empty() {
            ui.weak("No On/Off settings to add a switch for");
        }
        let mut picked = None;
        for (n, switch) in switches.iter().enumerate() {
            if ui.button(&switch.label).clicked() {
                picked = Some(n);
            }
        }
        picked
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let problems = self.session.diagnostics().len();
            let label = match problems {
                0 => egui::Label::new(egui::RichText::new("No problems").weak()),
                1 => egui::Label::new(
                    egui::RichText::new("1 problem").color(ui.visuals().warn_fg_color),
                ),
                n => egui::Label::new(
                    egui::RichText::new(format!("{n} problems")).color(ui.visuals().warn_fg_color),
                ),
            };
            let response = if problems == 0 {
                ui.add(label)
            } else {
                ui.add(label.sense(egui::Sense::click()))
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .on_hover_text("Click to list the problems")
            };
            if problems > 0 {
                // Clicking the status lists the problems; picking one shows
                // the node it is about.
                let mut reveal = None;
                egui::Popup::menu(&response).show(|ui| {
                    for diagnostic in self.session.diagnostics() {
                        let node = match &diagnostic.location {
                            noodle_engine::Location::Node(id) => *id,
                            noodle_engine::Location::Wire(endpoint) => endpoint.node,
                        };
                        if ui.button(diagnostic.to_string()).clicked() {
                            reveal = Some(node);
                            ui.close();
                        }
                    }
                });
                if let Some(node) = reveal {
                    self.editor.reveal_top_level(node);
                }
            }
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
            if let Some((fraction, name)) = self.session.render_progress() {
                ui.separator();
                ui.label(format!("Rendering {name} to a clip"));
                ui.add(
                    egui::ProgressBar::new(fraction)
                        .desired_width(120.0)
                        .show_percentage(),
                );
                if ui.small_button("Cancel").clicked() {
                    self.session.cancel_render();
                }
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(100));
            }
            if let Some(message) = self.session.message() {
                ui.separator();
                ui.label(message);
            }
        });
    }

    /// Offers the autosave a crashed run left.
    fn recovery_dialog(&mut self, ctx: &egui::Context) {
        let Some(recovery) = self.recovery.clone() else {
            return;
        };
        let mut recover = false;
        let mut discard = false;
        egui::Modal::new(egui::Id::new("recovery")).show(ctx, |ui| {
            ui.label("Noodle didn't close properly last time.");
            let when = recovery
                .age()
                .map(|a| format!(", saved {a}"))
                .unwrap_or_default();
            ui.label(format!(
                "Recover the unsaved work on {}{when}?",
                recovery.name()
            ));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                recover = ui.button("Recover").clicked();
                discard = ui.button("Discard").clicked();
            });
        });
        if recover {
            let loaded = self
                .session
                .recover(&recovery.text, recovery.origin.clone());
            if loaded {
                self.editor = EditorState::default();
            }
            // A copy that won't load is kept aside, to look at by hand.
            self.finish_recovery(&recovery, !loaded);
        } else if discard {
            self.finish_recovery(&recovery, false);
        }
    }

    fn finish_recovery(&mut self, recovery: &crate::autosave::Recovery, keep: bool) {
        self.recovery = None;
        if let Some(autosave) = &mut self.autosave {
            if keep {
                autosave.set_aside(recovery);
            } else {
                autosave.remove(recovery);
            }
        }
    }

    /// Writes the autosave when it is due, and removes it once the project is
    /// saved.
    fn autosave_tick(&mut self, ctx: &egui::Context) {
        let Some(autosave) = &mut self.autosave else {
            return;
        };
        if self.recovery.is_some() {
            return;
        }
        let session = &self.session;
        autosave.tick(
            std::time::Instant::now(),
            session.is_dirty(),
            session.history_position(),
            || (session.project_ron(), session.path().map(Path::to_owned)),
        );
        if session.is_dirty() {
            // Wake up for the next write even if nothing else moves.
            ctx.request_repaint_after(crate::autosave::INTERVAL);
        }
    }

    /// Asks what to do with unsaved changes before an action that would
    /// lose them.
    /// The command palette, and the box that names a template being saved.
    fn palette_and_templates(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        if self.palette.is_open() || self.template_name.is_some() {
            self.templates = self.session.templates();
        }
        let from_template: Vec<(String, Action)> = self
            .templates
            .iter()
            .enumerate()
            .map(|(n, (name, _))| {
                (
                    format!("New from Template: {name}"),
                    Action::NewFromTemplate(n),
                )
            })
            .collect();
        let out = self.palette.show(ctx, &mut self.keymap, &from_template);
        actions.extend(out.run);
        if out.keymap_changed {
            crate::prefs::remember_keymap(self.keymap.saved());
        }
        let Some(name) = &mut self.template_name else {
            return;
        };
        let mut done = None;
        let modal = egui::Modal::new(egui::Id::new("template name")).show(ctx, |ui| {
            ui.label("Name the project template");
            let field = ui.add(egui::TextEdit::singleline(name).hint_text("Template name"));
            // Read Enter before asking for the focus back: asking again
            // would cancel the focus loss Enter causes.
            let enter = field.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
            if !field.has_focus() && !enter {
                field.request_focus();
            }
            ui.horizontal(|ui| {
                if (ui.button("Save").clicked() || enter) && !name.trim().is_empty() {
                    done = Some(true);
                }
                if ui.button("Cancel").clicked() {
                    done = Some(false);
                }
            });
        });
        if modal.should_close() {
            done = Some(false);
        }
        match done {
            Some(true) => {
                let name = self.template_name.take().unwrap_or_default();
                self.session.save_template(&name);
                self.templates = self.session.templates();
            }
            Some(false) => self.template_name = None,
            None => {}
        }
    }

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
                if let Some(path) = crate::picker::pick_file(("Noodle project", &["ron"]))
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
                if let Some(path) =
                    crate::picker::save_file(&name, ("Noodle project", &["ron"]), None)
                    && self.session.save_as(&with_extension(path))
                    && let Some(then) = then
                {
                    self.run(ctx, then);
                }
            }
            Action::OpenPalette => self.palette.open(),
            Action::Guide(text) => self.session.notify(text.to_owned()),
            Action::TogglePunch => {
                let punch = self.session.project().punch();
                self.session.edit(vec![Edit::Apply(Command::SetPunch(
                    noodle_core::PunchRange {
                        on: !punch.on,
                        ..punch
                    },
                ))]);
            }
            Action::SetPunchIn | Action::SetPunchOut => {
                let punch = self.session.project().punch();
                let here = self.session.playhead();
                let map = self.session.project().tempo_map().clone();
                let moved = if action == Action::SetPunchIn {
                    timeline::punch_in_at(punch, here, &map)
                } else {
                    timeline::punch_out_at(punch, here, &map)
                };
                self.session
                    .edit(vec![Edit::Apply(Command::SetPunch(moved))]);
            }
            Action::AddMarker => {
                let here = self.session.playhead();
                if let Some(command) = timeline::add_marker_at(self.session.project(), here) {
                    self.session.edit(vec![Edit::Apply(command)]);
                }
            }
            Action::SaveTemplate => self.template_name = Some(String::new()),
            Action::NewFromTemplate(n) => {
                if let Some((_, path)) = self.templates.get(n).cloned()
                    && self.session.new_from_template(&path)
                {
                    self.editor = EditorState::default();
                }
            }
            Action::FoldFolders(folded) => {
                let organisation = self
                    .session
                    .project()
                    .organisation()
                    .clone()
                    .with_all_folded(folded);
                self.session
                    .edit([Edit::Apply(Command::SetOrganisation(organisation))]);
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
            Action::TapTempo => self.session.tap_tempo(std::time::Instant::now()),
            Action::SetTempoSource(source) => self.session.set_tempo_source(source),
            Action::WriteTempoToMap => self.session.write_tempo_to_map(),
            Action::ToggleEngine => {
                if self.session.engine_running() {
                    self.session.stop_engine();
                } else {
                    self.session.start_engine();
                }
            }
            Action::StopMonitor => self.session.stop_monitor(),
            Action::StopLearningMidi => self.session.edit([Edit::CancelLearnMidi]),
            Action::SetMonitorDevice(choice) => {
                let device = choice
                    .and_then(|n| self.outputs.devices().get(n))
                    .map_or_else(String::new, |d| d.id.clone());
                self.session.set_monitor_device(&device);
                crate::prefs::remember_monitor_device(&device);
            }
            Action::Rewind => self.session.rewind(),
            Action::Seek(tick) => self.session.seek(tick),
            Action::SetRecordQuantise(quantise) => self.session.set_record_quantise(quantise),
            Action::SetRecordMidi(mode) => {
                self.session.set_record_midi(mode);
                crate::prefs::remember_midi_record(mode);
            }
            Action::SetCountIn(bars) => {
                self.session.set_count_in_bars(bars);
                crate::prefs::remember_recording(
                    self.session.count_in_bars(),
                    self.session.pre_roll_bars(),
                );
            }
            Action::SetPreRoll(bars) => {
                self.session.set_pre_roll_bars(bars);
                crate::prefs::remember_recording(
                    self.session.count_in_bars(),
                    self.session.pre_roll_bars(),
                );
            }
            Action::SetAutomationMode(mode) => {
                self.session.set_automation_mode(mode);
                crate::prefs::remember_automation_mode(mode);
            }
            Action::SetKeyboardOctave(octave) => self.keyboard.set_octave(octave),
            Action::SetKeyboardVelocity(velocity) => self.keyboard.set_velocity(velocity),
            Action::SetAutoCache(on) => {
                self.session.set_auto_cache(on);
                crate::prefs::remember_auto_cache(on);
            }
            Action::ToggleKeyboard => {
                for message in self.keyboard.set_on(!self.keyboard.is_on()) {
                    self.session.midi_bus().send(message);
                }
            }
            Action::ToggleRecord => {
                if self.session.is_recording() {
                    self.session.stop_recording();
                } else if self.session.is_counting_in() {
                    self.session.cancel_count_in();
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
            Action::ToggleCache => self.cache_panel.toggle(),
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
            Action::DisplaySettings => self.display_dialog.open(),
            Action::ToggleOutputs => {
                self.outputs_open = !self.outputs_open;
                if self.outputs_open {
                    self.outputs
                        .refresh(self.session.audio_config().host.as_deref());
                }
            }
            Action::ToggleMidiPorts => {
                self.midi_ports_open = !self.midi_ports_open;
                if self.midi_ports_open {
                    self.midi_ports.refresh();
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
            Action::ExportAudio => self.export.open(&mut self.session),
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
            Action::Clips(action) => match self.focus {
                Focus::Arrangement => self.timeline.request(action),
                Focus::Notes => self.piano_roll.request(action),
                Focus::Graph => self.editor.request_clip(action),
            },
            Action::AddMixVersion => {
                let project = self.session.project();
                let command = if project.versions().is_empty() {
                    Ok(Command::SetVersions {
                        names: crate::session::versions::STARTING_VERSIONS
                            .iter()
                            .map(|s| (*s).to_owned())
                            .collect(),
                        active: 0,
                    })
                } else {
                    project
                        .add_version_command(&format!("Version {}", project.versions().len() + 1))
                };
                match command {
                    Ok(command) => self.session.edit([crate::session::Edit::Apply(command)]),
                    Err(error) => self.session.notify(error.to_string()),
                }
            }
            Action::OnSelectedClip(what) => {
                match self.timeline.selected_audio_clip(self.session.project()) {
                    Some(clip) => self.session.edit([match what {
                        SelectedClipAction::SplitToStems => {
                            crate::session::Edit::SplitToStems(clip)
                        }
                        SelectedClipAction::SliceToSampler => {
                            crate::session::Edit::SliceToSampler(clip)
                        }
                        SelectedClipAction::PinTransients => {
                            crate::session::Edit::PinTransients(clip, 50)
                        }
                        SelectedClipAction::ConvertToMidi(detect) => {
                            crate::session::Edit::ConvertToMidi(clip, detect, None)
                        }
                    }]),
                    None => self
                        .session
                        .notify("Select an audio clip in the arrangement first".to_string()),
                }
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

    /// Makes the view that was just clicked in the one the Edit menu acts on.
    /// A click on the menu itself, which floats over the views, doesn't count.
    fn track_focus(
        &mut self,
        ctx: &egui::Context,
        arrangement: egui::Rect,
        roll: Option<egui::Rect>,
        graph: egui::Rect,
    ) {
        let Some(at) = ctx.input(|i| i.pointer.press_origin().filter(|_| i.pointer.any_pressed()))
        else {
            return;
        };
        if ctx
            .layer_id_at(at)
            .is_some_and(|layer| layer.order != egui::Order::Background)
        {
            return;
        }
        if arrangement.contains(at) {
            self.focus = Focus::Arrangement;
        } else if roll.is_some_and(|r| r.contains(at)) {
            self.focus = Focus::Notes;
        } else if graph.contains(at) {
            self.focus = Focus::Graph;
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
    fn on_exit(&mut self) {
        // A clean exit leaves nothing to recover. A crash never gets here.
        // Only this run's files go; an unanswered recovery stays for next time.
        if let Some(autosave) = &mut self.autosave {
            autosave.remove_own();
        }
    }

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
        // One file is a file to save; several go in a folder.
        let path = if choice.groups.is_empty() {
            let name = format!("{}.{}", self.session.name(), choice.format.extension());
            let dir = self.session.directory().map(std::path::Path::to_path_buf);
            let Some(path) = crate::picker::save_file(
                &name,
                (choice.format.label(), &[choice.format.extension()]),
                dir.as_deref(),
            ) else {
                self.export.declined();
                return;
            };
            // Some platforms' dialogs don't add the extension.
            if path.extension().is_none() {
                path.with_extension(choice.format.extension())
            } else {
                path
            }
        } else {
            let dir = self.session.directory().map(std::path::Path::to_path_buf);
            let Some(path) =
                crate::picker::pick_folder_in("Folder for the exported files", dir.as_deref())
            else {
                self.export.declined();
                return;
            };
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

    /// The transients of the clips linked to the open one (see
    /// [`clip_editor::linked_lanes`]).
    fn linked_candidates(
        &mut self,
        ctx: &egui::Context,
        subject: clip_editor::subject::Subject,
    ) -> Option<(egui::Color32, Vec<clip_editor::Lane>)> {
        let clip_editor::subject::Subject::Clip(open) = subject else {
            return None;
        };
        let project = self.session.project().clone();
        clip_editor::linked_lanes(&project, open, |member| self.clip_source(ctx, member))
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

    /// Files or a folder dropped from the desktop on the canvas: on a Sample
    /// Set they are added to it (a folder fills it and is remembered for
    /// Rescan); on the empty canvas they become a new Sample Set.
    fn files_dropped_on_graph(&mut self, dropped: editor::DroppedFiles) {
        use noodle_core::{Command, Node};
        // One audio file on a pad (or a Sampler) is that drum's sample.
        if let Some(sampler) = dropped.pad
            && let [path] = dropped.paths.as_slice()
            && path.is_file()
        {
            match self.session.samples().import(path) {
                Ok(hash) => {
                    let command = crate::sample_drop::choose(&self.session, sampler, &hash);
                    self.session.edit([crate::session::Edit::Apply(command)]);
                }
                Err(e) => self.session.notify(format!("Couldn't read that file: {e}")),
            }
            return;
        }
        let (node, new) = match dropped.on {
            Ok(node) => (node, None),
            Err(at) => {
                let id = self.session.new_node_id();
                let mut node = Node::new(noodle_nodes::SAMPLE_SET_ID).at(at.x, at.y);
                node.parent = dropped.group;
                (id, Some(Command::AddNode { id, node }))
            }
        };
        let (folders, files): (Vec<_>, Vec<_>) =
            dropped.paths.iter().cloned().partition(|p| p.is_dir());
        // The set has to exist before the session can fill it.
        if let Some(add) = new {
            self.session.edit([crate::session::Edit::Apply(add)]);
        }
        let mut problem = None;
        if let Some(folder) = folders.first() {
            match self.session.sample_set_load_folder(node, folder) {
                Ok(edits) => self.session.edit(edits),
                Err(error) => problem = Some(error),
            }
        }
        if !files.is_empty() && problem.is_none() {
            match self.session.sample_set_add(node, &files) {
                Ok(edits) => self.session.edit(edits),
                Err(error) => problem = Some(error),
            }
        }
        if let Some(error) = problem {
            self.session.notify(error);
        }
        self.editor.select_only([node]);
    }

    fn import_audio(&mut self, target: timeline::Target) {
        let picked =
            crate::picker::pick_files(("Audio", &["wav", "flac", "mp3", "ogg", "m4a", "aac"]));
        let Some(files) = picked else {
            return;
        };
        let added = timeline::import::import(&self.session, &files, target.track, target.at);
        if let Some(command) = added.command {
            // The clip appearing is feedback enough, and an earlier notice
            // ("Add a track before importing audio") is now out of date.
            self.session.clear_message();
            self.session.edit([crate::session::Edit::Apply(command)]);
        }
        if let Some(notice) = added.notice {
            self.session.notify(notice);
        }
    }
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

    fn folded_app() -> App {
        let mut app = empty();
        for n in 1..=2 {
            app.session.edit([Edit::Apply(Command::AddNode {
                id: noodle_core::NodeId(n),
                node: Node::new(crate::timeline::TRACK_INPUT),
            })]);
        }
        let org = noodle_core::Organisation::default()
            .with_new_folder("Drums", &[noodle_core::NodeId(1), noodle_core::NodeId(2)]);
        app.session
            .edit([Edit::Apply(Command::SetOrganisation(org))]);
        app
    }

    fn folded(h: &Harness<'static, App>) -> bool {
        h.state()
            .session
            .project()
            .organisation()
            .folders
            .values()
            .all(|f| f.folded)
    }

    #[test]
    fn the_palette_finds_a_command_by_what_it_does_and_runs_it() {
        let mut h = harness(folded_app());
        h.run();
        let at = egui::pos2(300.0, 200.0);
        crate::testkit::press(&mut h, at, Modifiers::COMMAND, Key::K);
        h.run();
        assert!(h.state().palette.is_open());
        crate::testkit::type_text(&mut h, at, "fold all");
        h.run();
        crate::testkit::press(&mut h, at, Modifiers::NONE, Key::Enter);
        h.run();
        assert!(!h.state().palette.is_open(), "it closes after running one");
        assert!(folded(&h));
    }

    fn palette_run(app: App, query: &str) -> Harness<'static, App> {
        let mut h = harness(app);
        h.run();
        let at = egui::pos2(300.0, 200.0);
        crate::testkit::press(&mut h, at, Modifiers::COMMAND, Key::K);
        h.run();
        crate::testkit::type_text(&mut h, at, query);
        h.run();
        crate::testkit::press(&mut h, at, Modifiers::NONE, Key::Enter);
        h.run();
        h
    }

    #[test]
    fn the_palette_turns_punch_on_and_puts_it_at_the_playhead() {
        let h = palette_run(empty(), "punch on");
        assert!(h.state().session.project().punch().on);
        let h = palette_run(empty(), "punch out");
        let punch = h.state().session.project().punch();
        assert!(punch.end > punch.start, "{punch:?}");
    }

    #[test]
    fn the_palette_adds_a_marker_at_the_playhead() {
        let h = palette_run(empty(), "add marker");
        assert_eq!(h.state().session.project().markers().len(), 1);
    }

    #[test]
    fn the_palette_finds_a_feature_by_another_programs_name_and_says_where_it_is() {
        for (query, word) in [
            ("freeze", "Shift+F"),
            ("bounce", "Render to clip"),
            ("comp", "take folder"),
            ("lufs", ""),
        ] {
            let found = commands::search(query);
            assert!(!found.is_empty(), "{query} finds nothing");
            if let Action::Guide(text) = found[0].action {
                assert!(text.contains(word), "{query}: {text}");
            }
        }
        let h = palette_run(empty(), "freeze");
        assert!(
            h.state()
                .session
                .message()
                .is_some_and(|m| m.contains("Freeze")),
            "{:?}",
            h.state().session.message()
        );
    }

    #[test]
    fn escape_closes_the_palette_without_running_anything() {
        let mut h = harness(folded_app());
        h.run();
        let at = egui::pos2(300.0, 200.0);
        crate::testkit::press(&mut h, at, Modifiers::COMMAND, Key::K);
        h.run();
        crate::testkit::press(&mut h, at, Modifiers::NONE, Key::Escape);
        h.run();
        assert!(!h.state().palette.is_open());
        assert!(!folded(&h));
    }

    #[test]
    fn a_key_the_user_binds_runs_its_command_and_the_old_key_stops() {
        let mut app = empty();
        app.keymap.bind(
            "view.mixer",
            Some(KeyboardShortcut::new(Modifiers::COMMAND, Key::M)),
        );
        // Take Save's key for another command: Save no longer answers to it.
        app.keymap.bind(
            "view.scope",
            Some(KeyboardShortcut::new(Modifiers::COMMAND, Key::S)),
        );
        let mut h = harness(app);
        h.run();
        let at = egui::pos2(300.0, 200.0);
        assert!(!h.state().mixer_open);
        crate::testkit::press(&mut h, at, Modifiers::COMMAND, Key::M);
        h.run();
        assert!(h.state().mixer_open);
        assert!(!h.state().scope_open);
        crate::testkit::press(&mut h, at, Modifiers::COMMAND, Key::S);
        h.run();
        assert!(h.state().scope_open, "Cmd+S now toggles the scope");
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

    /// A harness for a window too short for the properties panel, so that
    /// what a node has to say does not fit in it.
    fn short_window(app: App) -> Harness<'static, App> {
        let mut h = Harness::builder()
            .with_size(egui::vec2(700.0, 300.0))
            .with_step_dt(1.0 / 60.0)
            .build_ui_state(|ui, app: &mut App| app.show(ui), app);
        h.run();
        h
    }

    /// Scrolls the properties panel down, with the wheel over it.
    fn scroll_properties_down(h: &mut Harness<'static, App>) {
        use crate::testkit::wheel;
        for _ in 0..20 {
            wheel(
                h,
                egui::pos2(650.0, 200.0),
                egui::MouseWheelUnit::Point,
                egui::vec2(0.0, -80.0),
            );
        }
        h.run_steps(20);
    }

    /// An app with a Sampler in it, selected: it has eleven parameters, so
    /// more than a short window can show.
    fn sampler_app() -> App {
        let mut app = empty();
        let id = noodle_core::NodeId(1);
        app.session.edit([Edit::Apply(Command::AddNode {
            id,
            node: Node::new(noodle_nodes::SAMPLER_ID),
        })]);
        app.editor.selected.insert(id);
        app.editor.active = Some(id);
        app
    }

    #[test]
    fn the_properties_panel_scrolls_to_a_parameter_below_the_fold() {
        let mut h = short_window(sampler_app());
        // Where the bottom of a parameter's label is, in the window.
        let bottom_of = |h: &Harness<'_, App>, label: &str| {
            h.query_all_by_label(label)
                .map(|w| w.rect().max.y)
                .fold(f32::MIN, f32::max)
        };
        assert!(
            bottom_of(&h, "Keytrack") > 300.0,
            "the parameter starts below the fold: {:?}",
            bottom_of(&h, "Keytrack")
        );
        // The wheel over the panel scrolls it.
        scroll_properties_down(&mut h);
        assert!(
            bottom_of(&h, "Loop end") <= 300.0,
            "the last parameter scrolls into view: {:?}",
            bottom_of(&h, "Loop end")
        );
    }

    #[test]
    fn the_properties_panel_keeps_its_scroll_when_another_node_is_selected() {
        let mut app = sampler_app();
        let b = noodle_core::NodeId(2);
        app.session.edit([Edit::Apply(Command::AddNode {
            id: b,
            node: Node::new(noodle_nodes::SAMPLER_ID),
        })]);
        let mut h = short_window(app);
        scroll_properties_down(&mut h);
        let top_of = |h: &Harness<'_, App>, label: &str| {
            h.query_all_by_label(label)
                .map(|w| w.rect().min.y)
                .fold(f32::MIN, f32::max)
        };
        let before = top_of(&h, "Root");
        assert!(before < 0.0, "the panel scrolled down: {before}");
        // Another node, with just as much in it, so the offset still fits.
        h.state_mut().editor.selected.clear();
        h.state_mut().editor.selected.insert(b);
        h.state_mut().editor.active = Some(b);
        h.run();
        assert_eq!(
            top_of(&h, "Root"),
            before,
            "the scroll did not jump when the selection changed"
        );
    }

    #[test]
    fn the_properties_panel_shows_a_node_with_less_in_it_after_scrolling() {
        let mut app = sampler_app();
        let osc = noodle_core::NodeId(2);
        app.session.edit([Edit::Apply(Command::AddNode {
            id: osc,
            node: Node::new("noodle.osc.sine"),
        })]);
        let mut h = short_window(app);
        scroll_properties_down(&mut h);
        // A node with much less in it: the panel shows it, not a blank.
        h.state_mut().editor.selected.clear();
        h.state_mut().editor.selected.insert(osc);
        h.state_mut().editor.active = Some(osc);
        h.run();
        let in_the_panel: Vec<_> = h
            .query_all_by_label("Frequency")
            .map(|w| w.rect())
            .filter(|r| r.min.x > 400.0)
            .collect();
        assert!(
            !in_the_panel.is_empty(),
            "the oscillator's parameter is there"
        );
        assert!(
            in_the_panel[0].min.y > 0.0 && in_the_panel[0].max.y < 300.0,
            "{in_the_panel:?}"
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
        harness
            .get_all_by_label_contains("Delete")
            .last()
            .expect("a Delete item")
            .click();
        harness.run();
        assert!(harness.state().session.project().graph().node(id).is_none());
    }

    #[test]
    fn edit_duplicate_acts_on_the_view_that_was_clicked_last() {
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
        // The node graph is where the menu acts to begin with.
        harness.get_by_label("Edit").click();
        harness.run();
        harness.get_by_label_contains("Duplicate").click();
        harness.run();
        assert_eq!(harness.state().session.project().graph().nodes().count(), 2);
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
        harness.get_by_label("No nodes yet. Right-click or press Shift+A to add one.");
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
    fn the_audio_chip_keeps_its_place_and_label_when_the_engine_fell_back() {
        let mut app = empty();
        app.session.set_audio_config(noodle_io::AudioConfig {
            output: Some("alsa:no such device".into()),
            ..noodle_io::AudioConfig::default()
        });
        app.session.start_engine();
        let mut harness = harness(app);
        harness.run_steps(3);
        assert!(harness.state().session.engine_warning().is_some());
        // The warning is the chip's amber dot and its hover, not a longer
        // label, so nothing next to it moves.
        harness.get_by_label("Audio");
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
        harness.get_by_label("Nothing to export yet: the project has no clips, automation or tempo changes, so it has no length. Import or record a clip on a track, or set Part to Range and choose how many seconds to render from the graph.");
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
    fn the_export_dialog_offers_the_mixdown_and_loudness_and_wants_something_chosen() {
        let mut harness = harness(empty());
        harness.run();
        harness.get_by_label("File").click();
        harness.run();
        harness.get_by_label("Export Audio…").click();
        harness.run();
        harness.get_by_label("Mixdown");
        harness.get_by_label_contains("Normalise mixdown to");
        harness.get_by_label_contains("dithered");
        // With the mixdown unticked and no track, there is nothing to write.
        harness.get_by_label("Mixdown").click();
        harness.run();
        harness.get_by_label("Choose the mixdown or a track to export");
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
    fn display_switches_are_set_from_the_dialog_and_remembered() {
        let dir = tempfile::tempdir().unwrap();
        let prefs = dir.path().join("prefs.ron");
        crate::prefs::init(prefs.clone());
        let mut harness = harness(empty());
        harness.run();
        harness.get_by_label("View").click();
        harness.run();
        harness.get_by_label("Display Settings…").click();
        harness.run();
        assert!(harness.state().display_dialog.is_open());
        // Everything is on but the CPU tint.
        assert!(harness.state().editor.bodies_display().event_wires);
        assert!(!harness.state().editor.bodies_display().cpu_heat);
        harness.get_by_label("Level bars on outputs").click();
        harness.run();
        assert!(!crate::prefs::load(&prefs).unwrap().display.output_meters);
        assert!(!harness.state().editor.bodies_display().output_meters);
        harness.get_by_label("Heat tint for CPU use").click();
        harness.run();
        assert!(crate::prefs::load(&prefs).unwrap().display.cpu_heat);
        assert!(harness.state().editor.bodies_display().cpu_heat);
        // Performance mode shows none of it, and keeps the choices.
        harness.get_by_label("Performance mode").click();
        harness.run();
        let saved = crate::prefs::load(&prefs).unwrap().display;
        assert!(saved.performance && saved.event_wires && saved.cpu_heat);
        let shown = harness.state().editor.bodies_display();
        assert!(!shown.event_wires && !shown.cpu_heat);
        // And the engine is told to stop measuring.
        assert!(!harness.state().session().telemetry().gather().events());
        harness.get_by_label("Performance mode").click();
        harness.run();
        assert!(harness.state().editor.bodies_display().event_wires);
        assert!(harness.state().session().telemetry().gather().events());
        assert!(
            !harness
                .state()
                .session()
                .telemetry()
                .gather()
                .output_levels()
        );
    }

    #[test]
    fn the_playback_cache_is_switched_from_its_panel_and_remembered() {
        let dir = tempfile::tempdir().unwrap();
        let prefs = dir.path().join("prefs.ron");
        crate::prefs::init(prefs.clone());
        let mut harness = harness(empty());
        harness.run();
        harness.get_by_label("View").click();
        harness.run();
        harness.get_by_label("Playback cache").click();
        harness.run();
        harness.get_by_label_contains("0 entries, 0 B of 4.0 GB");
        harness.get_by_label_contains("The audio engine is off");
        assert!(harness.state().session().auto_cache());
        harness.get_by_label("Cache playback automatically").click();
        harness.run();
        assert!(!harness.state().session().auto_cache());
        assert!(!crate::prefs::load(&prefs).unwrap().auto_cache);
        harness.get_by_label_contains("Off: everything plays live");
        harness.get_by_label("Cache playback automatically").click();
        harness.run();
        assert!(harness.state().session().auto_cache());
        assert!(crate::prefs::load(&prefs).unwrap().auto_cache);
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

    /// A crashed run's autosave of a project with one node.
    fn crash_leftover(dir: &Path) -> crate::autosave::Autosave {
        let mut crashed = empty();
        crashed.session.edit([Edit::Apply(Command::AddNode {
            id: noodle_core::NodeId(1),
            node: Node::new("noodle.osc.sine"),
        })]);
        let mut writer = crate::autosave::Autosave::for_run(dir.to_owned(), 1);
        let at = writer.next_due();
        writer.tick(at, true, 1, || (crashed.session.project_ron(), None));
        crate::autosave::Autosave::new(dir.to_owned())
    }

    #[test]
    fn a_crashed_runs_autosave_can_be_recovered() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = empty();
        app.set_autosave(crash_leftover(dir.path()));
        let mut harness = harness(app);
        harness.run();
        harness.get_by_label("Discard");
        harness.get_by_label("Recover").click();
        harness.run();
        let session = harness.state().session();
        assert_eq!(session.project().graph().nodes().count(), 1);
        assert!(session.is_dirty(), "recovered work still needs saving");
        assert!(harness.query_by_label("Recover").is_none());
        assert!(
            crate::autosave::Autosave::new(dir.path().to_owned())
                .pending()
                .is_none(),
            "the copy is gone once recovered"
        );
    }

    #[test]
    fn discarding_the_autosave_leaves_the_project_alone() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = empty();
        app.set_autosave(crash_leftover(dir.path()));
        let mut harness = harness(app);
        harness.run();
        harness.get_by_label("Discard").click();
        harness.run();
        assert_eq!(
            harness.state().session().project().graph().nodes().count(),
            0
        );
        assert!(
            crate::autosave::Autosave::new(dir.path().to_owned())
                .pending()
                .is_none()
        );
    }

    #[test]
    fn no_autosave_means_no_question() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = empty();
        app.set_autosave(crate::autosave::Autosave::new(dir.path().to_owned()));
        let mut harness = harness(app);
        harness.run();
        assert!(harness.query_by_label("Recover").is_none());
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
        assert_eq!(harness.state().keyboard.octave(), 5);
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
    fn an_open_piano_roll_follows_the_selected_clip() {
        use noodle_core::{Clip, ClipId, Tick};
        let mut app = empty();
        app.session.edit([Edit::Apply(Command::AddNode {
            id: noodle_core::NodeId(1),
            node: Node::new(crate::timeline::TRACK_INPUT),
        })]);
        for (id, start) in [(1, 0), (2, 3840)] {
            app.session.edit([Edit::Apply(Command::AddClip {
                id: ClipId(id),
                clip: Clip::midi(noodle_core::NodeId(1), Tick(start), Tick(3840)),
            })]);
        }
        app.piano_roll.open(ClipId(1));
        let mut harness = harness(app);
        harness.run();
        assert_eq!(harness.state().piano_roll.clip(), Some(ClipId(1)));
        harness.state_mut().timeline.select_only(ClipId(2));
        harness.run();
        assert_eq!(harness.state().piano_roll.clip(), Some(ClipId(2)));
        // A roll that is closed stays closed.
        harness.state_mut().piano_roll.close();
        harness.state_mut().timeline.select_only(ClipId(1));
        harness.run();
        assert_eq!(harness.state().piano_roll.clip(), None);
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

    /// Typing in a text field wins over the note keys: the letters reach the
    /// field and no note sounds, from the first key after it is clicked.
    #[test]
    fn a_focused_text_field_overrides_the_note_keys() {
        let mut app = empty();
        app.keyboard.set_on(true);
        let ctx = egui::Context::default();
        let mut heard = app.session.midi_bus().subscribe();
        let mut text = String::new();
        let field = egui::Id::new("a text field");
        let frame = |app: &mut App, events: Vec<egui::Event>, text: &mut String| {
            let input = egui::RawInput {
                events,
                ..Default::default()
            };
            let mut seen = true;
            let mut output = ctx.run_ui(input, |ui| {
                app.keyboard_midi(ui.ctx());
                seen = ui.ctx().input(|i| i.key_pressed(Key::A));
                let edit = ui.add(egui::TextEdit::singleline(text).id(field));
                if !edit.has_focus() {
                    edit.request_focus();
                }
            });
            output.textures_delta.clear();
            seen
        };
        // The frame that opens the field has focus after the keys are read,
        // so a key arriving with it may still be a note; the next ones not.
        frame(&mut app, vec![], &mut text);
        let key = egui::Event::Key {
            key: Key::A,
            physical_key: Some(Key::A),
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        };
        let seen = frame(
            &mut app,
            vec![key, egui::Event::Text("a".into())],
            &mut text,
        );
        assert!(seen, "the field sees A, it was not taken as a note");
        assert_eq!(text, "a");
        assert_eq!(heard.pop(), None, "no note sounded");
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

    /// Cmd+Shift+S is Save As, not Save: a dirty saved project stays dirty,
    /// because Save As only asks for a file (and a headless run answers none).
    #[test]
    fn cmd_shift_s_is_save_as_not_save() {
        let dir = std::env::temp_dir().join(format!("noodle-app-{}-saveas", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut app = empty();
        assert!(app.session.save_as(&dir.join("project.ron")));
        app.session.edit([Edit::Apply(Command::AddNode {
            id: noodle_core::NodeId(1),
            node: Node::new("noodle.osc.sine"),
        })]);
        assert!(app.session.is_dirty());
        let mut harness = harness(app);
        harness.run();
        harness.key_press_modifiers(Modifiers::COMMAND.plus(Modifiers::SHIFT), Key::S);
        harness.run();
        assert!(
            harness.state().session().is_dirty(),
            "Save As ran, not Save"
        );
        harness.key_press_modifiers(Modifiers::COMMAND, Key::S);
        harness.run();
        assert!(!harness.state().session().is_dirty(), "Cmd+S saves");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Issue #420: Enter in the template dialog saves it, and Ctrl+A selects
    /// the name being typed.
    #[test]
    fn enter_confirms_the_template_dialog() {
        let mut app = empty();
        app.template_name = Some(String::new());
        let mut harness = harness(app);
        harness.run();
        harness.key_press_modifiers(Modifiers::COMMAND, Key::A);
        harness.event(egui::Event::Text("Band".into()));
        harness.run();
        assert_eq!(harness.state().template_name.as_deref(), Some("Band"));
        harness.key_press(Key::Enter);
        harness.run();
        assert_eq!(harness.state().template_name, None);
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
    fn switching_the_engine_or_keyboard_does_not_move_the_transport() {
        let mut harness = harness(empty());
        harness.run();
        let place = |h: &Harness<'_, App>| {
            [
                h.get_by_label("▶").rect(),
                h.get_by_label("⏺").rect(),
                h.get_by_label("Audio").rect(),
            ]
        };
        let before = place(&harness);
        harness.get_by_label("Computer keyboard").click();
        harness.run();
        harness.get_by_label("Audio").click();
        harness.run_steps(4);
        assert_eq!(place(&harness), before);
    }

    #[test]
    fn typing_a_tempo_does_not_move_the_buttons_after_it() {
        let mut harness = harness(empty());
        harness.run();
        let source = |h: &Harness<'_, App>| h.get_by_label_contains("Tempo map").rect();
        let before = source(&harness);
        // The tempo field is the spin button that holds the default 120.
        let field = harness
            .query_all_by_role(egui::accesskit::Role::SpinButton)
            .find(|n| n.value().is_some_and(|v| v.starts_with("120")))
            .unwrap();
        field.click();
        harness.run();
        assert_eq!(source(&harness), before);
    }

    #[test]
    fn editing_or_moving_the_position_does_not_move_the_transport() {
        let mut harness = harness(empty());
        harness.run();
        let place = |h: &Harness<'_, App>| {
            [
                h.get_by_label("▶").rect(),
                h.get_by_label("⏺").rect(),
                h.get_by_label("Position").rect(),
                h.get_by_label_contains("Tempo map").rect(),
                h.get_by_label("Audio").rect(),
            ]
        };
        let before = place(&harness);
        // A far-off, wide position.
        harness
            .state_mut()
            .session
            .seek(noodle_core::Tick(9999 * 4 * noodle_core::TICKS_PER_QUARTER));
        harness.run();
        assert_eq!(place(&harness), before, "wide position");
        // Typing into the field.
        harness.get_by_label("Position").click();
        harness.run();
        harness.key_press_modifiers(Modifiers::COMMAND, Key::A);
        harness.event(egui::Event::Text("123456.2.100".into()));
        harness.run();
        assert_eq!(place(&harness), before, "while typing");
    }

    #[test]
    fn the_record_options_arrow_sits_flush_against_the_record_button() {
        let mut harness = harness(empty());
        harness.run();
        let record = harness.get_by_label("⏺").rect();
        let arrow = harness.get_by_label("Record options").rect();
        assert_eq!(record.right(), arrow.left());
    }

    #[test]
    fn a_position_typed_into_the_transport_moves_the_playhead() {
        let mut harness = harness(empty());
        harness.run();
        // Bar 3, beat 2: two bars and a beat in.
        harness.get_by_label("Position").click();
        harness.run();
        harness.key_press_modifiers(Modifiers::COMMAND, Key::A);
        harness.event(egui::Event::Text("3.2".into()));
        harness.run();
        harness.key_press(Key::Enter);
        harness.run();
        assert_eq!(
            harness.state().session.playhead(),
            noodle_core::Tick(9 * noodle_core::TICKS_PER_QUARTER)
        );
    }

    #[test]
    fn adding_a_switch_from_the_edit_menu_shows_a_strip_button_that_presses_it() {
        let mut harness = harness(empty());
        harness.run();
        // The metronome's own button makes a metronome with a switch left.
        harness.get_by_label("♩").click();
        harness.run();
        // With no switch yet there is no strip: the Edit menu makes the first.
        harness.get_by_label("Edit").click();
        harness.run();
        harness.get_by_label_contains("Add Switch Button").click();
        harness.run();
        harness.get_by_label("Metronome · Accent").click();
        harness.run();
        let shown = || {
            let state = harness.state();
            pill_buttons::extras(state.session.project(), state.session.registry())
        };
        assert_eq!(shown().len(), 1);
        assert!(shown()[0].on);
        harness.get_by_label("Metronome").click();
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
        assert!(h.state().clip_editor.subject().is_none());
        let at = h.get_by_label("Sample waveform").rect().center();
        double_click(&mut h, at);
        h.run();
        assert_eq!(
            h.state().clip_editor.subject(),
            Some(clip_editor::subject::Subject::Sampler(id))
        );
        h.get_by_label("Sample editor · 1.00 s at 48000 Hz");
        // Close shuts it again.
        h.get_by_label("Close").click();
        h.run();
        assert!(h.state().clip_editor.subject().is_none());
    }

    /// Where the sample editor drew its waveform, and the seconds axis.
    struct SampleAxis {
        origin: f32,
        pps: f32,
    }

    impl SampleAxis {
        fn x(&self, seconds: f64) -> f32 {
            self.origin + seconds as f32 * self.pps
        }
    }

    fn sample_axis(h: &Harness<'static, App>) -> (egui::Rect, SampleAxis) {
        let g = h
            .state()
            .clip_editor
            .plain_geometry()
            .expect("the editor has been drawn");
        (
            g.wave,
            SampleAxis {
                origin: g.window,
                pps: g.pps,
            },
        )
    }

    fn open_sample_view(h: &mut Harness<'static, App>) -> (egui::Rect, SampleAxis) {
        use crate::testkit::double_click;
        let at = h.get_by_label("Sample waveform").rect().center();
        double_click(h, at);
        h.run();
        sample_axis(h)
    }

    #[test]
    fn dragging_the_start_marker_in_the_sample_view_sets_the_start_parameter() {
        use crate::testkit::drag_between;
        let (mut h, id) = sampler_rig();
        let (body, axis) = open_sample_view(&mut h);
        // The sample is a second long.
        let y = body.center().y;
        let from = egui::Pos2::new(axis.x(0.0) + 1.0, y);
        let to = egui::Pos2::new(axis.x(0.25), y);
        drag_between(&mut h, egui::Modifiers::NONE, from, to);
        let start = h.state().session.project().graph().node(id).unwrap().params["start"];
        assert!((start - 0.25).abs() < 0.01, "{start}");
        // One undo step.
        h.state_mut().session.undo();
        let node = h.state().session.project().graph().node(id).unwrap();
        assert!(!node.params.contains_key("start"));
    }

    #[test]
    fn dragging_the_sample_between_the_markers_slips_the_region_along_it() {
        use crate::testkit::drag_between;
        let (mut h, id) = sampler_rig();
        // A region in the middle to slip.
        h.state_mut().session.edit([
            Edit::Apply(Command::SetParam {
                node: id,
                key: "start".into(),
                value: Some(0.2),
            }),
            Edit::Apply(Command::SetParam {
                node: id,
                key: "end".into(),
                value: Some(0.6),
            }),
        ]);
        h.state_mut().session.edit([
            Edit::Apply(Command::SetParam {
                node: id,
                key: "loop".into(),
                value: Some(1.0),
            }),
            Edit::Apply(Command::SetParam {
                node: id,
                key: "loop_start".into(),
                value: Some(0.3),
            }),
            Edit::Apply(Command::SetParam {
                node: id,
                key: "loop_end".into(),
                value: Some(0.5),
            }),
        ]);
        h.run();
        let (body, axis) = open_sample_view(&mut h);
        let y = body.center().y;
        // The audio follows the pointer, 0.1 s to the left.
        let from = egui::Pos2::new(axis.x(0.4), y);
        let to = egui::Pos2::new(axis.x(0.3), y);
        drag_between(&mut h, egui::Modifiers::NONE, from, to);
        let params = &h.state().session.project().graph().node(id).unwrap().params;
        assert!((params["start"] - 0.3).abs() < 0.01, "{}", params["start"]);
        assert!((params["loop_start"] - 0.4).abs() < 0.01);
        assert!((params["loop_end"] - 0.6).abs() < 0.01);
        assert!((params["end"] - 0.7).abs() < 0.01, "{}", params["end"]);
    }

    #[test]
    fn ctrl_scroll_zooms_the_sample_view_around_the_pointer_and_scroll_pans() {
        use crate::testkit::{wheel, wheel_with};
        let (mut h, _id) = sampler_rig();
        let (body, axis) = open_sample_view(&mut h);
        let point = egui::Pos2::new(body.left() + body.width() * 0.5, body.center().y);
        let (zoom0, under0) = (
            h.state().clip_editor.zoom(),
            (point.x - axis.origin) / axis.pps,
        );
        for _ in 0..10 {
            wheel_with(
                &mut h,
                point,
                egui::MouseWheelUnit::Line,
                egui::vec2(0.0, 3.0),
                egui::Modifiers::COMMAND,
            );
        }
        h.step();
        let zoom = h.state().clip_editor.zoom();
        assert!(zoom > zoom0 * 2.0, "zoomed in: {zoom0} -> {zoom}");
        // The second under the pointer stays under it.
        let (_, axis) = sample_axis(&h);
        let under = (point.x - axis.origin) / axis.pps;
        assert!((under - under0).abs() < 0.05, "{under0} -> {under}");
        // The plain wheel pans.
        let scroll = h.state().clip_editor.scroll();
        for _ in 0..3 {
            wheel(
                &mut h,
                point,
                egui::MouseWheelUnit::Point,
                egui::vec2(-60.0, 0.0),
            );
        }
        assert!(h.state().clip_editor.scroll() > scroll, "panned right");
    }

    #[test]
    fn adding_an_automation_track_shows_its_node_at_the_top_level_of_the_graph() {
        let mut app = empty();
        app.session.edit([Edit::Apply(Command::AddNode {
            id: noodle_core::NodeId(1),
            node: Node::new(noodle_nodes::TRACK_INPUT_ID),
        })]);
        let mut h = Harness::builder()
            .with_size(egui::vec2(1100.0, 700.0))
            .with_step_dt(1.0 / 60.0)
            .build_ui_state(|ui, app: &mut App| app.show(ui), app);
        h.run();
        // The editor is looking inside a group when the track is added.
        let group = h
            .state()
            .session
            .project()
            .graph()
            .nodes()
            .find(|(_, n)| n.type_id == noodle_core::group::GROUP)
            .map(|(id, _)| id);
        h.state_mut().editor.group = group;
        h.get_by_label("+ Auto").click();
        h.run();
        let state = h.state();
        let (id, _) = state
            .session
            .project()
            .graph()
            .nodes()
            .find(|(_, n)| n.type_id == noodle_nodes::AUTOMATION_NODE_ID)
            .expect("an Automation node");
        assert_eq!(state.editor.group, None, "the top level is shown");
        assert!(
            state.editor.selected.contains(&id),
            "and the node is picked"
        );
    }

    #[test]
    fn the_arrangement_panel_does_not_grow_to_fit_off_screen_lanes() {
        let mut app = empty();
        // Several free automation lanes: each is an Automation node plus a
        // lane, so the lanes pile up below the tracks and most sit off screen.
        for _ in 0..30 {
            let graph = app.session.project().graph().clone();
            let node = app.session.new_node_id();
            let lane = app.session.project().next_lane_id();
            let cmd = crate::timeline::add_track::automation_command(&graph, node, lane);
            app.session.edit([Edit::Apply(cmd)]);
        }
        let mut h = Harness::builder()
            .with_size(egui::vec2(1100.0, 700.0))
            .with_step_dt(1.0 / 60.0)
            .build_ui_state(|ui, app: &mut App| app.show(ui), app);
        h.run();
        let height =
            egui::containers::panel::PanelState::load(&h.ctx, egui::Id::new("arrangement"))
                .expect("the arrangement panel has been shown")
                .size()
                .y;
        assert_eq!(
            height,
            theme::timeline::DEFAULT_HEIGHT,
            "the panel keeps its default height instead of growing to fit the lanes"
        );
    }

    #[test]
    fn scrolling_down_the_tracks_does_not_resize_the_sequencer_panel() {
        use crate::testkit::wheel;
        let mut app = empty();
        for n in 1..=14 {
            app.session.edit([Edit::Apply(Command::AddNode {
                id: noodle_core::NodeId(n),
                node: Node::new(noodle_nodes::TRACK_INPUT_ID),
            })]);
        }
        let mut h = Harness::builder()
            .with_size(egui::vec2(1100.0, 700.0))
            .with_step_dt(1.0 / 60.0)
            .build_ui_state(|ui, app: &mut App| app.show(ui), app);
        h.run();
        let height = |h: &Harness<'static, App>| {
            h.ctx
                .data(|d| d.get_temp::<f32>(egui::Id::new("arrangement content height")))
                .expect("the sequencer has been drawn")
        };
        let before = height(&h);
        let over_tracks = egui::pos2(600.0, 150.0);
        for _ in 0..12 {
            wheel(
                &mut h,
                over_tracks,
                egui::MouseWheelUnit::Point,
                egui::vec2(0.0, -80.0),
            );
        }
        h.run_steps(20);
        assert!(h.state().timeline.scroll_y() > 0.0, "it scrolled");
        assert_eq!(height(&h), before, "the panel kept its size");
    }

    #[test]
    fn scrolling_to_the_bottom_reaches_the_add_track_buttons() {
        use crate::testkit::wheel;
        let mut app = empty();
        for n in 1..=14 {
            app.session.edit([Edit::Apply(Command::AddNode {
                id: noodle_core::NodeId(n),
                node: Node::new(noodle_nodes::TRACK_INPUT_ID),
            })]);
        }
        let mut h = Harness::builder()
            .with_size(egui::vec2(1100.0, 700.0))
            .with_step_dt(1.0 / 60.0)
            .build_ui_state(|ui, app: &mut App| app.show(ui), app);
        h.run();
        assert!(
            h.query_by_label("+ Track").is_none(),
            "the buttons are below the fold at the top"
        );
        let over_tracks = egui::pos2(600.0, 150.0);
        for _ in 0..20 {
            wheel(
                &mut h,
                over_tracks,
                egui::MouseWheelUnit::Point,
                egui::vec2(0.0, -80.0),
            );
        }
        h.run_steps(20);
        assert!(
            h.query_by_label("+ Track").is_some(),
            "the + Track button scrolls into view"
        );
    }

    #[test]
    fn resizing_the_arrangement_panel_keeps_the_graph_content_put() {
        let mut app = empty();
        app.session.edit([Edit::Apply(Command::AddNode {
            id: noodle_core::NodeId(1),
            node: Node::new("noodle.osc.sine").at(200.0, 150.0),
        })]);
        let mut h = harness(app);
        h.run();
        let before = on_canvas(&h, egui::pos2(200.0, 150.0));
        // Make the arrangement panel taller: the graph canvas shrinks from the
        // top, but the graph content must not move with its corner.
        let state = egui::containers::panel::PanelState {
            outer_rect: egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(100.0, 400.0)),
        };
        h.ctx
            .data_mut(|d| d.insert_persisted(egui::Id::new("arrangement"), state));
        h.run();
        assert_eq!(
            on_canvas(&h, egui::pos2(200.0, 150.0)),
            before,
            "the graph content stays where it was on screen"
        );
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
    fn a_clip_dropped_on_the_sample_in_a_scrolled_properties_panel_sets_the_sample() {
        use crate::testkit::wheel;
        let (mut h, clip, _dir) = clip_rig();
        // A short window, so the properties panel has more to show than fits.
        h.set_size(egui::vec2(1100.0, 400.0));
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
        // Scroll the panel down, so the sample field moves up with it.
        let before = h.get_by_label("Load sample…").rect().center();
        for _ in 0..3 {
            wheel(
                &mut h,
                egui::pos2(1050.0, 200.0),
                egui::MouseWheelUnit::Point,
                egui::vec2(0.0, -40.0),
            );
        }
        h.run_steps(10);
        let after = h.get_by_label("Load sample…").rect().center();
        assert!(
            after.y < before.y,
            "the panel scrolled down: {before:?} then {after:?}"
        );
        // The drop lands on the sample field, which moved with the scroll.
        carry_clip(&mut h, clip, after);
        let node = h.state().session.project().graph().node(sampler).unwrap();
        assert!(
            (node.params["start"] - 0.25).abs() < 1e-4,
            "{:?}",
            node.params
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
