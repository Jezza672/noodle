//! S18: swap a plugin for another, keeping what maps.
//!
//! Partial. The fixture uses two scanned plugin descriptors rather than real
//! plugin binaries (the editor only needs their ports), so hosting, plugin
//! state and the sound of a plugin are covered by the plugin crate's own
//! tests, not here. The route: right-click the reverb, "Replace with...",
//! pick the other reverb. Its wires, its automation lane and its stored
//! values follow by parameter name. The second late change of the story
//! (reopening where a plugin is missing) is a session test: the node stays
//! as a pass-through and the project keeps all of it.

use std::path::Path;

use egui::{Key, Modifiers, Pos2};
use noodle_core::{
    AutomationLane, AutomationPoint, Command, Connection, Endpoint, Node, NodeId, Project, Tick,
    Value,
};
use noodle_plugins::{AudioPort, Descriptor, ParamDesc, PluginFile, PluginFormat};

use super::super::handle::{Handle, Probe};
use super::super::{LateChange, Route, Tag};
use crate::app::App;
use crate::session::{Edit, Nodes, Session};
use crate::testkit::virtual_audio;

fn param(id: u32, name: &str, max: f64) -> ParamDesc {
    ParamDesc {
        id,
        name: name.into(),
        module: String::new(),
        min: 0.0,
        max,
        default: 0.0,
        stepped: false,
    }
}

fn reverb(id: &str, name: &str, params: Vec<ParamDesc>) -> Descriptor {
    let port = AudioPort {
        name: "main".into(),
        channels: 2,
    };
    Descriptor {
        format: PluginFormat::Clap,
        id: id.into(),
        name: name.into(),
        vendor: "Nobody".into(),
        instrument: false,
        audio_inputs: vec![port.clone()],
        audio_outputs: vec![port],
        note_inputs: 0,
        note_outputs: 0,
        params,
    }
}

fn nodes() -> Nodes {
    let files = vec![PluginFile {
        path: "/no/such/reverbs.clap".into(),
        len: 0,
        modified: 0,
        plugins: vec![
            reverb(
                "verb.a",
                "Hall Verb",
                vec![param(1, "Mix", 1.0), param(2, "Decay", 10.0)],
            ),
            reverb(
                "verb.b",
                "Plate Verb",
                vec![param(40, "Decay", 20.0), param(41, "Mix", 1.0)],
            ),
        ],
        error: None,
    }];
    let catalog = noodle_plugins::Catalog::new(&files);
    let mut nodes = Nodes::all();
    catalog.register(&mut nodes.registry);
    nodes.plugins = catalog;
    nodes
}

const HALL: &str = "clap.verb.a";
const PLATE: &str = "clap.verb.b";

/// A sine through the hall reverb to the output, with the reverb's decay
/// automated and its mix set.
fn fixture(dir: &Path) -> App {
    let mut session = Session::new(nodes());
    assert!(session.save_as(&dir.join("project.ron")));
    let add = |session: &mut Session, node: Node| {
        let id = session.new_node_id();
        session.edit([Edit::Apply(Command::AddNode { id, node })]);
        id
    };
    let sine = add(&mut session, Node::new("noodle.osc.sine").at(80.0, 300.0));
    let mut hall = Node::new(HALL).at(420.0, 300.0);
    hall.config.set("params", Value::Text("1,2".into()));
    hall.params.insert("p1".into(), 0.4);
    let hall = add(&mut session, hall);
    let out = add(
        &mut session,
        Node::new(noodle_engine::OUTPUT_ID).at(800.0, 300.0),
    );
    let lane = session.project().next_lane_id();
    let wire = |a: NodeId, ap: &str, b: NodeId, bp: &str| {
        Edit::Apply(Command::Connect(Connection {
            from: Endpoint::new(a, ap),
            to: Endpoint::new(b, bp),
        }))
    };
    session.edit([
        wire(sine, "out", hall, "in"),
        wire(hall, "out", out, "in"),
        Edit::Apply(Command::AddLane {
            id: lane,
            lane: AutomationLane::new(
                Endpoint::new(hall, "p2"),
                vec![
                    AutomationPoint {
                        tick: Tick(0),
                        value: 2.0,
                        curve: Default::default(),
                    },
                    AutomationPoint {
                        tick: Tick(3840),
                        value: 6.0,
                        curve: Default::default(),
                    },
                ],
            ),
        }),
    ]);
    session.save();
    let mut session = Session::open(nodes(), &dir.join("project.ron")).expect("the project opens");
    session.set_audio_config(virtual_audio());
    App::new(session)
}

