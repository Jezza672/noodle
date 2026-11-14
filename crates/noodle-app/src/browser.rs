//! The browser: one side panel holding everything that can be found and put
//! into the project. Its tabs are Sounds (the sample packs, ADR 0012), Nodes,
//! Groups (the library's group templates) and Presets (for the selected node
//! or group).
//!
//! The whole panel collapses to a thin strip, so it can stay out of the way
//! of the canvas and still be one click from open. Nodes and Groups are
//! added by a click (at the centre of the canvas) or by dragging them onto
//! it, as a sound is; a click on a preset applies it to the selected node.

use egui::{Context, Sense, Ui, text::LayoutJob};
use noodle_core::NodeId;
use noodle_core::group::GROUP;

use crate::carry::{self, Item};
use crate::editor::{self, Choice, EditorState, SearchEntry};
use crate::session::{Edit, Session};
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
    Groups,
    Presets,
}

impl Tab {
    pub const ALL: [Tab; 4] = [Tab::Sounds, Tab::Nodes, Tab::Groups, Tab::Presets];

    pub fn name(self) -> &'static str {
        match self {
            Tab::Sounds => "Sounds",
            Tab::Nodes => "Nodes",
            Tab::Groups => "Groups",
            Tab::Presets => "Presets",
        }
    }

    fn hint(self) -> &'static str {
        match self {
            Tab::Sounds => "Sounds from packs and folders",
            Tab::Nodes => "Every node type, to click or drag onto the canvas",
            Tab::Groups => "Saved groups and instruments, to click or drag onto the canvas",
            Tab::Presets => "Presets for the selected node or group",
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
    groups: String,
    presets: String,
}

impl Default for Browser {
    fn default() -> Self {
        Self {
            shown: false,
            collapsed: false,
            tab: Tab::Sounds,
            nodes: String::new(),
            groups: String::new(),
            presets: String::new(),
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
        session: &mut Session,
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
                        .on_hover_text("Open the browser: sounds, nodes, groups and presets")
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
                    Tab::Nodes => nodes(ui, &mut self.nodes, session, editor),
                    Tab::Groups => groups(ui, &mut self.groups, session, editor),
                    Tab::Presets => presets(ui, &mut self.presets, session, editor),
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
fn row(ctx: &Context, ui: &mut Ui, name: &str, detail: &str, item: Item) -> bool {
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
        .on_hover_text("Click to add it at the centre of the canvas, or drag it onto the canvas");
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

fn entries(session: &Session, query: &str) -> Vec<SearchEntry> {
    editor::search_entries(session.registry(), session.library(), query, false)
}

fn is_group(entry: &SearchEntry) -> bool {
    matches!(&entry.choice, Choice::Template(_) | Choice::Node(GROUP))
}

fn nodes(ui: &mut Ui, query: &mut String, session: &Session, editor: &mut EditorState) {
    search_box(ui, query, "Search nodes");
    let found: Vec<SearchEntry> = entries(session, query)
        .into_iter()
        .filter(|e| matches!(e.choice, Choice::Node(_)) && !is_group(e))
        .collect();
    ui.weak(format!("{} nodes", found.len()));
    let ctx = ui.ctx().clone();
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            let mut category = "";
            for entry in &found {
                if entry.category != category {
                    category = &entry.category;
                    ui.add_space(4.0);
                    ui.strong(category);
                }
                if let Choice::Node(id) = entry.choice
                    && row(&ctx, ui, &entry.name, "", Item::Node(id))
                {
                    editor.request_add(Choice::Node(id), None);
                }
            }
        });
}

fn groups(ui: &mut Ui, query: &mut String, session: &Session, editor: &mut EditorState) {
    search_box(ui, query, "Search groups");
    let found: Vec<SearchEntry> = entries(session, query)
        .into_iter()
        .filter(is_group)
        .collect();
    ui.weak(format!("{} groups", found.len()));
    let ctx = ui.ctx().clone();
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for entry in &found {
                let (item, detail) = match &entry.choice {
                    Choice::Template(id) => (Item::Template(carry::template_key(id)), ""),
                    _ => (Item::Node(GROUP), "empty"),
                };
                if row(&ctx, ui, &entry.name, detail, item) {
                    editor.request_add(entry.choice.clone(), None);
                }
            }
        });
}

/// What a library entry ID says about where it came from.
fn is_user(entry: &str) -> bool {
    entry.starts_with(noodle_core::library::USER)
}

fn presets(ui: &mut Ui, query: &mut String, session: &mut Session, editor: &EditorState) {
    let Some(id) = editor.active else {
        ui.label("Select a node or group to see the presets that fit it.");
        return;
    };
    let Some(node) = session.project().graph().node(id) else {
        ui.label("Select a node or group to see the presets that fit it.");
        return;
    };
    let heading = if node.type_id == GROUP {
        "Group presets"
    } else {
        "Node presets"
    };
    ui.strong(heading);
    search_box(ui, query, "Search presets");
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    let applied = session.preset_status(id);
    let available: Vec<(String, String)> = session
        .presets_for(id)
        .into_iter()
        .filter(|(_, name)| {
            let name = name.to_lowercase();
            words.iter().all(|w| name.contains(w))
        })
        .map(|(entry, name)| (entry.to_owned(), name.to_owned()))
        .collect();
    if available.is_empty() {
        ui.weak("No presets fit this one yet. Save its values from the properties panel.");
        return;
    }
    let mut chosen: Option<NodeId> = None;
    let mut edit: Option<Edit> = None;
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for (entry, name) in &available {
                let on = applied.as_ref().is_some_and(|a| &a.id == entry);
                let mut label = if is_user(entry) {
                    format!("{name} (yours)")
                } else {
                    name.clone()
                };
                if on {
                    label.push_str(if applied.as_ref().is_some_and(|a| a.modified) {
                        "  ✓ modified"
                    } else {
                        "  ✓"
                    });
                }
                if ui
                    .selectable_label(on, label)
                    .on_hover_text("Apply to the selected node, as one undo step")
                    .clicked()
                {
                    edit = session.apply_preset(id, entry);
                    chosen = Some(id);
                }
            }
        });
    if let (Some(edit), Some(_)) = (edit, chosen) {
        session.edit([edit]);
    }
}
