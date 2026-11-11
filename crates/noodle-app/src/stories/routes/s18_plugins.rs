//! S18: use third-party plugins.
//!
//! Partial. View, Plugins opens the browser, which lists the CLAP plugins
//! found on the machine, each with an Add button. The test machine has no
//! plugins, so adding one, automating it and saving its state are not
//! routed; only CLAP is hosted (no VST3 or AU), and swapping a plugin
//! while keeping its automation, or opening a project with a plugin
//! missing, are untested.

use std::cell::Cell;

use egui::Modifiers;

use super::super::handle::{Handle, Probe};
use super::super::{Route, Tag};
use super::empty_project;

// Steps and check run on the same thread, so a thread-local carries the record.
thread_local! {
    /// Whether the browser opened and said how many plugins it found. The
    /// browser is not in a saved project, so the check cannot look for it
    /// after the reopen.
    static LISTED: Cell<bool> = const { Cell::new(false) };
}

fn steps(h: &mut Handle, _size: usize) {
    LISTED.with(|l| l.set(false));
    h.step("open the plugin browser");
    let view = h.find("View");
    h.click(view, Modifiers::NONE);
    h.probe().settle();
    let plugins = h.find("Plugins");
    h.click(plugins, Modifiers::NONE);
    h.probe().settle();
    assert!(
        h.probe().mentions(" plugins"),
        "the plugin browser does not show how many plugins it found"
    );
    LISTED.with(|l| l.set(true));
}

fn done(p: &mut Probe, _size: usize) -> Result<(), String> {
    // The browser leaves nothing in the project; the steps noted that it
    // opened and counted the plugins. Browsing must not add a node either.
    if !LISTED.with(Cell::get) {
        return Err("the plugin browser was never opened".into());
    }
    if p.project().graph().nodes().next().is_some() {
        return Err("browsing the plugins changed the graph".into());
    }
    Ok(())
}

pub fn route() -> Route {
    Route {
        story: "S18",
        tag: Tag::Default,
        name: "open the plugin browser",
        fixture: empty_project,
        size: None,
        steps,
        done,
        late: &[],
    }
}
