//! The Settings window: one place, with a tab each for the General
//! preferences, the Audio devices, the Display switches, the separation
//! Models and the Sources of sounds. Everything in it
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
            Tab::Sources => "Sources",
        }
    }
}

/// The window never changes size or place: a tab is a page inside it.
const MAX_SIZE: egui::Vec2 = egui::vec2(760.0, 560.0);
/// Room left between the window and the screen's edge.
const MARGIN: f32 = 60.0;
/// The tab list's width. Fixed, so nothing moves when one is hovered.
const TAB_W: f32 = 120.0;
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
fn tab_button(ui: &mut egui::Ui, current: &mut Tab, tab: Tab) {
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
    ui.painter().text(
        rect.left_center() + egui::vec2(10.0, 0.0),
        egui::Align2::LEFT_CENTER,
        tab.label(),
        egui::TextStyle::Button.resolve(ui.style()),
        text,
    );
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
}

impl Settings {
    /// Opens the window on `tab`.
    pub fn open(&mut self, tab: Tab) {
        self.open = true;
        self.tab = tab;
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
                    for tab in Tab::ALL {
                        tab_button(ui, &mut self.tab, tab);
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
                            match self.tab {
                                Tab::General => general(ui, session, &mut out),
                                Tab::Audio => {
                                    crate::latency::monitoring_row(ui, session);
                                    ui.add_space(6.0);
                                    out.audio = devices.body(ui, latency);
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
    ui.add_space(8.0);
    ui.weak(
        "Your settings are saved on this machine. Export them to take to another machine \
         or to share; the audio devices and the plugin sandbox stay local.",
    );
    ui.horizontal(|ui| {
        out.import |= ui.button("Import User Settings…").clicked();
        out.export |= ui.button("Export User Settings…").clicked();
    });
}
