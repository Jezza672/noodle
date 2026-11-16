//! The browser: one side panel holding everything that can be found and put
//! into the project. Its tabs are Sounds (the sample packs, ADR 0012) and
//! Nodes (every node type and the library's group templates, in one list).
//!
//! The whole panel collapses to a thin strip, so it can stay out of the way
//! of the canvas and still be one click from open. A node or group is added by
//! a click (at the centre of the canvas) or by dragging it onto the canvas, as
//! a sound is. One that has presets unfolds to list them, and a preset is added
//! the same way, with its values. Applying a preset to a node already in the
//! project is in the properties panel.

use std::collections::HashSet;

use egui::{Context, Sense, Ui, text::LayoutJob};
use noodle_core::group::GROUP;

use crate::carry::{self, Item};
use crate::editor::{self, Choice, EditorState, SearchEntry};
use crate::session::Session;
use crate::sounds::Sounds;
use crate::theme;

/// The width of the browser while it is open, until the user drags it.
const WIDTH: f32 = 320.0;
/// The width of the strip it collapses to.
const STRIP: f32 = 28.0;

/// A tab of the browser.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    Sounds,
    Nodes,
    /// Groups and presets that sources offer, to install.
    Library,
}

impl Tab {
    pub const ALL: [Tab; 3] = [Tab::Sounds, Tab::Nodes, Tab::Library];

    pub fn name(self) -> &'static str {
        match self {
            Tab::Sounds => "Sounds",
            Tab::Nodes => "Nodes",
            Tab::Library => "Library",
        }
    }

    fn hint(self) -> &'static str {
        match self {
            Tab::Sounds => "Sounds from packs and folders",
            Tab::Nodes => "Every node and saved group, with their presets, to add to the canvas",
            Tab::Library => "Groups and presets from packs, to install into your library",
        }
    }
}

pub struct Browser {
    /// Whether the panel is on the screen at all (View > Browser).
    shown: bool,
    /// Reduced to a strip: still there, but out of the way.
    collapsed: bool,
    tab: Tab,
    nodes: String,
    /// The entries (a node type or a template) unfolded to show their presets.
    unfolded: HashSet<String>,
}

impl Default for Browser {
    fn default() -> Self {
        Self {
            shown: false,
            collapsed: false,
            tab: Tab::Sounds,
            nodes: String::new(),
            unfolded: HashSet::new(),
        }
    }
}

impl Browser {
    /// Whether the panel is on the screen, open or collapsed.
    pub fn is_shown(&self) -> bool {
        self.shown
    }

    #[cfg(test)]
    pub fn is_collapsed(&self) -> bool {
        self.collapsed
    }

    /// The tab that is open, if the panel is open and not collapsed.
    pub fn visible_tab(&self) -> Option<Tab> {
        (self.shown && !self.collapsed).then_some(self.tab)
    }

    /// Fills the Nodes tab's search box, so a test's row is on screen.
    #[cfg(test)]
    pub fn search_nodes(&mut self, query: &str) {
        self.nodes = query.to_owned();
    }

    /// View > Browser: shows the panel, or hides it.
    pub fn toggle(&mut self) {
        self.shown = !self.shown;
        if self.shown {
            self.collapsed = false;
        }
    }

    /// Shows the panel on `tab`, opened out if it was collapsed.
    pub fn open_tab(&mut self, tab: Tab) {
        self.shown = true;
        self.collapsed = false;
        self.tab = tab;
    }

    /// Collapses the panel to a strip, or opens it out again.
    pub fn toggle_collapsed(&mut self) {
        self.collapsed = !self.collapsed;
    }

    /// Draws the panel at the left of the window.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        session: &Session,
        editor: &mut EditorState,
        sounds: &mut Sounds,
    ) {
        if !self.shown {
            return;
        }
        if self.collapsed {
            egui::Panel::left("browser strip")
                .resizable(false)
                .exact_size(STRIP)
                .frame(egui::Frame::side_top_panel(ui.style()).fill(theme::PANEL))
                .show(ui, |ui| {
                    #[cfg(test)]
                    crate::testkit::mark_view(ui, "browser strip");
                    if ui
                        .button("▶")
                        .on_hover_text("Open the browser: sounds, nodes and groups")
                        .clicked()
                    {
                        self.collapsed = false;
                    }
                });
            return;
        }
        egui::Panel::left("browser")
            .resizable(true)
            .default_size(WIDTH)
            .size_range(200.0..=640.0)
            .frame(egui::Frame::side_top_panel(ui.style()).fill(theme::PANEL))
            .show(ui, |ui| {
                #[cfg(test)]
                crate::testkit::mark_view(ui, "browser");
                ui.set_min_width(ui.available_width());
                ui.horizontal(|ui| {
                    for tab in Tab::ALL {
                        if ui
                            .selectable_label(self.tab == tab, tab.name())
                            .on_hover_text(tab.hint())
                            .clicked()
                        {
                            self.tab = tab;
                        }
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .button("◀")
                            .on_hover_text("Collapse the browser out of the way")
                            .clicked()
                        {
                            self.collapsed = true;
                        }
                    });
                });
                ui.separator();
                match self.tab {
                    Tab::Sounds => sounds.show(ui),
                    Tab::Library => sounds.show_library(ui),
                    Tab::Nodes => nodes(ui, &mut self.nodes, &mut self.unfolded, session, editor),
                }
            });
    }
}

