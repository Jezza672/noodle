//! The Settings window: the manager view, with a tab each for the General
//! preferences, the Audio devices, the Display switches, the separation
//! Models and the Sound sources. The first action for most of these happens
//! where it is needed (the Sounds panel, Create drum rack, Split to stems, the
//! View menu); this is where they are kept up afterwards. Everything in it
//! is saved in the user's settings file (see [`crate::prefs`]); the audio
//! devices and the plugin sandbox belong to this machine and are left out of
//! exported and project-embedded settings.

use noodle_io::{AudioConfig, LatencyReport};

use crate::devices::DevicePicker;
use crate::display::Display;
use crate::session::Session;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tab {
    #[default]
    General,
    Audio,
    Display,
    Models,
    Sources,
}

impl Tab {
    const ALL: [Tab; 5] = [
        Tab::General,
        Tab::Audio,
        Tab::Display,
        Tab::Models,
        Tab::Sources,
    ];

    fn label(self) -> &'static str {
        match self {
            Tab::General => "General",
            Tab::Audio => "Audio",
            Tab::Display => "Display",
            Tab::Models => "Models",
            Tab::Sources => "Sound sources",
        }
    }
}

impl Tab {
    /// A small glyph that leads the tab's row, so the list scans by shape.
    fn icon(self) -> &'static str {
        match self {
            Tab::General => "⚙",
            Tab::Audio => "🔊",
            Tab::Display => "👁",
            Tab::Models => "📦",
            Tab::Sources => "🌐",
        }
    }

    /// One line under the tab list's choice, saying what the page is for.
    fn blurb(self) -> &'static str {
        match self {
            Tab::General => "Plugin safety, and taking your settings to another machine.",
            Tab::Audio => "Where the sound goes out, and what plays into Noodle.",
            Tab::Display => "What the graph shows of the sound as it plays, and what that costs.",
            Tab::Models => "The AI models for Create drum rack and Split to stems.",
            Tab::Sources => {
                "Where the Sounds panel gets its sounds. What a source has already offered \
                 stays listed when this computer is offline."
            }
        }
    }

    /// Words the search field matches, besides the label.
    fn keywords(self) -> &'static str {
        match self {
            Tab::General => "plugin sandbox process crash import export settings file",
            Tab::Audio => {
                "output input device latency buffer sample rate host midi keyboard \
                 monitoring channels worker threads bluetooth"
            }
            Tab::Display => {
                "wires meters levels nodes preset performance cpu heat clip flash beat pulse \
                 voice silent fade"
            }
            Tab::Models => "drum stems separation download licence ai model demucs",
            Tab::Sources => "sounds packs folder web source login token download cache clean",
        }
    }

    fn matches(self, query: &str) -> bool {
        let q = query.trim().to_lowercase();
        q.is_empty()
            || self.label().to_lowercase().contains(&q)
            || self.keywords().split(' ').any(|w| w.contains(&q))
    }
}

/// The window never changes size or place: a tab is a page inside it.
const MAX_SIZE: egui::Vec2 = egui::vec2(760.0, 560.0);
/// Room left between the window and the screen's edge.
const MARGIN: f32 = 60.0;
/// The tab list's width. Fixed, so nothing moves when one is hovered.
const TAB_W: f32 = 132.0;
const TAB_H: f32 = 28.0;
/// The heading above and the Close row below the pages.
const HEADER_H: f32 = 40.0;
const FOOTER_H: f32 = 46.0;

/// The size of the Settings window on a screen of `screen`.
fn window_size(screen: egui::Vec2) -> egui::Vec2 {
    egui::vec2(
        MAX_SIZE.x.min((screen.x - MARGIN).max(320.0)),
        MAX_SIZE.y.min((screen.y - MARGIN).max(260.0)),
    )
}

