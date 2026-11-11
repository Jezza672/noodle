//! Group E of the scorecard, part one: how does each action's entry point
//! show itself?
//!
//! Every action in a route's log is put in one surface:
//!
//! | Surface | What it means |
//! |---|---|
//! | `Visible` | A labelled button, control or label the user can see and press. |
//! | `Menu` | An item of a menu, a dropdown or a dialog. |
//! | `ContextMenu` | A right click, or an item of the menu it opened. |
//! | `Search` | Typed into a search box (and the Enter that takes the result). |
//! | `Shortcut` | A key chord with no other way in: hidden. |
//! | `ModifierDrag` | A pointer gesture that needs a held modifier key: hidden. |
//! | `Untagged` | The scorer cannot tell. Listed, so each one gets looked at. |
//!
//! **How it is tagged.** Widgets cannot usefully carry a tag without editing
//! every call site: menu bodies are
//! only built while a menu is open, and the shortcuts are consumed in about
//! thirty places. What the log already has is egui's accessibility tree at
//! each end of each action, which says whether a labelled widget was there.
//! So pointer actions are classified from what they hit, and the only thing
//! that has to be written down is the key chords, in [`CHORDS`], with the way
//! each can also be reached. A chord that is not in the table is `Untagged`,
//! so a new shortcut shows up as a gap, and a test keeps every global
//! shortcut (`app::shortcut_chords`) in the table.
//!
//! The share of **hidden** entry points is `Shortcut` plus `ModifierDrag` over
//! all actions. `Untagged` actions are neither: they are listed.

use egui::{Key, Modifiers};

use crate::testkit::{Action, ActionKind};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Surface {
    Visible,
    Menu,
    ContextMenu,
    Search,
    Shortcut,
    ModifierDrag,
    Untagged,
}

impl Surface {
    pub const ALL: [Surface; 7] = [
        Surface::Visible,
        Surface::Menu,
        Surface::ContextMenu,
        Surface::Search,
        Surface::Shortcut,
        Surface::ModifierDrag,
        Surface::Untagged,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Surface::Visible => "visible",
            Surface::Menu => "menu",
            Surface::ContextMenu => "context menu",
            Surface::Search => "search",
            Surface::Shortcut => "shortcut only",
            Surface::ModifierDrag => "modifier only",
            Surface::Untagged => "untagged",
        }
    }

    /// Whether a user has to know it is there, because nothing shows it.
    pub fn hidden(self) -> bool {
        matches!(self, Surface::Shortcut | Surface::ModifierDrag)
    }
}

/// A key chord and the best way into the same thing that does not need it.
pub struct Chord {
    pub modifiers: Modifiers,
    pub key: Key,
    pub surface: Surface,
    /// Where the other way is, or why there is none.
    pub note: &'static str,
}

const fn chord(modifiers: Modifiers, key: Key, surface: Surface, note: &'static str) -> Chord {
    Chord {
        modifiers,
        key,
        surface,
        note,
    }
}

const NONE: Modifiers = Modifiers::NONE;
const CMD: Modifiers = Modifiers::COMMAND;
const SHIFT: Modifiers = Modifiers::SHIFT;
const CMD_SHIFT: Modifiers = Modifiers {
    alt: false,
    ctrl: false,
    shift: true,
    mac_cmd: false,
    command: true,
};

