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

/// Fails if `look` found any of the words.
pub fn nothing_found(_: &mut Probe, story: &str) -> Result<(), String> {
    FOUND.with(|f| {
        let found = f.borrow();
        if found.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "{story} has what the route looked for ({}): write the real route and update the registry",
                found.join(", ")
            ))
        }
    })
}
