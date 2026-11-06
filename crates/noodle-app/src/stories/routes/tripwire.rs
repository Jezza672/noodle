//! Routes for stories whose feature does not exist yet.
//!
//! A blocked story still gets a route: it opens the menus where a user would
//! look for the feature and checks that the feature is not there. The check
//! is a tripwire. The day a menu offers the feature, the route fails and
//! says so, and the PR that adds the feature writes the real route and moves
//! the story's status in the registry (see `docs/architecture/usability.md`).

use std::cell::RefCell;

use egui::Modifiers;

use super::super::handle::{Handle, Probe};

thread_local! {
    static FOUND: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

/// Opens each top-level menu in turn and notes which of `words` it mentions.
pub fn look(h: &mut Handle, words: &[&str]) {
    FOUND.with(|f| f.borrow_mut().clear());
    for menu in ["File", "Edit", "View"] {
        h.step(&format!("look in the {menu} menu"));
        let at = h.find(menu);
        h.click(at, Modifiers::NONE);
        h.probe().settle();
        for word in words {
            if h.probe().mentions(word) {
                FOUND.with(|f| f.borrow_mut().push(format!("{menu}: {word}")));
            }
        }
        // Close the menu again.
        h.click(at, Modifiers::NONE);
        h.probe().settle();
    }
}

/// The done check of a blocked story. It always fails, since the story's
/// goal cannot be reached: a pass would score a project nobody built
/// anything in. The message starts with [`BLOCKED`] while the feature is
/// still absent, so the runner can tell that expected failure from the
/// feature turning up in a menu.
pub fn blocked(_: &mut Probe, story: &str) -> Result<(), String> {
    FOUND.with(|f| {
        let found = f.borrow();
        if found.is_empty() {
            Err(format!(
                "{BLOCKED} {story} has no menu entry for it yet, so nothing can be built"
            ))
        } else {
            Err(format!(
                "{story} has what the route looked for ({}): write the real route and update the registry",
                found.join(", ")
            ))
        }
    })
}

/// How the failure of a blocked story's route begins.
pub const BLOCKED: &str = "blocked:";