/// The key chords the app answers, and how else each can be reached. A chord
/// with `Surface::Shortcut` has no other way. Keep it complete for the keys a
/// route uses: a missing one is reported as untagged.
pub const CHORDS: &[Chord] = &[
    // Global shortcuts (`app::SHORTCUTS`).
    chord(CMD, Key::Z, Surface::Menu, "Edit > Undo"),
    chord(CMD_SHIFT, Key::Z, Surface::Menu, "Edit > Redo"),
    chord(CMD, Key::S, Surface::Menu, "File > Save"),
    chord(CMD_SHIFT, Key::S, Surface::Menu, "File > Save As"),
    chord(CMD, Key::O, Surface::Menu, "File > Open"),
    chord(
        CMD_SHIFT,
        Key::O,
        Surface::Menu,
        "File > Open with Settings",
    ),
    chord(CMD, Key::N, Surface::Menu, "File > New"),
    chord(CMD, Key::Comma, Surface::Menu, "Settings in the menu"),
    chord(CMD, Key::K, Surface::Menu, "View > Command Palette"),
    chord(
        NONE,
        Key::Space,
        Surface::Visible,
        "the transport's play button",
    ),
    chord(
        NONE,
        Key::M,
        Surface::Visible,
        "the computer keyboard button",
    ),
    chord(NONE, Key::T, Surface::Visible, "the Tap button"),
    // Shown in the Edit and View menus, handled by the view itself.
    chord(CMD, Key::X, Surface::Menu, "Edit > Cut"),
    chord(CMD, Key::C, Surface::Menu, "Edit > Copy"),
    chord(CMD, Key::V, Surface::Menu, "Edit > Paste"),
    chord(CMD, Key::D, Surface::Menu, "Edit > Duplicate"),
    chord(CMD, Key::L, Surface::Menu, "Edit > Arrange"),
    chord(NONE, Key::F, Surface::Menu, "View > Fit"),
    chord(NONE, Key::X, Surface::Menu, "Edit > Delete"),
    chord(NONE, Key::Delete, Surface::Menu, "Edit > Delete"),
    chord(NONE, Key::Backspace, Surface::Menu, "Edit > Delete"),
    // The node editor.
    chord(
        SHIFT,
        Key::A,
        Surface::Shortcut,
        "no menu or button adds a node",
    ),
    chord(
        SHIFT,
        Key::D,
        Surface::Shortcut,
        "no menu entry duplicates nodes",
    ),
    chord(NONE, Key::A, Surface::Shortcut, "no menu entry selects all"),
];

fn find_chord(modifiers: Modifiers, key: Key) -> Option<&'static Chord> {
    // `command` and `ctrl` both mean the platform's command key in a test.
    CHORDS.iter().find(|c| {
        c.key == key
            && c.modifiers.shift == modifiers.shift
            && c.modifiers.alt == modifiers.alt
            && c.modifiers.command == modifiers.command
    })
}

/// Roles of egui's accessibility tree that are containers and not something
/// the user can press.
const CONTAINERS: &[&str] = &[
    "Window",
    "Pane",
    "GenericContainer",
    "Group",
    "Canvas",
    "Unknown",
    "Document",
    "ScrollView",
    "Main",
    "Region",
    "Application",
];

/// One action's surface and, for an untagged one, what it was.
#[derive(Clone, Debug, PartialEq)]
pub struct Tagged {
    pub surface: Surface,
    pub what: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Surfaces {
    /// Actions by surface, in [`Surface::ALL`] order.
    pub counts: [usize; 7],
    pub total: usize,
    /// The untagged actions, in order.
    pub untagged: Vec<String>,
    /// The hidden entry points, in order.
    pub hidden: Vec<String>,
}

impl Surfaces {
    pub fn count(&self, s: Surface) -> usize {
        self.counts[Surface::ALL.iter().position(|&x| x == s).unwrap()]
    }

    pub fn hidden_count(&self) -> usize {
        self.hidden.len()
    }

