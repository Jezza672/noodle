//! S03: drive external hardware like an internal instrument.
//!
//! Partial. A MIDI part goes to the synth through a MIDI Out node, and the
//! port is chosen in View > MIDI ports (#346), which lists every MIDI node
//! with a drop-down of the ports (the test build lists two fake ones).
//! Recording the synth's audio back in is not routed because a route cannot
//! supply an audio input (#334); round-trip latency compensation for the
//! recording is therefore not scored either. The route builds the sending
//! half: the keys reach a MIDI Out node on the synth's port.

use std::path::Path;

use egui::Modifiers;
use noodle_core::Value;
use noodle_nodes::{MIDI_OUT_ID, MIDI_OUT_PORT_KEY};

use super::super::handle::{Handle, Probe};
use super::super::{LateChange, Route, Tag};
use super::patch::{Op, Ref, check, run, seeded};
use crate::app::App;

const SYNTH: &str = "Test Synth";

/// Opens View > MIDI ports and picks the synth for the MIDI Out.
fn pick_port(h: &mut Handle) {
    h.step("open the MIDI ports view");
    let view = h.find("View");
    h.click(view, Modifiers::NONE);
    let item = h.find("MIDI ports");
    h.click(item, Modifiers::NONE);
    h.probe().settle();
    h.step("choose the synth's port");
    let node = h
        .probe()
        .project()
        .graph()
        .nodes()
        .find(|(_, n)| n.type_id == MIDI_OUT_ID)
        .map(|(id, _)| id)
        .expect("the MIDI Out node");
    let combo = h.find(&format!("MIDI Out {}", node.0));
    h.click(combo, Modifiers::NONE);
    let port = h.find(SYNTH);
    h.click(port, Modifiers::NONE);
    h.probe().settle();
}

fn steps(h: &mut Handle, _: usize) {
    run(h, BUILD);
    pick_port(h);
}

fn done(p: &mut Probe, _: usize) -> Result<(), String> {
    check(p, BUILD)?;
    let port = p
        .project()
        .graph()
        .nodes()
        .find(|(_, n)| n.type_id == MIDI_OUT_ID)
        .and_then(|(_, n)| match n.config.get(MIDI_OUT_PORT_KEY) {
            Some(Value::Text(t)) => Some(t.clone()),
            _ => None,
        });
    if port.as_deref() != Some(SYNTH) {
        return Err(format!("the MIDI Out's port is {port:?}, wanted {SYNTH:?}"));
    }
    Ok(())
}

const KEYS: Ref = Ref("event.midi_in", 0);
const ARP: Ref = Ref("event.arpeggiator", 0);
const SEND: Ref = Ref("event.midi_out", 0);

fn fixture(dir: &Path) -> App {
    seeded(dir, &[("event.midi_in", 60.0, 300.0, &[])], &[])
}

const BUILD: &[Op] = &[
    Op::Step("add the MIDI Out"),
    Op::Add(SEND, 700.0, 460.0),
    Op::Step("send the part to it"),
    Op::Wire(KEYS, "out", SEND, "in"),
];

const THROUGH_ARP: &[Op] = &[
    Op::Step("play the part through an arpeggiator first"),
    Op::Add(ARP, 380.0, 460.0),
    Op::Wire(KEYS, "out", ARP, "in"),
    Op::Wire(ARP, "out", SEND, "in"),
];

pub fn route() -> Route {
    Route {
        story: "S03",
        tag: Tag::Default,
        name: "send MIDI to an external synth",
        fixture,
        size: None,
        steps,
        done,
        late: &[LateChange {
            name: "change what is sent",
            steps: |h, _| run(h, THROUGH_ARP),
            done: |p, _| check(p, THROUGH_ARP),
            keep: &[],
        }],
    }
}
