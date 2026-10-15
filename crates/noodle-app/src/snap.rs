//! The snap setting: how fine a grid clips, notes and markers snap to. One
//! setting for the arrangement, the piano roll and the clip editor, set from
//! the menu bar.

use egui::Ui;

/// The steps on offer, in ticks. 1 is "Off": no snapping.
pub const STEPS: [(&str, i64); 6] = [
    ("Beat", 960),
    ("1/2 beat", 480),
    ("1/4 beat", 240),
    ("1/8 beat", 120),
    ("1/16 beat", 60),
    ("Off", 1),
];

/// What a new session snaps to.
pub const DEFAULT: i64 = 960;

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
        assert_eq!(label(240), "1/4 beat");
        assert_eq!(label(7), "Custom");
    }
}
