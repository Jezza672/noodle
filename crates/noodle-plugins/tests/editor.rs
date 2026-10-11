//! Editors and capturing a live plugin, through the shared plugin thread.
//!
//! The test plugin's editor draws nothing, but a timer it asks the host for
//! plays the user: while the editor is shown, the first tick turns `Gain`
//! down to 0.25. So these tests prove the host creates and shows the editor,
//! runs the plugin's timers on the plugin thread, and reads parameters and
//! state back from the running instance.

mod common;

use std::time::{Duration, Instant};

use base64::Engine as _;
use common::*;
use noodle_core::{Command, NodeId};
use noodle_engine::{Config, Instance, NodeType, Setup, Value};
use noodle_plugins::{
    Captured, STATE_KEY, capture, close_editor, editor_is_open, is_running, open_editor,
};

const GAIN: u32 = 1;

fn build(node_type: &dyn NodeType, node: NodeId, config: &Config) -> Instance {
    node_type
        .instantiate(&Setup {
            node,
            config,
            sample_rate: 48_000.0,
            max_frames: 256,
            input_shapes: &[],
            output_shapes: &[],
            seed: 0,
            position: 0,
        })
        .expect("instantiate")
}

fn gain(captured: &Captured) -> f64 {
    captured
        .params
        .iter()
        .find(|(id, _)| *id == GAIN)
        .expect("gain")
        .1
}

fn until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < deadline, "timed out: {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn a_running_plugin_can_be_captured_and_stops_being_running_when_dropped() {
    let catalog = catalog();
    let node_type = node_type(&catalog);
    let node = NodeId(9_001);
    assert!(!is_running(node));
    let saved = 0.5f32.to_le_bytes();
    let text = base64::engine::general_purpose::STANDARD.encode(saved);
    let config = Config::new().with(STATE_KEY, Value::Text(text));
    let instance = build(node_type, node, &config);
    assert!(is_running(node));

    let captured = capture(node).unwrap();
    assert_eq!(captured.state, saved, "the state it was built with");
    assert_eq!(gain(&captured), 1.0, "Gain's default");

    drop(instance);
    assert!(!is_running(node));
    assert!(capture(node).is_err());
    assert!(open_editor(node).is_err());
}

#[test]
fn edits_made_in_the_editor_are_captured_as_parameter_values() {
    let catalog = catalog();
    let node_type = node_type(&catalog);
    let node = NodeId(9_002);
    let instance = build(node_type, node, &Config::new());

    assert!(!editor_is_open(node));
    open_editor(node).expect("the test plugin has a floating editor");
    assert!(editor_is_open(node));
    // The plugin's timer, run by the host on the plugin thread.
    until("the user turns Gain down", || {
        gain(&capture(node).unwrap()) == 0.25
    });

    close_editor(node);
    until("the editor closes", || !editor_is_open(node));

    // What the app stores: the state, and Gain on its port.
    let captured = capture(node).unwrap();
    let Command::Batch(commands) = captured.commands(node, node_type.descriptor()) else {
        panic!("a batch");
    };
    assert!(commands.iter().any(|c| matches!(
        c,
        Command::SetConfig { key, value: Some(Value::Text(_)), .. } if key == STATE_KEY
    )));
    assert!(commands.iter().any(|c| matches!(
        c,
        Command::SetParam { key, value: Some(v), .. } if key == "p1" && *v == 0.25
    )));
    drop(instance);
}

#[test]
fn the_editor_can_be_opened_again_after_it_closes() {
    let catalog = catalog();
    let node_type = node_type(&catalog);
    let node = NodeId(9_003);
    let _instance = build(node_type, node, &Config::new());
    for _ in 0..3 {
        open_editor(node).unwrap();
        assert!(editor_is_open(node));
        close_editor(node);
        until("the editor closes", || !editor_is_open(node));
    }
}

#[test]
fn many_instances_load_and_close_from_many_threads_on_the_one_plugin_thread() {
    let catalog = catalog();
    let node_type = node_type(&catalog);
    std::thread::scope(|scope| {
        for n in 0..8u64 {
            scope.spawn(move || {
                let node = NodeId(9_100 + n);
                let instance = build(node_type, node, &Config::new());
                assert!(capture(node).is_ok());
                drop(instance);
                assert!(!is_running(node));
            });
        }
    });
}

#[test]
fn an_editor_is_followed_by_its_instance_even_when_the_node_is_built_again() {
    let catalog = catalog();
    let node_type = node_type(&catalog);
    let node = NodeId(9_200);
    let live = build(node_type, node, &Config::new());
    let key = open_editor(node).unwrap();
    assert!(noodle_plugins::editor_key_is_open(key));

    // A render builds the same node afterwards. Asking by node would now
    // find the render, but the key still means the instance with the editor.
    let render = build(node_type, node, &Config::new());
    assert!(noodle_plugins::editor_key_is_open(key));
    until("the user turns Gain down", || {
        gain(&noodle_plugins::capture_key(key).unwrap()) == 0.25
    });
    assert_eq!(
        gain(&capture(node).unwrap()),
        1.0,
        "the render is untouched"
    );

    drop(render);
    close_editor(node); // the node's newest is now the live one
    until("the editor closes", || {
        !noodle_plugins::editor_key_is_open(key)
    });
    drop(live);
    assert!(!noodle_plugins::editor_instance_running(key));
}