pub fn route() -> Route {
    Route {
        story: "S18",
        tag: Tag::Default,
        name: "swap the reverb plugin, keeping its automation",
        fixture,
        size: None,
        steps: replace_with_plate,
        done: is_plate,
        late: &[LateChange {
            name: "swap it back to the first reverb",
            steps: replace_with_hall,
            done: is_hall,
            keep: &[],
        }],
    }
}

fn reverb_node(p: &Project) -> NodeId {
    p.graph()
        .nodes()
        .find(|(_, n)| n.type_id == HALL || n.type_id == PLATE)
        .map(|(id, _)| id)
        .expect("the reverb is in the project")
}

fn replace(h: &mut Handle, search: &str) {
    let node = reverb_node(h.probe().project());
    h.step("open the node's menu");
    // The middle of the node, between its input and output sockets.
    let (a, b) = (h.socket(node, false, "in"), h.socket(node, true, "out"));
    h.right_click(Pos2::new((a.x + b.x) / 2.0, a.y));
    h.step("choose Replace with");
    let item = h.find("Replace with...");
    h.click(item, Modifiers::NONE);
    h.step("pick the other reverb");
    h.type_text(Pos2::new(640.0, 300.0), search);
    h.press(Pos2::new(640.0, 300.0), Modifiers::NONE, Key::Enter);
}

fn replace_with_plate(h: &mut Handle, _size: usize) {
    replace(h, "plate");
}

fn replace_with_hall(h: &mut Handle, _size: usize) {
    replace(h, "hall");
}

/// Checks the reverb is `type_id` with decay on `decay` / mix on `mix` and
/// everything it had still attached.
fn check(
    p: &mut Probe,
    type_id: &str,
    mix: &str,
    decay: &str,
    decay_end: f32,
) -> Result<(), String> {
    let project = p.project();
    let id = reverb_node(project);
    let node = project.graph().node(id).expect("the node");
    if node.type_id != type_id {
        return Err(format!("the reverb is {}, not {type_id}", node.type_id));
    }
    let sine = project
        .graph()
        .nodes()
        .find(|(_, n)| n.type_id == "noodle.osc.sine")
        .map(|(i, _)| i)
        .ok_or("the sine is gone")?;
    if project.graph().source(&Endpoint::new(id, "in")) != Some(&Endpoint::new(sine, "out")) {
        return Err("the input wire was lost".into());
    }
    if !project
        .graph()
        .connections()
        .any(|c| c.from.node == id && c.from.port == "out")
    {
        return Err("the output wire was lost".into());
    }
    let (_, lane) = project
        .lanes()
        .find(|(_, l)| l.target.node == id)
        .ok_or("the decay automation was lost")?;
    if lane.target.port != decay {
        return Err(format!("the lane drives {}, not {decay}", lane.target.port));
    }
    let last = lane.points.last().map_or(0.0, |p| p.value);
    if (last - decay_end).abs() > 1e-3 {
        return Err(format!("the lane ends at {last}, not {decay_end}"));
    }
    let kept = node.params.get(mix).copied().unwrap_or(-1.0);
    if (kept - 0.4).abs() > 1e-4 {
        return Err(format!("the mix is {kept}, not 0.4"));
    }
    Ok(())
}

fn is_plate(p: &mut Probe, _size: usize) -> Result<(), String> {
    // The plate's decay runs to 20: the lane's 6 of 10 becomes 12 of 20.
    check(p, PLATE, "p41", "p40", 12.0)
}

fn is_hall(p: &mut Probe, _size: usize) -> Result<(), String> {
    check(p, HALL, "p1", "p2", 6.0)
}
