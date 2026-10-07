//! A dark theme modelled on Blender's: dense, low-contrast greys, with blue
//! for selection.

use egui::{Color32, CornerRadius, Stroke, Visuals};

/// The canvas behind the node editor.
pub const CANVAS: Color32 = Color32::from_gray(29);
/// Panels and headers.
pub const PANEL: Color32 = Color32::from_gray(48);
/// Selected items, and text selection.
pub const SELECTED: Color32 = Color32::from_rgb(71, 114, 179);

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
