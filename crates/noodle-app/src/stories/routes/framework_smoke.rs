//! The framework's own check, not one of the thirty stories: add an
//! oscillator, wire it to the output, and hear it.

use std::time::Duration;

use egui::{Key, Modifiers, Pos2};
use noodle_core::Endpoint;
use noodle_engine::OUTPUT_ID;

use super::super::handle::{Handle, Probe, peak};
use super::super::{LateChange, Route, Tag};
use super::{add_node, empty_project};

pub fn route() -> Route {
    Route {
        story: "S00",
        tag: Tag::Default,
        name: "sine to output",
        fixture: empty_project,
        size: None,
        steps,
        done,
        late: &[LateChange {
            name: "stop the transport",
            steps: stop,
            done: stopped,
            keep: &[],
        }],
    }
}

fn steps(h: &mut Handle, _size: usize) {
    h.step("add the oscillator");
    let osc = add_node(h, Pos2::new(250.0, 400.0), "sine");
    h.step("add the output");
    let out = add_node(h, Pos2::new(650.0, 430.0), "output");
    h.step("wire them");
    let from = h.socket(osc, true, "out");
    let to = h.socket(out, false, "in");
    h.drag_between(Modifiers::NONE, from, to);
    h.step("play");
    h.press_playing(Pos2::new(640.0, 300.0), Modifiers::NONE, Key::Space);
}

fn done(p: &mut Probe, _size: usize) -> Result<(), String> {
    let graph = p.project().graph();
    let find = |type_id: &str| {
        graph
            .nodes()
            .find(|(_, n)| n.type_id == type_id)
            .map(|(id, _)| id)
    };
    let osc = find("noodle.osc.sine").ok_or("no sine oscillator")?;
    let out = find(OUTPUT_ID).ok_or("no output")?;
    if graph.source(&Endpoint::new(out, "in")) != Some(&Endpoint::new(osc, "out")) {
        return Err("the sine is not wired to the output".into());
    }
    let heard = p.hear(Duration::from_millis(300));
    let level = peak(&heard);
    if level < 0.01 {
        return Err(format!("nothing is heard (peak {level})"));
    }
    Ok(())
}

fn stop(h: &mut Handle, _size: usize) {
    h.step("stop");
    h.press_playing(Pos2::new(640.0, 300.0), Modifiers::NONE, Key::Space);
}

fn stopped(p: &mut Probe, _size: usize) -> Result<(), String> {
    if p.transport_running() {
        return Err("the transport still runs".into());
    }
    Ok(())
}
