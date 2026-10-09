//! The look of the app, kept in one place so it can be swapped wholesale.
//!
//! The "Canvas" direction from the design workshop: a blue-black ground,
//! floating rounded cards, a lime accent for what is playing or selected,
//! and signal types told apart by port colour and shape. Views should take
//! colours, sizes and spacing from here rather than hard-coding them.

use egui::{Color32, CornerRadius, Stroke, Visuals};

/// The canvas behind the node editor.
pub const CANVAS: Color32 = Color32::from_rgb(14, 16, 20);
/// Panels and headers.
pub const PANEL: Color32 = Color32::from_rgb(23, 26, 32);
/// The accent: the playhead, the play button, focus.
pub const ACCENT: Color32 = Color32::from_rgb(200, 241, 105);
/// Selected items, and text selection.
pub const SELECTED: Color32 = Color32::from_rgb(60, 90, 168);
/// Mute, solo and record.
pub const MUTE: Color32 = Color32::from_rgb(255, 122, 107);
pub const SOLO: Color32 = ACCENT;
pub const RECORD: Color32 = Color32::from_rgb(255, 90, 79);
/// Corner radius for panels, buttons and nodes.
pub const RADIUS: u8 = 8;
/// Corner radius of a node.
pub const NODE_RADIUS: u8 = 10;
/// The transport pill: its fill, outline and corner radius.
pub const TRANSPORT_FILL: Color32 = Color32::from_rgb(15, 18, 22);
pub const TRANSPORT_OUTLINE: Color32 = Color32::from_rgb(52, 58, 71);
pub const PILL_RADIUS: u8 = 14;
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
    visuals.window_fill = Color32::from_rgb(29, 33, 42);
    visuals.window_stroke = Stroke::new(1.0, Color32::from_rgb(52, 58, 71));
    visuals.extreme_bg_color = Color32::from_rgb(15, 18, 22);
    visuals.faint_bg_color = Color32::from_rgb(27, 31, 39);
    visuals.window_corner_radius = CornerRadius::same(RADIUS + 2);
    visuals.menu_corner_radius = radius;
    visuals.selection.bg_fill = SELECTED;
    visuals.selection.stroke = Stroke::new(1.0, Color32::from_rgb(233, 235, 240));
    visuals.hyperlink_color = ACCENT;

    let widgets = &mut visuals.widgets;
    widgets.noninteractive.bg_fill = PANEL;
    widgets.noninteractive.bg_stroke = Stroke::new(1.0, Color32::from_rgb(42, 47, 57));
    widgets.noninteractive.fg_stroke = Stroke::new(1.0, Color32::from_rgb(170, 177, 192));
    widgets.noninteractive.corner_radius = radius;
    for (state, fill) in [
        (&mut widgets.inactive, Color32::from_rgb(36, 41, 51)),
        (&mut widgets.hovered, Color32::from_rgb(48, 54, 66)),
        (&mut widgets.active, Color32::from_rgb(60, 68, 84)),
        (&mut widgets.open, Color32::from_rgb(44, 50, 64)),
    ] {
        state.bg_fill = fill;
        state.weak_bg_fill = fill;
        state.corner_radius = radius;
        state.fg_stroke = Stroke::new(1.0, Color32::from_rgb(233, 235, 240));
    }
    widgets.inactive.bg_stroke = Stroke::new(1.0, Color32::from_rgb(52, 58, 71));
    widgets.hovered.bg_stroke = Stroke::new(1.0, Color32::from_rgb(78, 86, 104));
    widgets.active.bg_stroke = Stroke::new(1.0, ACCENT);
    visuals
}

/// The node editor's colours.
pub mod editor {
    use egui::Color32;