    /// The share of actions that are hidden entry points.
    pub fn hidden_share(&self) -> f64 {
        if self.total == 0 {
            0.0
        } else {
            self.hidden_count() as f64 / self.total as f64
        }
    }
}

fn describe(a: &Action) -> String {
    let view = if a.start_view.is_empty() {
        "window"
    } else {
        &a.start_view
    };
    let on = a
        .start_hit
        .as_ref()
        .filter(|h| !h.label.is_empty())
        .map_or("an unlabelled target".to_string(), |h| {
            format!("{:?} ({})", h.label, h.role)
        });
    let held = mods(a.modifiers);
    match &a.kind {
        ActionKind::Key(k) => format!("{held}{k:?} in the {view}"),
        ActionKind::Text(t) => format!("type {t:?} in the {view}"),
        ActionKind::Click(egui::PointerButton::Secondary) => {
            format!("{held}right click on {on} in the {view}")
        }
        ActionKind::Click(_) => format!("{held}click on {on} in the {view}"),
        ActionKind::DoubleClick => format!("{held}double click on {on} in the {view}"),
        ActionKind::Drag(_) => format!("{held}drag from {on} in the {view}"),
        ActionKind::Wheel => format!("{held}scroll on {on} in the {view}"),
        ActionKind::Drop { .. } => format!("drop on {on} in the {view}"),
        _ => "marker".into(),
    }
}

fn mods(m: Modifiers) -> String {
    let mut s = String::new();
    if m.command {
        s.push_str("Cmd+");
    }
    if m.ctrl && !m.command {
        s.push_str("Ctrl+");
    }
    if m.alt {
        s.push_str("Alt+");
    }
    if m.shift {
        s.push_str("Shift+");
    }
    s
}

/// Tags every action of `log`. Markers are skipped.
pub fn tag(log: &[Action]) -> Vec<(usize, Tagged)> {
    let mut out: Vec<(usize, Tagged)> = Vec::new();
    // How the last thing outside any popup was opened: the root of the popup
    // that is open now.
    let mut opener = Surface::Menu;
    let mut prev: Option<(Surface, &Action)> = None;
    for (i, a) in log.iter().enumerate() {
        if a.kind.is_marker() {
            continue;
        }
        let in_popup = a.start_view == "overlay";
        let surface = match &a.kind {
            ActionKind::Text(_) => match prev {
                Some((_, p)) if is_search_opener(p) => Surface::Search,
                _ => Surface::Untagged,
            },
            ActionKind::Key(key) => {
                match prev {
                    // Enter or Escape right after typing belongs to the box.
                    Some((s, p))
                        if matches!(p.kind, ActionKind::Text(_))
                            && matches!(key, Key::Enter | Key::Escape) =>
                    {
                        s
                    }
                    _ => find_chord(a.modifiers, *key).map_or(Surface::Untagged, |c| c.surface),
                }
            }
            ActionKind::Click(egui::PointerButton::Secondary) if !in_popup => Surface::ContextMenu,
            ActionKind::Click(_) | ActionKind::DoubleClick | ActionKind::Drag(_) => {
                if a.modifiers.any() {
                    Surface::ModifierDrag
                } else if in_popup {
                    opener
                } else if is_labelled_control(a) {
                    Surface::Visible
                } else {
                    Surface::Untagged
                }
            }
            // Scrolling and dropping files are not buttons; the scorer does
            // not know what a user would expect to find there.
            ActionKind::Wheel | ActionKind::Drop { .. } => {
                if a.modifiers.any() {
                    Surface::ModifierDrag
                } else {
                    Surface::Untagged
                }
            }
            _ => continue,
        };
        // What opens a popup decides what its items are.
        if !in_popup {
            opener = if surface == Surface::ContextMenu {
                Surface::ContextMenu
            } else {
                Surface::Menu
            };
        }
        out.push((
            i,
            Tagged {
                surface,
                what: describe(a),
            },
        ));
        prev = Some((surface, a));
    }
    out
}

fn is_search_opener(a: &Action) -> bool {
    matches!(a.kind, ActionKind::Key(Key::A)) && a.modifiers.shift
}

fn is_labelled_control(a: &Action) -> bool {
    a.start_hit
        .as_ref()
        .is_some_and(|h| !h.label.is_empty() && !CONTAINERS.contains(&h.role.as_str()))
}

pub fn score(log: &[Action]) -> Surfaces {
    let mut s = Surfaces::default();
    for (_, t) in tag(log) {
        s.total += 1;
        let i = Surface::ALL.iter().position(|&x| x == t.surface).unwrap();
        s.counts[i] += 1;
        match t.surface {
            Surface::Untagged => s.untagged.push(t.what),
            x if x.hidden() => s.hidden.push(t.what),
            _ => {}
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::Hit;
    use egui::{PointerButton, Pos2, Rect};

    fn act(
        kind: ActionKind,
        modifiers: Modifiers,
        view: &str,
        hit: Option<(&str, &str)>,
    ) -> Action {
        let hit = hit.map(|(label, role)| Hit {
            label: label.into(),
            role: role.into(),
            rect: Rect::from_min_size(Pos2::ZERO, egui::vec2(10.0, 10.0)),
        });
        Action {
            kind,
            modifiers,
            start: Pos2::ZERO,
            end: Pos2::ZERO,
            start_hit: hit.clone(),
            end_hit: hit,
            start_view: view.into(),
            end_view: view.into(),
            time: 0.0,
        }
    }

    fn click(hit: Option<(&str, &str)>) -> Action {
        act(
            ActionKind::Click(PointerButton::Primary),
            Modifiers::NONE,
            "node editor",
            hit,
        )
    }

    fn key(modifiers: Modifiers, k: Key) -> Action {
        act(ActionKind::Key(k), modifiers, "node editor", None)
    }

    fn surfaces(log: &[Action]) -> Vec<Surface> {
        tag(log).into_iter().map(|(_, t)| t.surface).collect()
    }

    #[test]
    fn a_labelled_button_is_visible_and_a_bare_canvas_is_not_tagged() {
        let log = [click(Some(("Play", "Button"))), click(None)];
        assert_eq!(surfaces(&log), [Surface::Visible, Surface::Untagged]);
        // A labelled container is not something to press.
        assert_eq!(
            surfaces(&[click(Some(("Canvas", "GenericContainer")))]),
            [Surface::Untagged]
        );
    }

    #[test]
    fn a_held_modifier_makes_a_pointer_gesture_hidden_even_on_a_button() {
        let mut a = click(Some(("Play", "Button")));
        a.modifiers = Modifiers::SHIFT;
        assert_eq!(surfaces(&[a]), [Surface::ModifierDrag]);
    }

    #[test]
    fn search_is_shift_a_then_text_then_enter_and_only_the_opener_is_hidden() {
        let log = [
            key(Modifiers::SHIFT, Key::A),
            act(
                ActionKind::Text("sine".into()),
                Modifiers::NONE,
                "overlay",
                None,
            ),
            key(Modifiers::NONE, Key::Enter),
        ];
        assert_eq!(
            surfaces(&log),
            [Surface::Shortcut, Surface::Search, Surface::Search]
        );
        let s = score(&log);
        assert_eq!((s.total, s.hidden_count()), (3, 1));
        assert!((s.hidden_share() - 1.0 / 3.0).abs() < 1e-9);
    }

    #[test]
    fn text_nobody_opened_is_untagged() {
        let log = [act(
            ActionKind::Text("x".into()),
            Modifiers::NONE,
            "properties",
            None,
        )];
        assert_eq!(surfaces(&log), [Surface::Untagged]);
    }

    #[test]
    fn text_after_something_other_than_the_search_chord_is_untagged() {
        let log = [
            click(Some(("Name", "TextInput"))),
            act(
                ActionKind::Text("x".into()),
                Modifiers::NONE,
                "properties",
                None,
            ),
        ];
        assert_eq!(surfaces(&log), [Surface::Visible, Surface::Untagged]);
    }

    #[test]
    fn a_chord_with_a_menu_item_is_not_hidden_and_an_unknown_one_is_untagged() {
        let log = [
            key(Modifiers::COMMAND, Key::Z),
            key(Modifiers::COMMAND | Modifiers::SHIFT, Key::Z),
            key(Modifiers::NONE, Key::F9),
        ];
        assert_eq!(
            surfaces(&log),
            [Surface::Menu, Surface::Menu, Surface::Untagged]
        );
    }

    #[test]
    fn the_items_of_a_popup_take_the_surface_of_what_opened_it() {
        let item = |hit| {
            act(
                ActionKind::Click(PointerButton::Primary),
                Modifiers::NONE,
                "overlay",
                hit,
            )
        };
        let right = act(
            ActionKind::Click(PointerButton::Secondary),
            Modifiers::NONE,
            "timeline",
            None,
        );
        let log = [
            right,
            item(Some(("Show takes", "MenuItem"))),
            click(Some(("Snap: Beat", "Button"))),
            item(Some(("1/2 beat", "MenuItem"))),
        ];
        assert_eq!(
            surfaces(&log),
            [
                Surface::ContextMenu,
                Surface::ContextMenu,
                Surface::Visible,
                Surface::Menu
            ]
        );
    }

    #[test]
    fn markers_are_not_actions_and_untagged_ones_are_listed_by_what_they_were() {
        let log = [
            act(ActionKind::Step("go".into()), Modifiers::NONE, "", None),
            click(None),
            act(ActionKind::Wheel, Modifiers::NONE, "piano roll", None),
        ];
        let s = score(&log);
        assert_eq!((s.total, s.count(Surface::Untagged)), (2, 2));
        assert_eq!(
            s.untagged[0],
            "click on an unlabelled target in the node editor"
        );
        assert_eq!(
            s.untagged[1],
            "scroll on an unlabelled target in the piano roll"
        );
    }

    /// Every shortcut the app answers globally must be in the table, or a
    /// new one would show as untagged in every route that uses it.
    #[test]
    fn every_global_shortcut_has_a_surface() {
        for shortcut in crate::app::shortcut_chords() {
            assert!(
                find_chord(shortcut.modifiers, shortcut.logical_key).is_some(),
                "{shortcut:?} is a shortcut but is not in surface::CHORDS"
            );
        }
    }
}
