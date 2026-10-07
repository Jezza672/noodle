//! The look of the app, kept in one place so it can be swapped wholesale.
//!
//! For now, a placeholder dark theme modelled on Blender's: dense,
//! low-contrast greys, with blue for selection. The final design direction is
//! still being chosen, so views should take colours, sizes and spacing from
//! here rather than hard-coding them.

use egui::{Color32, CornerRadius, Stroke, Visuals};

/// The canvas behind the node editor.
pub const CANVAS: Color32 = Color32::from_gray(29);
/// Panels and headers.
pub const PANEL: Color32 = Color32::from_gray(48);
/// Selected items, and text selection.
pub const SELECTED: Color32 = Color32::from_rgb(71, 114, 179);
/// The properties panel's starting width.
pub const PROPERTIES_WIDTH: f32 = 260.0;

pub fn apply(ctx: &egui::Context) {
    ctx.set_visuals(visuals());
    ctx.global_style_mut(|style| {
        style.spacing.item_spacing = egui::vec2(6.0, 4.0);
        style.spacing.button_padding = egui::vec2(6.0, 2.0);
        style.spacing.interact_size.y = 20.0;
    });
}

pub fn visuals() -> Visuals {
    let mut visuals = Visuals::dark();
    let radius = CornerRadius::same(4);
    visuals.panel_fill = PANEL;
    visuals.window_fill = Color32::from_gray(40);
    visuals.extreme_bg_color = Color32::from_gray(24);
    visuals.faint_bg_color = Color32::from_gray(43);
    visuals.window_corner_radius = radius;
    visuals.menu_corner_radius = radius;
    visuals.selection.bg_fill = SELECTED;
    visuals.selection.stroke = Stroke::new(1.0, Color32::WHITE);

    let widgets = &mut visuals.widgets;
    widgets.noninteractive.bg_fill = PANEL;
    widgets.noninteractive.bg_stroke = Stroke::new(1.0, Color32::from_gray(36));
    widgets.noninteractive.fg_stroke = Stroke::new(1.0, Color32::from_gray(200));
    for (state, fill) in [
        (&mut widgets.inactive, 84),
        (&mut widgets.hovered, 101),
        (&mut widgets.active, 120),
        (&mut widgets.open, 84),
    ] {
        state.bg_fill = Color32::from_gray(fill);
        state.weak_bg_fill = Color32::from_gray(fill);
        state.corner_radius = radius;
        state.fg_stroke = Stroke::new(1.0, Color32::from_gray(230));
    }
    widgets.inactive.bg_stroke = Stroke::new(1.0, Color32::from_gray(61));
    widgets.hovered.bg_stroke = Stroke::new(1.0, Color32::from_gray(80));
    widgets.active.bg_stroke = Stroke::new(1.0, SELECTED);
    visuals
}

/// The node editor's colours.
pub mod editor {
    use egui::Color32;

    pub const GRID: Color32 = Color32::from_gray(36);
    pub const GRID_MAJOR: Color32 = Color32::from_gray(44);
    pub const NODE: Color32 = Color32::from_gray(48);
    pub const NODE_OUTLINE: Color32 = Color32::from_gray(20);
    pub const TEXT: Color32 = Color32::from_gray(225);
    pub const TEXT_WEAK: Color32 = Color32::from_gray(150);
    /// A selected node's outline, Blender's orange.
    pub const SELECTED: Color32 = Color32::from_rgb(237, 135, 37);
    /// The active node's outline: the one the properties panel shows.
    pub const ACTIVE: Color32 = Color32::from_rgb(255, 255, 255);
    pub const PROBLEM: Color32 = Color32::from_rgb(230, 70, 60);
    pub const WIRE: Color32 = Color32::from_gray(150);
    /// Wires to and from selected nodes.
    pub const WIRE_SELECTED: Color32 = Color32::from_gray(230);
    pub const EVENT_WIRE: Color32 = Color32::from_rgb(200, 110, 200);
    pub const AUDIO_SOCKET: Color32 = Color32::from_rgb(99, 199, 255);
    pub const PARAM_SOCKET: Color32 = Color32::from_rgb(161, 161, 161);
    pub const EVENT_SOCKET: Color32 = Color32::from_rgb(204, 102, 204);
    pub const FRAME: Color32 = Color32::from_rgba_premultiplied(60, 60, 60, 120);
    pub const BOX_SELECT: Color32 = Color32::from_rgba_premultiplied(40, 40, 40, 40);
    pub const CUT: Color32 = Color32::from_rgb(230, 70, 60);

    /// Header colours by node category, like Blender's.
    pub fn header(category: &str) -> Color32 {
        match category {
            "Generators" => Color32::from_rgb(40, 110, 100),
            "Filters" => Color32::from_rgb(90, 60, 130),
            "Utilities" => Color32::from_rgb(50, 80, 120),
            "Input/Output" => Color32::from_rgb(130, 50, 50),
            "Polyphony" => Color32::from_rgb(120, 90, 40),
            "Offline" => Color32::from_rgb(90, 90, 40),
            _ => Color32::from_gray(70),
        }
    }
}