    pub const GRID: Color32 = Color32::from_rgb(24, 27, 33);
    pub const GRID_MAJOR: Color32 = Color32::from_rgb(38, 43, 53);
    pub const NODE: Color32 = Color32::from_rgb(27, 31, 39);
    /// A node's title row, a shade lighter than its body.
    /// The dark core of a wire, matching the canvas.
    pub const WIRE_CORE: Color32 = super::CANVAS;
    pub const NODE_HEADER: Color32 = Color32::from_rgb(34, 39, 49);
    pub const NODE_OUTLINE: Color32 = Color32::from_rgb(44, 50, 62);
    pub const TEXT: Color32 = Color32::from_rgb(233, 235, 240);
    pub const TEXT_WEAK: Color32 = Color32::from_rgb(138, 145, 161);
    /// A selected node's outline: the accent.
    pub const SELECTED: Color32 = Color32::from_rgb(200, 241, 105);
    /// The active node's outline: the one the properties panel shows.
    pub const ACTIVE: Color32 = Color32::from_rgb(255, 255, 255);
    pub const PROBLEM: Color32 = Color32::from_rgb(255, 90, 79);
    pub const WIRE: Color32 = Color32::from_rgb(92, 200, 230);
    /// Wires to and from selected nodes.
    pub const WIRE_SELECTED: Color32 = Color32::from_rgb(200, 241, 105);
    pub const EVENT_WIRE: Color32 = Color32::from_rgb(240, 160, 64);
    pub const AUDIO_SOCKET: Color32 = Color32::from_rgb(92, 200, 230);
    pub const PARAM_SOCKET: Color32 = Color32::from_rgb(138, 138, 147);
    pub const EVENT_SOCKET: Color32 = Color32::from_rgb(240, 160, 64);
    /// Frames are barely tinted; the outline carries them.
    pub const FRAME: Color32 = Color32::from_rgba_premultiplied(6, 9, 16, 20);
    /// The border around a frame.
    pub const FRAME_OUTLINE: Color32 = super::SELECTED;
    pub const BOX_SELECT: Color32 = Color32::from_rgba_premultiplied(40, 40, 40, 40);
    pub const CUT: Color32 = Color32::from_rgb(255, 90, 79);
    /// Behind a node's body, such as a meter or scope.
    pub const BODY: Color32 = Color32::from_rgb(15, 18, 22);
    pub const METER_RMS: Color32 = Color32::from_rgb(200, 241, 105);
    /// The part of a meter's bar between the RMS level and the peak.
    pub const METER_PEAK: Color32 = Color32::from_rgb(80, 96, 42);
    /// The held peak, once it's above 0 dB.
    pub const METER_OVER: Color32 = Color32::from_rgb(255, 90, 79);
    pub const SCOPE_TRACE: Color32 = Color32::from_rgb(92, 200, 230);
    pub const SCOPE_AXIS: Color32 = Color32::from_rgb(42, 47, 57);

    /// The dot beside a node's title, by category.
    pub fn header(category: &str) -> Color32 {
        match category {
            "Generators" => Color32::from_rgb(91, 209, 138),
            "Filters" => Color32::from_rgb(109, 140, 255),
            "Utilities" => Color32::from_rgb(138, 145, 161),
            "Input/Output" => Color32::from_rgb(255, 122, 107),
            "Polyphony" => Color32::from_rgb(240, 160, 64),
            "Offline" => Color32::from_rgb(79, 183, 189),
            "Group" => Color32::from_rgb(233, 235, 240),
            "Views" => Color32::from_rgb(197, 140, 255),
            _ => Color32::from_rgb(138, 145, 161),
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

    pub const BACKGROUND: Color32 = Color32::from_rgb(22, 25, 32);
    pub const LANE_EVEN: Color32 = Color32::from_rgb(25, 28, 35);
    pub const LANE_ODD: Color32 = Color32::from_rgb(27, 30, 38);
    pub const HEADER: Color32 = Color32::from_rgb(29, 33, 42);
    pub const RULER: Color32 = Color32::from_rgb(22, 25, 32);
    pub const BAR_LINE: Color32 = Color32::from_rgb(52, 58, 71);
    pub const BEAT_LINE: Color32 = Color32::from_rgb(32, 36, 44);
    pub const TEXT: Color32 = Color32::from_rgb(233, 235, 240);
    pub const TEXT_WEAK: Color32 = Color32::from_rgb(138, 145, 161);
    pub const PLAYHEAD: Color32 = super::ACCENT;
    pub const SELECTED: Color32 = Color32::WHITE;
    pub const MISSING: Color32 = Color32::from_rgb(255, 90, 79);
    /// An automation lane's row, its header and its curve.
    pub const AUTOMATION: Color32 = Color32::from_rgb(30, 34, 42);
    pub const AUTOMATION_HEADER: Color32 = Color32::from_rgb(36, 41, 51);
    pub const AUTOMATION_LINE: Color32 = Color32::from_rgb(92, 200, 230);

    /// A track's clip colour: the Canvas palette, then spread around the wheel.
    pub fn track_colour(index: usize) -> Color32 {
        const PALETTE: [Color32; 6] = [
            Color32::from_rgb(255, 122, 107),
            Color32::from_rgb(109, 140, 255),
            Color32::from_rgb(197, 140, 255),
            Color32::from_rgb(79, 183, 189),
            Color32::from_rgb(240, 160, 64),
            Color32::from_rgb(91, 209, 138),
        ];
        if let Some(&c) = PALETTE.get(index) {
            return c;
        }
        let hue = (index as f32 * 0.17 + 0.52).fract();
        egui::ecolor::Hsva::new(hue, 0.5, 0.85, 1.0).into()
    }
}
