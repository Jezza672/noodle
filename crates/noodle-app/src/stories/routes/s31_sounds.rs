//! S31: find a sound in the packs, hear it, and put it on a Sampler.
//!
//! The panel is opened from the View menu, searched, and the result clicked
//! to audition it (the sound is downloaded first, from a fake source: a route
//! never touches the network). Then the result is dragged onto the Sampler on
//! the canvas, which takes its hash.

use std::cell::{Cell, RefCell};
use std::path::Path;

use egui::Modifiers;
use noodle_core::{Command, Node, Value};
use noodle_nodes::{SAMPLE_KEY, SAMPLER_ID};

use super::super::handle::{Handle, Probe};
use super::super::{Route, Tag};
use super::{finished, virtual_session};
use crate::app::App;
use crate::session::Edit;
use crate::sounds::Sounds;
use crate::sounds::source::{Net, fake};

thread_local! {
    /// Whether the steps saw the sound being auditioned. Playing is not in a
    /// saved project, so the check reads the record.
    static AUDITIONED: Cell<bool> = const { Cell::new(false) };
    /// The hash of the sound the fixture serves.
    static HASH: RefCell<String> = const { RefCell::new(String::new()) };
}

fn fixture(dir: &Path) -> App {
    let served = dir.join("served");
    std::fs::create_dir_all(&served).expect("the folder is made");
    let (site, hashes) = fake::site(&served, &[220.0]);
    HASH.with(|h| *h.borrow_mut() = hashes[0].clone());
    let mut session = virtual_session(dir);
    let id = session.new_node_id();
    session.edit([Edit::Apply(Command::AddNode {
        id,
        node: Node::new(SAMPLER_ID).at(500.0, 300.0),
    })]);
    let mut app = finished(session, dir);
    app.use_sounds(Sounds::with(
        Net::new(site),
        Some(dir.join("sounds-data")),
        vec![fake::source()],
    ));
    app
}

fn steps(h: &mut Handle, _size: usize) {
    AUDITIONED.with(|a| a.set(false));
    h.step("open the Sounds panel");
    let view = h.find("View");
    h.click(view, Modifiers::NONE);
    let item = h.find("Sounds");
    h.click(item, Modifiers::NONE);
    h.probe().settle_sounds();
    h.step("search for a tone");
    let search = h.probe().line_box();
    h.click(search, Modifiers::NONE);
    h.type_text(search, "tone");
    h.step("click the result to hear it");
    let row = h.find_like("Tones ·");
    h.click(row, Modifiers::NONE);
    h.probe().settle_sounds();
    AUDITIONED.with(|a| a.set(h.probe().has("Auditioning Tone 220")));
    h.step("drag it onto the Sampler");
    let row = h.find_like("Tones ·");
    let node = h
        .probe()
        .project()
        .graph()
        .nodes()
        .find(|(_, n)| n.type_id == SAMPLER_ID)
        .map(|(id, _)| id)
        .expect("the fixture has a Sampler");
    let sampler = h.socket(node, false, "in");
    h.drag_between(Modifiers::NONE, row, sampler);
    h.probe().settle_sounds();
}

fn done(p: &mut Probe, _size: usize) -> Result<(), String> {
    if !AUDITIONED.with(Cell::get) {
        return Err("the sound was never heard".into());
    }
    let want = HASH.with(|h| h.borrow().clone());
    let sampler = p
        .project()
        .graph()
        .nodes()
        .find(|(_, n)| n.type_id == SAMPLER_ID)
        .map(|(_, n)| n)
        .ok_or("the Sampler is gone")?;
    match sampler.config.get(SAMPLE_KEY) {
        Some(Value::Text(hash)) if *hash == want => Ok(()),
        other => Err(format!("the Sampler's sample is {other:?}, not the sound")),
    }
}

pub fn route() -> Route {
    Route {
        story: "S31",
        tag: Tag::Default,
        name: "search the packs, hear a sound and drop it on a Sampler",
        fixture,
        size: None,
        steps,
        done,
        late: &[],
    }
}