/// One row of the tab list, always [`TAB_W`] by [`TAB_H`]: hover and
/// selection only change its colour.
fn tab_button(ui: &mut egui::Ui, current: &mut Tab, tab: Tab, status: bool) {
    let selected = *current == tab;
    let (rect, response) = ui.allocate_exact_size(egui::vec2(TAB_W, TAB_H), egui::Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::selected(
            egui::WidgetType::SelectableLabel,
            true,
            selected,
            tab.label(),
        )
    });
    let visuals = ui.visuals();
    let (fill, text) = if selected {
        (visuals.selection.bg_fill, visuals.selection.stroke.color)
    } else if response.hovered() {
        (
            visuals.widgets.hovered.weak_bg_fill,
            visuals.strong_text_color(),
        )
    } else {
        (egui::Color32::TRANSPARENT, visuals.text_color())
    };
    ui.painter().rect_filled(rect, 4.0, fill);
    let font = egui::TextStyle::Button.resolve(ui.style());
    ui.painter().text(
        rect.left_center() + egui::vec2(10.0, 0.0),
        egui::Align2::LEFT_CENTER,
        tab.icon(),
        font.clone(),
        text,
    );
    ui.painter().text(
        rect.left_center() + egui::vec2(32.0, 0.0),
        egui::Align2::LEFT_CENTER,
        tab.label(),
        font,
        text,
    );
    if status {
        // A download is running, or a source can't be reached.
        ui.painter().circle_filled(
            rect.right_center() - egui::vec2(10.0, 0.0),
            3.5,
            ui.visuals().warn_fg_color,
        );
    }
    if response.clicked() {
        *current = tab;
    }
}

fn down() -> egui::Layout {
    egui::Layout::top_down(egui::Align::Min)
}

/// What the user did in the window this frame.
#[derive(Default)]
pub struct Outcome {
    /// New audio settings, applied.
    pub audio: Option<AudioConfig>,
    /// A display switch changed.
    pub display_changed: bool,
    /// Import User Settings was pressed.
    pub import: bool,
    /// Export User Settings was pressed.
    pub export: bool,
}

#[derive(Default)]
pub struct Settings {
    open: bool,
    tab: Tab,
    search: String,
}

impl Settings {
    /// Opens the window on `tab`.
    pub fn open(&mut self, tab: Tab) {
        self.open = true;
        self.tab = tab;
        self.search.clear();
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Draws the window while it is open.
    #[allow(clippy::too_many_arguments)]
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        devices: &mut DevicePicker,
        latency: Option<LatencyReport>,
        display: &mut Display,
        session: &mut Session,
        models: &mut crate::models::Models,
        sounds: &mut crate::sounds::Sounds,
    ) -> Outcome {
        let mut out = Outcome::default();
        if !self.open {
            return out;
        }
        let screen = ctx.content_rect().size();
        let size = window_size(screen);
        let modal = egui::Modal::new(egui::Id::new("settings")).show(ctx, |ui| {
            ui.set_min_size(size);
            ui.set_max_size(size);
            ui.heading("Settings");
            ui.add_space(4.0);
            let body_h = size.y - HEADER_H - FOOTER_H;
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                ui.allocate_ui_with_layout(egui::vec2(TAB_W, body_h), down(), |ui| {
                    ui.set_min_size(egui::vec2(TAB_W, body_h));
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.add(
                        egui::TextEdit::singleline(&mut self.search)
                            .hint_text("Search settings")
                            .desired_width(TAB_W - 8.0),
                    );
                    ui.add_space(4.0);
                    let shown: Vec<Tab> = Tab::ALL
                        .into_iter()
                        .filter(|t| t.matches(&self.search))
                        .collect();
                    // The search takes you to the first page that has a match.
                    if let Some(first) = shown.first()
                        && !shown.contains(&self.tab)
                    {
                        self.tab = *first;
                    }
                    for tab in shown {
                        let status = match tab {
                            Tab::Models => models.any_downloading(),
                            Tab::Sources => sounds.has_unreachable(),
                            _ => false,
                        };
                        tab_button(ui, &mut self.tab, tab, status);
                    }
                });
                ui.separator();
                ui.add_space(8.0);
                let page_w = size.x - TAB_W - 17.0;
                ui.allocate_ui_with_layout(egui::vec2(page_w, body_h), down(), |ui| {
                    egui::ScrollArea::vertical()
                        .id_salt("settings page")
                        .max_width(page_w)
                        .max_height(body_h)
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            ui.set_width(page_w - 14.0);
                            ui.weak(self.tab.blurb());
                            ui.add_space(6.0);
                            match self.tab {
                                Tab::General => general(ui, session, &mut out),
                                Tab::Audio => {
                                    out.audio = devices.body(ui, latency, |ui| {
                                        crate::latency::monitoring_row(ui, session)
                                    });
                                }
                                Tab::Display => {
                                    out.display_changed = crate::display::body(ui, display)
                                }
                                Tab::Models => models.body(ui),
                                Tab::Sources => sounds.body(ui),
                            }
                        });
                });
            });
            ui.separator();
            if ui.button("Close").clicked() {
                self.open = false;
            }
        });
        if modal.should_close() {
            self.open = false;
        }
        out
    }
}

