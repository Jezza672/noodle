//! Pieces of the top bar: the arrow drawn on a drop-down, a split button (a
//! button with its options arrow flush against it), a row of choices for a
//! popover, and the position field.
//!
//! Arrows are painted, not typed: the bundled font has no glyph for them, and
//! a typed one shows as an empty box.

use egui::{Color32, DragValue, Pos2, Response, Sense, Stroke, StrokeKind, Ui, vec2};
use noodle_core::{TICKS_PER_QUARTER, TempoMap, Tick};

use crate::theme;

/// A small downward arrow centred on `centre`.
pub fn paint_chevron(ui: &Ui, centre: Pos2, colour: Color32) {
    let s = 3.0;
    ui.painter().add(egui::Shape::line(
        vec![
            centre + vec2(-s, -s * 0.5),
            centre + vec2(0.0, s * 0.5),
            centre + vec2(s, -s * 0.5),
        ],
        Stroke::new(1.4, colour),
    ));
}

/// The computer-keyboard toggle: a keyboard drawn large enough to read as
/// one (the typed glyph is too small), lit while on.
pub fn keyboard_toggle(ui: &mut Ui, on: bool) -> Response {
    let size = vec2(40.0, ui.spacing().interact_size.y.max(22.0));
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Button, true, on, "Computer keyboard")
    });
    if ui.is_rect_visible(rect) {
        let visuals = ui.style().interact_selectable(&response, on);
        ui.painter()
            .rect_filled(rect, egui::CornerRadius::same(4), visuals.weak_bg_fill);
        let colour = if on {
            theme::ACCENT
        } else {
            theme::timeline::TEXT
        };
        let painter = ui.painter();
        let body = egui::Rect::from_center_size(rect.center(), vec2(30.0, 15.0));
        painter.rect_stroke(
            body,
            egui::CornerRadius::same(2),
            Stroke::new(1.2, colour),
            StrokeKind::Inside,
        );
        // Two rows of keys and a space bar.
        let left = body.left() + 3.5;
        for (row, count, indent) in [(0.0, 7, 0.0), (1.0, 6, 1.7)] {
            for key in 0..count {
                let at = egui::pos2(
                    left + indent + key as f32 * 3.6,
                    body.top() + 3.0 + row * 3.6,
                );
                painter.rect_filled(
                    egui::Rect::from_min_size(at, vec2(2.4, 2.4)),
                    egui::CornerRadius::same(1),
                    colour,
                );
            }
        }
        painter.rect_filled(
            egui::Rect::from_center_size(
                egui::pos2(body.center().x, body.bottom() - 3.4),
                vec2(14.0, 2.2),
            ),
            egui::CornerRadius::same(1),
            colour,
        );
    }
    response
}

/// A button and its options arrow, joined by one outline so the arrow reads
/// as part of the button. `main` draws the button; the arrow comes back as a
/// second response, to anchor the popover on. `label` names the arrow for
/// accessibility and tests.
pub fn split_button(
    ui: &mut Ui,
    label: &str,
    main: impl FnOnce(&mut Ui) -> Response,
) -> (Response, Response) {
    let (main, arrow) = ui
        .scope_builder(
            egui::UiBuilder::new().layout(egui::Layout::left_to_right(egui::Align::Center)),
            |ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                let main = main(ui);
                let size = vec2(15.0, main.rect.height().max(ui.spacing().interact_size.y));
                let (rect, arrow) = ui.allocate_exact_size(size, Sense::click());
                if arrow.hovered() || arrow.is_pointer_button_down_on() {
                    ui.painter().rect_filled(
                        rect,
                        egui::CornerRadius::same(4),
                        ui.visuals().widgets.hovered.weak_bg_fill,
                    );
                }
                let colour = ui.style().interact(&arrow).fg_stroke.color;
                paint_chevron(ui, rect.center(), colour);
                (main, arrow)
            },
        )
        .inner;
    arrow.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label));
    ui.painter().rect_stroke(
        main.rect.union(arrow.rect),
        egui::CornerRadius::same(4),
        Stroke::new(1.0, theme::TRANSPORT_OUTLINE),
        StrokeKind::Inside,
    );
    (main, arrow)
}

/// A row of choices where the current one is lit, each with a hover. Returns
/// the one clicked.
pub fn segmented<T: Copy + PartialEq>(
    ui: &mut Ui,
    choices: &[(&str, T, &str)],
    current: T,
) -> Option<T> {
    let mut picked = None;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        for (label, value, tip) in choices {
            if ui
                .selectable_label(*value == current, *label)
                .on_hover_text(*tip)
                .clicked()
            {
                picked = Some(*value);
            }
        }
    });
    picked
}

/// A setting: its name, then its choices.
pub fn setting<T: Copy + PartialEq>(
    ui: &mut Ui,
    name: &str,
    choices: &[(&str, T, &str)],
    current: T,
) -> Option<T> {
    ui.label(egui::RichText::new(name).weak());
    segmented(ui, choices, current)
}

