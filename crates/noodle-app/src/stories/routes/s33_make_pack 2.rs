//! S33: make a shareable pack from a folder of sounds.
//!
//! The File menu's Make Shareable Pack… asks for a folder (the route answers
//! the native picker, as a person would in the dialog) and opens the wizard.
//! Export stays disabled until the pack names an author, so the route types
//! one, exports to a folder, and adds the result as a source to see its sounds
//! listed in the Sounds panel, read back from the exported site.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};

use egui::Modifiers;

use super::super::handle::{Handle, Probe};
use super::super::{Route, Tag};
use super::{finished, virtual_session};
use crate::app::App;
use crate::sounds::Sounds;
use crate::sounds::source::{Net, fake};

thread_local! {
    /// Where the exported site is expected.
    /// Whether the steps saw both sounds listed. The panel is not in a saved
    /// project, so the check reads the record.
    static LISTED: Cell<bool> = const { Cell::new(false) };
    static OUT: RefCell<PathBuf> = const { RefCell::new(PathBuf::new()) };
}

fn fixture(dir: &Path) -> App {
    let src = dir.join("My Kit");
    let drums = src.join("Drums");
    let scratch = dir.join("scratch");
    for d in [&drums, &scratch] {
        std::fs::create_dir_all(d).expect("the folder is made");
    }
    for (name, hz) in [("Kick", 110.0), ("Snare", 220.0)] {
        let (bytes, _) = fake::tone(&scratch, hz);
        std::fs::write(drums.join(format!("{name}.flac")), bytes).expect("the sound is written");
    }
    let out = dir.join("export");
    std::fs::create_dir_all(&out).expect("the folder is made");
    OUT.with(|o| *o.borrow_mut() = out.clone());
    crate::picker::set_handler(Box::new(move |what: &str| {
        if what.contains("to put the pack in") {
            vec![out.clone()]
        } else {
            vec![src.clone()]
        }
    }));
    let mut app = finished(virtual_session(dir), dir);
    app.use_sounds(Sounds::with(
        Net::http(),
        Some(dir.join("sounds-data")),
        Vec::new(),
    ));
    app
}

fn steps(h: &mut Handle, _size: usize) {
    LISTED.with(|l| l.set(false));
    h.step("choose Make Shareable Pack… from the File menu");
    let file = h.find("File");
    h.click(file, Modifiers::NONE);
    let item = h.find("Make Shareable Pack…");
    h.click(item, Modifiers::NONE);
    h.probe().settle_sounds();
    h.step("say who made the sounds");
    let author = h.text_field_after("Author");
    h.click(author, Modifiers::NONE);
    h.type_text(author, "Jo");
    h.step("export it to a folder");
    let export = h.find("Export…");
    h.click(export, Modifiers::NONE);
    h.probe().settle_sounds();
    h.step("add it as a source");
    let add = h.find("Add it as a source");
    h.click(add, Modifiers::NONE);
    h.probe().settle_sounds();
    h.step("open the Sounds panel to see its sounds");
    let view = h.find("View");
    h.click(view, Modifiers::NONE);
    let sounds = h.find("Sounds");
    h.click(sounds, Modifiers::NONE);
    h.probe().settle_sounds();
    LISTED.with(|l| l.set(h.probe().mentions("Kick") && h.probe().mentions("Snare")));
}

fn done(p: &mut Probe, _size: usize) -> Result<(), String> {
    let out = OUT.with(|o| o.borrow().clone());
    // The exported site is what CI would publish: verify it decodes to its hashes.
    let checked = noodle_publish::verify(&out).map_err(|e| e.to_string())?;
    if checked != 2 {
        return Err(format!("the site holds {checked} sounds, not 2"));
    }
    let _ = p;
    if !LISTED.with(Cell::get) {
        return Err("the exported pack's sounds were not listed in the Sounds panel".into());
    }
    Ok(())
}

pub fn route() -> Route {
    Route {
        story: "S33",
        tag: Tag::Default,
        name: "make a shareable pack from a folder and add it as a source",
        fixture,
        size: None,
        steps,
        done,
        late: &[],
    }
}
