//! The snap setting: how fine a grid clips, notes and markers snap to. One
//! setting for the arrangement, the piano roll and the clip editor, set from
//! the menu bar.

use egui::Ui;
use noodle_core::TICKS_PER_QUARTER;

/// The steps on offer, in ticks. 1 is "Off": no snapping.
pub const STEPS: [(&str, i64); 6] = [
    ("Beat", TICKS_PER_QUARTER),
    ("1/2 beat", TICKS_PER_QUARTER / 2),
    ("1/4 beat", TICKS_PER_QUARTER / 4),
    ("1/8 beat", TICKS_PER_QUARTER / 8),
    ("1/16 beat", TICKS_PER_QUARTER / 16),
    ("Off", 1),
];

/// What a new session snaps to.
pub const DEFAULT: i64 = TICKS_PER_QUARTER;

/// The name of a step, or "Custom" for a tick count not on offer.
pub fn label(ticks: i64) -> &'static str {
    STEPS
        .iter()
        .find(|(_, step)| *step == ticks)
        .map_or("Custom", |(name, _)| *name)
}

/// The snap menu: a button that opens the list. Says the step picked, if one
/// was. (A menu rather than a combo box, which is the same on every platform.)
pub fn menu(ui: &mut Ui, current: i64) -> Option<i64> {
    let mut picked = None;
    ui.menu_button(format!("Snap: {}", label(current)), |ui| {
        for (name, step) in STEPS {
            if ui.selectable_label(step == current, name).clicked() {
                picked = Some(step);
                ui.close();
            }
        }
    });
    picked
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_are_named() {
        assert_eq!(label(TICKS_PER_QUARTER / 4), "1/4 beat");
        assert_eq!(label(7), "Custom");
    }
}