fn general(ui: &mut egui::Ui, session: &mut Session, out: &mut Outcome) {
    let mut sandboxed = session.sandbox_plugins();
    if ui
        .checkbox(&mut sandboxed, "Run plugins in separate processes")
        .on_hover_text(
            "A plugin that crashes or hangs then plays silence and is started \
             again, instead of taking the app with it. Costs a little CPU for each \
             plugin. Editors open as before. Kept on this machine only.",
        )
        .changed()
    {
        session.set_sandbox_plugins(sandboxed);
        crate::prefs::remember_sandbox(sandboxed);
    }
    ui.add_space(12.0);
    ui.label(egui::RichText::new("Your settings").strong());
    if let Some(file) = crate::prefs::default_path() {
        show_file_button(ui, "Show file", &file);
    }
    ui.weak(
        "Saved on this machine. Export them to take to another machine or to share; the \
         audio devices and the plugin sandbox stay local.",
    );
    ui.horizontal(|ui| {
        out.import |= ui.button("Import User Settings…").clicked();
        out.export |= ui.button("Export User Settings…").clicked();
    });
}

/// A "Show file" button: opens the folder that holds `path` in the system's
/// file manager. Disabled, with a hint, while the folder doesn't exist yet
/// (the settings file is written on the first change).
pub fn show_file_button(ui: &mut egui::Ui, label: &str, path: &std::path::Path) {
    let folder = if path.is_dir() {
        Some(path)
    } else {
        path.parent().filter(|p| p.is_dir())
    };
    let response = ui
        .add_enabled(folder.is_some(), egui::Button::new(label))
        .on_hover_text(path.display().to_string());
    if response.clicked()
        && let Some(folder) = folder
    {
        ui.ctx().open_url(egui::OpenUrl::new_tab(file_url(folder)));
    }
}

/// A `file://` address for a local path, with the characters a URL can't
/// carry as they are written out as `%XX`.
fn file_url(path: &std::path::Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    let mut url = String::from("file://");
    if !text.starts_with('/') {
        url.push('/');
    }
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' | b':' => {
                url.push(byte as char);
            }
            other => url.push_str(&format!("%{other:02X}")),
        }
    }
    url
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_becomes_a_file_address() {
        assert_eq!(
            file_url(std::path::Path::new(
                "/Users/a b/Library/Application Support/Noodle"
            )),
            "file:///Users/a%20b/Library/Application%20Support/Noodle"
        );
        assert_eq!(
            file_url(std::path::Path::new("C:\\Users\\me\\Noodle")),
            "file:///C:/Users/me/Noodle"
        );
    }
}
