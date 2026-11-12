//! S29: organise a large session with templates.
//!
//! Colours come from a palette on the track's header menu, tracks are put in
//! a folder from the same menu and folded by name from the command palette
//! (Cmd+K), a lane can be dragged down to just its name instead of hiding the
//! track, and the setup is saved as a project template from the File menu. A
//! project made from a template is a copy: the template never updates it.
//! The late change is starting the next project from the template.
//!
//! Key commands for repetitive edits are the palette and the keymap; macros
//! are not built.

use std::path::Path;

use egui::{Key, Modifiers, Pos2};
use noodle_core::Project;

use super::super::handle::{Handle, Probe};
use super::super::{LateChange, Route, Tag};
use super::{add_tracks, finished, has_tracks, virtual_session};
use crate::app::App;

const TEMPLATE: &str = "Band";
/// The story's session: 150 tracks.
const TRACKS: usize = 150;

fn fixture(dir: &Path) -> App {
    let mut session = virtual_session(dir);
    add_tracks(&mut session, TRACKS, 0.0, 0.0);
    let mut app = finished(session, dir);
    app.session_mut()
        .set_templates_dir(Some(dir.join("templates")));
    app
}

/// Right-clicks the header of the `n`th track, in its left margin where
/// there is no widget.
fn header_menu(h: &mut Handle, n: usize) {
    let name = h.find(&format!("Track {n}"));
    h.right_click(Pos2::new(name.x - 26.0, name.y));
}

fn steps(h: &mut Handle, _size: usize) {
    h.step("colour the first track");
    header_menu(h, 1);
    let colour = h.find_like("Colour");
    h.click(colour, Modifiers::NONE);
    let blue = h.find("Blue");
    h.click(blue, Modifiers::NONE);

    h.step("put the first track in a new folder");
    header_menu(h, 1);
    let folder = h.find_like("Folder");
    h.click(folder, Modifiers::NONE);
    let new = h.find("New folder with this track");
    h.click(new, Modifiers::NONE);

    h.step("move the second track into it");
    header_menu(h, 2);
    let folder = h.find_like("Folder");
    h.click(folder, Modifiers::NONE);
    let into = h.find_like("Move to");
    h.click(into, Modifiers::NONE);

    h.step("fold the folder from the command palette");
    let anywhere = h.probe().canvas().center();
    h.press(anywhere, Modifiers::COMMAND, Key::K);
    h.type_text(anywhere, "fold all");
    h.press(anywhere, Modifiers::NONE, Key::Enter);

    h.step("save the setup as a project template");
    let file = h.find("File");
    h.click(file, Modifiers::NONE);
    let save = h.find("Save as Project Template…");
    h.click(save, Modifiers::NONE);
    h.type_text(anywhere, TEMPLATE);
    let ok = h.find("Save");
    h.click(ok, Modifiers::NONE);
}

fn done(p: &mut Probe, _size: usize) -> Result<(), String> {
    has_tracks(p.project(), TRACKS)?;
    let org = p.project().organisation();
    if org.colours.is_empty() {
        return Err("no track has a colour".into());
    }
    let folded: Vec<_> = org.folders.values().filter(|f| f.folded).collect();
    if folded.len() != 1 || org.membership.len() < 2 {
        return Err("there is not one folded folder of two tracks".into());
    }
    let template = p
        .project_dir()
        .join("templates")
        .join(format!("{TEMPLATE}.ron"));
    if !template.exists() {
        return Err("the template was not saved".into());
    }
    Ok(())
}

fn from_template(h: &mut Handle, _size: usize) {
    h.step("start the next project from the template");
    let file = h.find("File");
    h.click(file, Modifiers::NONE);
    let menu = h.find_like("New from Template");
    h.click(menu, Modifiers::NONE);
    let template = h.find(TEMPLATE);
    h.click(template, Modifiers::NONE);
    if h.probe().has("Don't Save") {
        let discard = h.find("Don't Save");
        h.click(discard, Modifiers::NONE);
    }
}

fn started(p: &mut Probe, _size: usize) -> Result<(), String> {
    let project: &Project = p.project();
    has_tracks(project, TRACKS)?;
    let tracks = crate::timeline::tracks(project).len();
    if tracks == TRACKS {
        Ok(())
    } else {
        Err(format!(
            "the new project has {tracks} tracks, wanted {TRACKS}"
        ))
    }
}

pub fn route() -> Route {
    Route {
        story: "S29",
        tag: Tag::Default,
        name: "colour, fold and template a session",
        fixture,
        size: None,
        steps,
        done,
        late: &[LateChange {
            name: "start the next project from the template",
            steps: from_template,
            done: started,
            keep: &[],
        }],
    }
}