fn search_box(ui: &mut Ui, query: &mut String, hint: &str) {
    ui.add(
        egui::TextEdit::singleline(query)
            .hint_text(hint)
            .desired_width(ui.available_width()),
    );
}

/// One row that can be clicked to add what it names, or dragged onto the
/// canvas. Returns whether it was clicked.
fn row(ctx: &Context, ui: &mut Ui, name: &str, detail: &str, item: Item, tip: &str) -> bool {
    let mut job = LayoutJob::default();
    let font = egui::TextStyle::Body.resolve(ui.style());
    job.append(
        name,
        0.0,
        egui::TextFormat::simple(font, ui.visuals().text_color()),
    );
    if !detail.is_empty() {
        job.append(
            &format!("  {detail}"),
            0.0,
            egui::TextFormat::simple(
                egui::TextStyle::Small.resolve(ui.style()),
                ui.visuals().weak_text_color(),
            ),
        );
    }
    let response = ui
        .add(
            egui::Button::new(job)
                .frame(false)
                .min_size(egui::vec2(ui.available_width(), 0.0))
                .sense(Sense::click_and_drag()),
        )
        .on_hover_text(tip);
    if let (true, Some(pointer)) = (response.dragged(), ctx.pointer_latest_pos()) {
        carry::carrying(ctx, item, pointer, name);
    }
    if response.drag_stopped()
        && let Some(pointer) = ctx.pointer_latest_pos()
    {
        carry::release(ctx, item, pointer);
    }
    response.clicked()
}

const ADD_TIP: &str = "Click to add it at the centre of the canvas, or drag it onto the canvas";
const PRESET_TIP: &str =
    "Click to add it at the centre of the canvas with this preset, or drag it onto the canvas";

/// The width of the arrow that unfolds a row, which a row without one keeps
/// free so the names line up.
const ARROW: f32 = 18.0;

/// Everything that can be added, as one list by category: the nodes, then the
/// saved groups. A row with presets has an arrow that unfolds them.
fn nodes(
    ui: &mut Ui,
    query: &mut String,
    unfolded: &mut HashSet<String>,
    session: &Session,
    editor: &mut EditorState,
) {
    search_box(ui, query, "Search nodes and groups");
    let found: Vec<SearchEntry> =
        editor::search_entries(session.registry(), session.library(), query, false)
            .into_iter()
            .filter(|e| !matches!(e.choice, Choice::Frame))
            .collect();
    ui.weak(format!("{} found", found.len()));
    let ctx = ui.ctx().clone();
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .scroll_bar_visibility(egui::containers::scroll_area::ScrollBarVisibility::AlwaysVisible)
        .show(ui, |ui| {
            let mut category = "";
            for entry in &found {
                if entry.category != category {
                    category = &entry.category;
                    ui.add_space(4.0);
                    ui.strong(category);
                }
                entry_row(ui, &ctx, entry, unfolded, session, editor);
            }
        });
}

fn entry_row(
    ui: &mut Ui,
    ctx: &Context,
    entry: &SearchEntry,
    unfolded: &mut HashSet<String>,
    session: &Session,
    editor: &mut EditorState,
) {
    let (key, item, presets) = match &entry.choice {
        Choice::Node(GROUP) => (GROUP.to_owned(), Item::Node(GROUP), Vec::new()),
        Choice::Node(id) => (
            (*id).to_owned(),
            Item::Node(id),
            session.presets_for_type(id),
        ),
        Choice::Template(id) => (
            id.clone(),
            Item::Template(carry::entry_key(id)),
            session.presets_for_template(id),
        ),
        Choice::Frame => return,
    };
    let open = unfolded.contains(&key);
    let mut clicked = false;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        if presets.is_empty() {
            ui.add_space(ARROW);
        } else if ui
            .add_sized(
                [ARROW, ui.spacing().interact_size.y],
                egui::Button::new(if open { "▾" } else { "▸" }).frame(false),
            )
            .on_hover_text(if open {
                "Hide its presets"
            } else {
                "Show its presets"
            })
            .clicked()
        {
            if open {
                unfolded.remove(&key);
            } else {
                unfolded.insert(key.clone());
            }
        }
        let detail = if presets.is_empty() {
            String::new()
        } else {
            format!("{} presets", presets.len())
        };
        clicked = row(ctx, ui, &entry.name, &detail, item, ADD_TIP);
    });
    if clicked {
        editor.request_add(entry.choice.clone(), None, None);
    }
    if !open {
        return;
    }
    for (preset, name) in presets {
        let item = match (&entry.choice, carry::entry_key(preset)) {
            (Choice::Node(id), key) => Item::NodePreset(id, key),
            (Choice::Template(template), key) => {
                Item::TemplatePreset(carry::entry_key(template), key)
            }
            (Choice::Frame, _) => continue,
        };
        let label = if is_user(preset) {
            format!("{name} (yours)")
        } else {
            name.to_owned()
        };
        let mut clicked = false;
        ui.horizontal(|ui| {
            ui.add_space(ARROW * 2.0);
            clicked = row(ctx, ui, &label, "", item, PRESET_TIP);
        });
        if clicked {
            editor.request_add(entry.choice.clone(), None, Some(preset.to_owned()));
        }
    }
}

/// Whether a library entry is the user's own.
fn is_user(entry: &str) -> bool {
    entry.starts_with(noodle_core::library::USER)
}
