//! S03: drive external hardware like an internal instrument.
//!
//! Partial. A MIDI part goes to the synth through a MIDI Out node. Its port
//! is typed text, not a picker (#346), and recording the synth's audio back
//! in is not routed because a route cannot supply an audio input (#334);
//! round-trip latency compensation for the recording is therefore not
//! scored either. The route builds the sending half: the keys reach a MIDI
//! Out node.

use std::path::Path;

use super::super::{LateChange, Route, Tag};
use super::patch::{Op, Ref, check, run, seeded};
use crate::app::App;

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
        steps: |h, _| run(h, BUILD),
        done: |p, _| check(p, BUILD),
        late: &[LateChange {
            name: "change what is sent",
            steps: |h, _| run(h, THROUGH_ARP),
            done: |p, _| check(p, THROUGH_ARP),
            keep: &[],
        }],
    }
}
