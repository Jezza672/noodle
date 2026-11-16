//! S32: add a self-hosted source of sounds and use a sound from it.
//!
//! The Sounds panel starts with no sources, so it is empty. Its Sources
//! button opens Settings on the Sources page, where the address of a site and
//! a token are typed and the source is added. The fake site refuses requests
//! without that token, so the sound only arrives if the login was really
//! entered and sent. Back in the panel the sound is dragged onto a Sampler.
//! Adding a folder is not routed: it asks a native dialog.

use std::cell::RefCell;
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

const TOKEN: &str = "s3cret";

thread_local! {
    /// The hash of the sound the fixture serves.
    static HASH: RefCell<String> = const { RefCell::new(String::new()) };
}

fn fixture(dir: &Path) -> App {
    let served = dir.join("served");
    std::fs::create_dir_all(&served).expect("the folder is made");
    let (site, hashes) = fake::site(&served, &[220.0]);
    *site.require.lock().unwrap() = Some(format!("Bearer {TOKEN}"));
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
        Vec::new(),
    ));
    app
}

fn steps(h: &mut Handle, _size: usize) {
    h.step("open the Sounds panel, which has no sources");
    let view = h.find("View");
    h.click(view, Modifiers::NONE);
    let item = h.find("Sounds");
    h.click(item, Modifiers::NONE);
    h.probe().settle_sounds();
    h.step("open the Sources page from the panel");
    let sources = h.find("Sources…");
    h.click(sources, Modifiers::NONE);
    h.probe().settle_sounds();
    h.step("type the address of the site");
    let address = h.text_field_after("Address");
    h.click(address, Modifiers::NONE);
    h.type_text(address, fake::BASE);
    h.step("say it wants a token, and type it");
    let login = h.probe().find_value("None");
    h.click(login, Modifiers::NONE);
    let token = h.find("Token");
    h.click(token, Modifiers::NONE);
    let field = h.text_field_after("Token");
    h.click(field, Modifiers::NONE);
    h.type_text(field, TOKEN);
    h.step("add the source and close Settings");
    let add = h.find("Add source");
    h.click(add, Modifiers::NONE);
    h.probe().settle_sounds();
    let close = h.find("Close");
    h.click(close, Modifiers::NONE);
    h.step("drag its sound onto the Sampler");
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
        other => Err(format!(
            "the Sampler's sample is {other:?}, not the sound from the added source"
        )),
    }
}

pub fn route() -> Route {
    Route {
        story: "S32",
        tag: Tag::Default,
        name: "add a self-hosted source with a token and use a sound from it",
        fixture,
        size: None,
        steps,
        done,
        late: &[],
    }
}
