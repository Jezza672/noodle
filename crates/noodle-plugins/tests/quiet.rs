//! A plugin with nothing to do isn't called, and its outputs say so: the
//! silence skipping of the rest of the graph reaches plugins (CLAP and VST3;
//! the sandbox has its own test).
//!
//! The test plugins count their `process` calls, and say they are done
//! (CLAP `Sleep`, VST3 silence flags on every output) when a block had no
//! input, events or held notes. Both share a process-wide counter, so the
//! tests take turns.

mod common;

use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, PoisonError};

use common::*;
use noodle_engine::testing::Harness;
use noodle_engine::{Config, Event, EventKind, NodeType, NoteId, Shape};

static TURN: Mutex<()> = Mutex::new(());

const RATE: f32 = 48_000.0;
const FRAMES: usize = 256;
const IN: usize = 0;
const GAIN: usize = 1;

/// One second of blocks.
const SECOND: usize = RATE as usize / FRAMES;

/// A plugin under test: how to load it and where its counter is.
struct Rig {
    _turn: MutexGuard<'static, ()>,
    harness: Harness,
    library: PathBuf,
}

fn clap() -> Rig {
    let turn = TURN.lock().unwrap_or_else(PoisonError::into_inner);
    let catalog = catalog();
    let harness = harness(node_type(&catalog));
    Rig {
        _turn: turn,
        harness,
        library: plugin_path(),
    }
}

fn vst3() -> Rig {
    let turn = TURN.lock().unwrap_or_else(PoisonError::into_inner);
    let catalog = vst3_catalog();
    let harness = harness(vst3_node_type(&catalog));
    Rig {
        _turn: turn,
        harness,
        library: vst3_library(),
    }
}

fn harness(node: &dyn NodeType) -> Harness {
    Harness::new(node, &Config::new(), &[(IN, Shape::STEREO)], RATE, FRAMES).expect("harness")
}

impl Rig {
    /// A block of silence, flagged as such like a node upstream would.
    fn silent_block(&mut self) {
        {
            let mut input = self.harness.input(IN, FRAMES);
            for channel in 0..2 {
                input.silence(0, channel);
            }
        }
        self.harness.run(FRAMES).unwrap();
    }

    fn calls(&self) -> u64 {
        process_calls(&self.library)
    }
}

fn note(time: u32, on: bool) -> Event {
    Event {
        time,
        kind: if on {
            EventKind::NoteOn {
                note: NoteId(1),
                channel: 0,
                key: 69,
                velocity: 1.0,
            }
        } else {
            EventKind::NoteOff {
                note: NoteId(1),
                velocity: 0.0,
            }
        },
    }
}

fn idle_plugin_is_left_alone(mut rig: Rig) {
    // Its tail is nothing, so a call to find that out and one more.
    let before = rig.calls();
    for _ in 0..SECOND {
        rig.silent_block();
    }
    let calls = rig.calls() - before;
    assert!(calls <= 2, "a quiet plugin was called {calls} times");
    let out = rig.harness.output(0);
    assert!(out.is_silent(0, 0) && out.is_silent(0, 1), "flagged silent");
    assert!(out.lane(0, 0).iter().all(|&s| s == 0.0));
}

fn note_on_wakes_the_plugin_in_the_same_block(mut rig: Rig) {
    for _ in 0..8 {
        rig.silent_block();
    }
    let before = rig.calls();
    rig.silent_block();
    assert_eq!(rig.calls(), before, "asleep");

    rig.harness.send_events(0, &[note(10, true)]);
    rig.silent_block();
    assert_eq!(rig.calls(), before + 1, "woken by the note");
    let out = rig.harness.output(0);
    assert!(!out.is_silent(0, 0), "not flagged silent while it sounds");
    assert!(
        out.lane(0, 0)[100..].iter().any(|&s| s.abs() > 0.01),
        "the note sounds in the block it arrived in"
    );

    // Held, it keeps running; released, it goes quiet again.
    rig.harness.send_events(0, &[]);
    rig.silent_block();
    assert_eq!(rig.calls(), before + 2);
    rig.harness.send_events(0, &[note(0, false)]);
    rig.silent_block();
    rig.harness.send_events(0, &[]);
    for _ in 0..8 {
        rig.silent_block();
    }
    assert!(rig.calls() <= before + 5, "quiet again after the release");
    assert!(rig.harness.output(0).is_silent(0, 0));
}

fn a_parameter_change_wakes_the_plugin(mut rig: Rig) {
    for _ in 0..8 {
        rig.silent_block();
    }
    let before = rig.calls();
    rig.silent_block();
    assert_eq!(rig.calls(), before, "asleep");
    rig.harness.set(GAIN, 0.25);
    rig.silent_block();
    assert_eq!(rig.calls(), before + 1, "woken by the parameter");
}

fn sound_in_wakes_the_plugin(mut rig: Rig) {
    for _ in 0..8 {
        rig.silent_block();
    }
    {
        let mut input = rig.harness.input(IN, FRAMES);
        for channel in 0..2 {
            input.lane_mut(0, channel).fill(0.25);
        }
    }
    rig.harness.run(FRAMES).unwrap();
    let out = rig.harness.output(0);
    assert!(!out.is_silent(0, 0));
    assert!(out.lane(0, 0).iter().all(|&s| (s - 0.25).abs() < 1e-6));
}

#[test]
fn clap_idle_plugin_is_left_alone() {
    idle_plugin_is_left_alone(clap());
}

#[test]
fn clap_note_on_wakes_the_plugin_in_the_same_block() {
    note_on_wakes_the_plugin_in_the_same_block(clap());
}

#[test]
fn clap_a_parameter_change_wakes_the_plugin() {
    a_parameter_change_wakes_the_plugin(clap());
}

#[test]
fn clap_sound_in_wakes_the_plugin() {
    sound_in_wakes_the_plugin(clap());
}

#[test]
fn vst3_idle_plugin_is_left_alone() {
    idle_plugin_is_left_alone(vst3());
}

#[test]
fn vst3_note_on_wakes_the_plugin_in_the_same_block() {
    note_on_wakes_the_plugin_in_the_same_block(vst3());
}

#[test]
fn vst3_a_parameter_change_wakes_the_plugin() {
    a_parameter_change_wakes_the_plugin(vst3());
}

#[test]
fn vst3_sound_in_wakes_the_plugin() {
    sound_in_wakes_the_plugin(vst3());
}
