//! The look of the app, kept in one place so it can be swapped wholesale.
//!
//! The "Studio" direction from the design workshop: a cool graphite ground,
//! rounded corners, a yellow accent for what is playing or selected, and
//! signal types told apart by port colour and shape. Views should take
//! colours, sizes and spacing from here rather than hard-coding them.

use egui::{Color32, CornerRadius, Stroke, Visuals};

/// The canvas behind the node editor.
pub const CANVAS: Color32 = Color32::from_rgb(26, 26, 29);
/// Panels and headers.
pub const PANEL: Color32 = Color32::from_rgb(28, 28, 31);
/// The accent: the playhead, the play button, focus.
pub const ACCENT: Color32 = Color32::from_rgb(245, 208, 74);
/// Selected items, and text selection.
pub const SELECTED: Color32 = Color32::from_rgb(70, 86, 140);
/// Mute, solo and record.
pub const MUTE: Color32 = Color32::from_rgb(255, 122, 107);
pub const SOLO: Color32 = ACCENT;
pub const RECORD: Color32 = Color32::from_rgb(255, 90, 79);
/// Corner radius for panels, buttons and nodes.
pub const RADIUS: u8 = 6;
/// The properties panel's starting width.
pub const PROPERTIES_WIDTH: f32 = 260.0;

pub fn apply(ctx: &egui::Context) {
    ctx.set_visuals(visuals());
    ctx.global_style_mut(|style| {
        style.spacing.item_spacing = egui::vec2(8.0, 5.0);
        style.spacing.button_padding = egui::vec2(8.0, 3.0);
        style.spacing.interact_size.y = 22.0;
    });
}

pub fn visuals() -> Visuals {
    let mut visuals = Visuals::dark();
    let radius = CornerRadius::same(RADIUS);
    visuals.panel_fill = PANEL;
    visuals.window_fill = Color32::from_rgb(35, 35, 39);
    visuals.window_stroke = Stroke::new(1.0, Color32::from_rgb(51, 51, 56));
    visuals.extreme_bg_color = Color32::from_rgb(15, 15, 17);
    visuals.faint_bg_color = Color32::from_rgb(35, 35, 39);
    visuals.window_corner_radius = CornerRadius::same(RADIUS + 2);
    visuals.menu_corner_radius = radius;
    visuals.selection.bg_fill = SELECTED;
    visuals.selection.stroke = Stroke::new(1.0, Color32::from_rgb(232, 232, 234));
    visuals.hyperlink_color = ACCENT;

    let widgets = &mut visuals.widgets;
    widgets.noninteractive.bg_fill = PANEL;
    widgets.noninteractive.bg_stroke = Stroke::new(1.0, Color32::from_rgb(44, 44, 49));
    widgets.noninteractive.fg_stroke = Stroke::new(1.0, Color32::from_rgb(201, 201, 207));
    widgets.noninteractive.corner_radius = radius;
    for (state, fill) in [
        (&mut widgets.inactive, Color32::from_rgb(42, 42, 46)),
        (&mut widgets.hovered, Color32::from_rgb(58, 58, 64)),
        (&mut widgets.active, Color32::from_rgb(75, 75, 85)),
        (&mut widgets.open, Color32::from_rgb(52, 52, 58)),
    ] {
        state.bg_fill = fill;
        state.weak_bg_fill = fill;
        state.corner_radius = radius;
        state.fg_stroke = Stroke::new(1.0, Color32::from_rgb(232, 232, 234));
    }
    widgets.inactive.bg_stroke = Stroke::new(1.0, Color32::from_rgb(58, 58, 64));
    widgets.hovered.bg_stroke = Stroke::new(1.0, Color32::from_rgb(85, 85, 94));
    widgets.active.bg_stroke = Stroke::new(1.0, ACCENT);
    visuals
}

/// The node editor's colours.
pub mod editor {
    use egui::Color32;

