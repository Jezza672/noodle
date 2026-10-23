//! The plugin thread sleeps while nothing needs it, and wakes when asked.
//!
//! Its own test binary: it counts wakes of the process's one plugin thread.
//! Skipped where the application's own thread is the plugin thread.

mod common;

use std::time::Duration;

use common::*;
use noodle_core::NodeId;
use noodle_engine::{Config, NodeType, Setup};
use noodle_plugins::{capture, thread_wakes};

#[test]
fn an_idle_plugin_thread_does_not_wake_until_asked() {
    let catalog = catalog();
    let node = NodeId(9_201);
    let _instance = node_type(&catalog)
        .instantiate(&Setup {
            node,
            config: &Config::new(),
            sample_rate: 48_000.0,
            max_frames: 256,
            input_shapes: &[],
            output_shapes: &[],
            seed: 0,
            position: 0,
        })
        .expect("instantiate");
    // Let the load settle: it is itself a command.
    std::thread::sleep(Duration::from_millis(200));
    let idle = thread_wakes();
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(
        thread_wakes(),
        idle,
        "a loaded plugin with no timer and no editor needs no wake"
    );

    capture(node).expect("answers");
    assert!(thread_wakes() > idle, "a command wakes it");
}
