//! A plugin's reported latency reaches the engine, which compensates for it
//! (see `noodle-nodes/tests/pdc.rs` for the compensation itself).
//!
//! The test plugins report the latency in `NOODLE_TEST_PLUGIN_LATENCY`, so
//! this file sets it once, before any plugin loads; it is its own process.

mod common;

use common::*;
use noodle_core::Project;
use noodle_engine::{NodeType, Registry, Settings, engine};
use noodle_plugins::Catalog;

const SETTINGS: Settings = Settings {
    sample_rate: 48_000.0,
    max_frames: 512,
    channels: 2,
};

const LATENCY: u32 = 777;

fn project(type_id: &str) -> Project {
    Project::from_ron(&format!(
        r#"(
    format: 1,
    graph: (
        nodes: {{
            1: (type: "noodle.osc.sine", params: {{"frequency": 440.0}}, position: (x: 0.0, y: 0.0)),
            2: (type: "{type_id}", position: (x: 200.0, y: 0.0)),
            3: (type: "noodle.osc.sine", params: {{"frequency": 220.0}}, position: (x: 0.0, y: 100.0)),
            4: (type: "noodle.util.mix", position: (x: 400.0, y: 0.0)),
            5: (type: "noodle.io.output", position: (x: 600.0, y: 0.0)),
        }},
        connections: [
            (from: (node: 1, port: "out"), to: (node: 2, port: "in")),
            (from: (node: 2, port: "out"), to: (node: 4, port: "in1")),
            (from: (node: 3, port: "out"), to: (node: 4, port: "in2")),
            (from: (node: 4, port: "out"), to: (node: 5, port: "in")),
        ],
    ),
)"#
    ))
    .unwrap()
}

fn output_latency(catalog: &Catalog, type_id: &str) -> usize {
    let mut registry = Registry::with_builtins();
    let _telemetry = noodle_nodes::register_all(&mut registry);
    catalog.register(&mut registry);
    let (mut controller, _processor) = engine(SETTINGS).unwrap();
    let diagnostics = controller.update_project(&project(type_id), &registry);
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    controller.output_latency()
}

#[test]
fn plugins_report_their_latency_to_the_engine() {
    // SAFETY: nothing else in this process reads the environment yet, and
    // the one test in this file runs alone.
    unsafe { std::env::set_var("NOODLE_TEST_PLUGIN_LATENCY", LATENCY.to_string()) };
    assert_eq!(
        output_latency(&catalog(), "clap.noodle.test.gain-sine"),
        LATENCY as usize
    );
    let vst3 = vst3_catalog();
    let id = vst3_node_type(&vst3).info().id;
    assert_eq!(output_latency(&vst3, id), LATENCY as usize);
}