    pub const GRID: Color32 = Color32::from_rgb(33, 33, 37);
    pub const GRID_MAJOR: Color32 = Color32::from_rgb(40, 40, 45);
    pub const NODE: Color32 = Color32::from_rgb(38, 38, 43);
    pub const NODE_OUTLINE: Color32 = Color32::from_rgb(54, 54, 61);
    pub const TEXT: Color32 = Color32::from_rgb(232, 232, 234);
    pub const TEXT_WEAK: Color32 = Color32::from_rgb(161, 161, 168);
    /// A selected node's outline: the accent yellow.
    pub const SELECTED: Color32 = Color32::from_rgb(245, 208, 74);
    /// The active node's outline: the one the properties panel shows.
    pub const ACTIVE: Color32 = Color32::from_rgb(255, 255, 255);
    pub const PROBLEM: Color32 = Color32::from_rgb(255, 90, 79);
    pub const WIRE: Color32 = Color32::from_rgb(120, 140, 160);
    /// Wires to and from selected nodes.
    pub const WIRE_SELECTED: Color32 = Color32::from_rgb(245, 208, 74);
    pub const EVENT_WIRE: Color32 = Color32::from_rgb(240, 160, 64);
    pub const AUDIO_SOCKET: Color32 = Color32::from_rgb(92, 200, 230);
    pub const PARAM_SOCKET: Color32 = Color32::from_rgb(138, 138, 147);
    pub const EVENT_SOCKET: Color32 = Color32::from_rgb(240, 160, 64);
    pub const FRAME: Color32 = Color32::from_rgba_premultiplied(60, 60, 60, 120);
    pub const BOX_SELECT: Color32 = Color32::from_rgba_premultiplied(40, 40, 40, 40);
    pub const CUT: Color32 = Color32::from_rgb(255, 90, 79);
    /// Behind a node's body, such as a meter or scope.
    pub const BODY: Color32 = Color32::from_rgb(22, 22, 25);
    pub const METER_RMS: Color32 = Color32::from_rgb(91, 209, 138);
    /// The part of a meter's bar between the RMS level and the peak.
    pub const METER_PEAK: Color32 = Color32::from_rgb(40, 95, 55);
    /// The held peak, once it's above 0 dB.
    pub const METER_OVER: Color32 = Color32::from_rgb(255, 90, 79);
    pub const SCOPE_TRACE: Color32 = Color32::from_rgb(92, 200, 230);
    pub const SCOPE_AXIS: Color32 = Color32::from_rgb(52, 52, 58);

    /// Header colours by node category: muted, so the ports stand out.
    pub fn header(category: &str) -> Color32 {
        match category {
            "Generators" => Color32::from_rgb(47, 95, 99),
            "Filters" => Color32::from_rgb(58, 74, 120),
            "Utilities" => Color32::from_rgb(61, 61, 70),
            "Input/Output" => Color32::from_rgb(120, 56, 60),
            "Polyphony" => Color32::from_rgb(112, 88, 48),
            "Offline" => Color32::from_rgb(47, 79, 99),
            "Group" => Color32::from_rgb(61, 61, 70),
            "Views" => Color32::from_rgb(47, 95, 70),
            _ => Color32::from_rgb(61, 61, 70),
        }
    }
}

/// The arrangement view's colours and sizes.
pub mod timeline {
    use egui::Color32;

    /// The track headers' width, and the ruler's and a lane's height.
    pub const HEADER_WIDTH: f32 = 150.0;
    pub const RULER_HEIGHT: f32 = 22.0;
    pub const LANE_HEIGHT: f32 = 64.0;
    /// The height the arrangement starts at, above the node editor.
    pub const DEFAULT_HEIGHT: f32 = 240.0;

    pub const BACKGROUND: Color32 = Color32::from_rgb(22, 22, 24);
    pub const LANE_EVEN: Color32 = Color32::from_rgb(28, 28, 31);
    pub const LANE_ODD: Color32 = Color32::from_rgb(31, 31, 34);
    pub const HEADER: Color32 = Color32::from_rgb(35, 35, 39);
    pub const RULER: Color32 = Color32::from_rgb(31, 31, 34);
    pub const BAR_LINE: Color32 = Color32::from_rgb(58, 58, 64);
    pub const BEAT_LINE: Color32 = Color32::from_rgb(38, 38, 43);
    pub const TEXT: Color32 = Color32::from_rgb(232, 232, 234);
    pub const TEXT_WEAK: Color32 = Color32::from_rgb(145, 145, 153);
    pub const PLAYHEAD: Color32 = super::ACCENT;
    pub const SELECTED: Color32 = Color32::WHITE;
    pub const MISSING: Color32 = Color32::from_rgb(255, 90, 79);
    /// An automation lane's row, its header and its curve.
    pub const AUTOMATION: Color32 = Color32::from_rgb(34, 34, 38);
    pub const AUTOMATION_HEADER: Color32 = Color32::from_rgb(42, 42, 47);
    pub const AUTOMATION_LINE: Color32 = Color32::from_rgb(92, 200, 230);

    /// A track's clip colour, spread around the colour wheel so neighbouring
    /// tracks differ.
    pub fn track_colour(index: usize) -> Color32 {
        let hue = (index as f32 * 0.17 + 0.52).fract();
        egui::ecolor::Hsva::new(hue, 0.45, 0.68, 1.0).into()
    }
}
