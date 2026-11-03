//! S18: use third-party plugins.
//!
//! Partial. View, Plugins opens the browser, which lists the CLAP plugins
//! found on the machine, each with an Add button. The test machine has no
//! plugins, so adding one, automating it and saving its state are not
//! routed; only CLAP is hosted (no VST3 or AU), and swapping a plugin
//! while keeping its automation, or opening a project with a plugin
//! missing, are untested (#357).

use egui::Modifiers;

use super::super::handle::{Handle, Probe};
use super::super::{Route, Tag};
use super::empty_project;

fn steps(h: &mut Handle, _size: usize) {
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
}

fn done(_: &mut Probe, _size: usize) -> Result<(), String> {
    // The browser leaves nothing in the project. Its labels are checked in
    // the steps, which cannot go on without them.
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