/// How the position reads: bar.beat.tick, counting bars and beats from 1.
pub fn format_position(map: &TempoMap, tick: Tick) -> String {
    let at = map.position(tick);
    format!("{}.{}.{:03}", at.bar + 1, at.beat + 1, at.tick)
}

/// The tick a typed position stands for: `5`, `5.2` or `5.2.480` (bar, beat,
/// tick). `None` for what is not a place in the music: bar 0, a beat past the
/// bar's last, a tick past the beat's end.
pub fn parse_position(map: &TempoMap, text: &str) -> Option<Tick> {
    let mut parts = text
        .trim()
        .split(|c: char| c == '.' || c == ':' || c.is_whitespace())
        .filter(|p| !p.is_empty());
    let bar: u32 = parts.next()?.parse().ok()?;
    let beat: u32 = parts.next().map_or(Some(1), |p| p.parse().ok())?;
    let tick: i64 = parts.next().map_or(Some(0), |p| p.parse().ok())?;
    if parts.next().is_some() || bar == 0 || beat == 0 || tick < 0 {
        return None;
    }
    let bar0 = bar - 1;
    let signature = map
        .signatures()
        .iter()
        .rev()
        .find(|change| change.bar <= bar0)?
        .signature;
    if beat > u32::from(signature.numerator) || tick >= signature.ticks_per_beat() {
        return None;
    }
    Some(Tick(
        map.bar_start(bar0).0 + i64::from(beat - 1) * signature.ticks_per_beat() + tick,
    ))
}

/// The finest step a drag of the position field lands on.
const SCRUB_STEP: i64 = TICKS_PER_QUARTER / 16;

/// The position as a field: drag it sideways to scrub, or click it and type a
/// bar.beat.tick. Returns the tick asked for.
pub fn position_field(ui: &mut Ui, map: &TempoMap, at: Tick) -> Option<Tick> {
    let mut value = at.0 as f64;
    let for_format = map.clone();
    let for_parse = map.clone();
    let response = ui
        .add_sized(
            vec2(96.0, ui.spacing().interact_size.y),
            DragValue::new(&mut value)
                .range(0.0..=1.0e9)
                // A beat for every dozen pixels.
                .speed(TICKS_PER_QUARTER as f64 / 12.0)
                .update_while_editing(false)
                .custom_formatter(move |v, _| format_position(&for_format, Tick(v.round() as i64)))
                .custom_parser(move |text| {
                    parse_position(&for_parse, text).map(|tick| tick.0 as f64)
                }),
        )
        .on_hover_text(
            "Bar.beat.tick. Drag to move the playhead, or click and type a position, \
             such as 5.2 for bar 5 beat 2",
        );
    response
        .widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::DragValue, true, "Position"));
    if !response.changed() {
        return None;
    }
    let mut tick = value.round() as i64;
    let dragged = response.dragged();
    // Shift is the fine drag, which lands on any tick.
    if dragged && !ui.input(|i| i.modifiers.shift) {
        tick = (tick + SCRUB_STEP / 2) / SCRUB_STEP * SCRUB_STEP;
    }
    Some(Tick(tick.max(0)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_core::{SignatureChange, TempoChange, TimeSignature};

    fn map() -> TempoMap {
        TempoMap::new(
            vec![TempoChange {
                tick: Tick(0),
                bpm: 120.0,
            }],
            vec![
                SignatureChange {
                    bar: 0,
                    signature: TimeSignature::COMMON,
                },
                SignatureChange {
                    bar: 2,
                    signature: TimeSignature {
                        numerator: 3,
                        denominator: 4,
                    },
                },
            ],
        )
        .unwrap()
    }

    #[test]
    fn a_typed_position_is_the_tick_it_is_shown_as() {
        let map = map();
        for tick in [
            0,
            1,
            TICKS_PER_QUARTER * 3 + 77,
            8 * TICKS_PER_QUARTER,
            11 * TICKS_PER_QUARTER + 5,
        ] {
            let text = format_position(&map, Tick(tick));
            assert_eq!(parse_position(&map, &text), Some(Tick(tick)), "{text}");
        }
    }

    #[test]
    fn short_forms_fill_in_the_rest() {
        let map = map();
        assert_eq!(parse_position(&map, "1"), Some(Tick(0)));
        assert_eq!(parse_position(&map, "2"), Some(Tick(4 * TICKS_PER_QUARTER)));
        assert_eq!(
            parse_position(&map, "2 3"),
            Some(Tick(6 * TICKS_PER_QUARTER))
        );
    }

    #[test]
    fn places_that_are_not_in_the_music_are_refused() {
        let map = map();
        assert_eq!(parse_position(&map, "0"), None);
        assert_eq!(parse_position(&map, "1.5"), None, "bar 1 has four beats");
        assert_eq!(parse_position(&map, "3.4"), None, "bar 3 is in three");
        assert_eq!(
            parse_position(&map, "3.3"),
            Some(Tick(10 * TICKS_PER_QUARTER))
        );
        assert_eq!(parse_position(&map, "1.1.960"), None);
        assert_eq!(parse_position(&map, "abc"), None);
        assert_eq!(parse_position(&map, "1.1.1.1"), None);
    }
}
