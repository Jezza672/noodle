//! On macOS a plugin stays loaded for the life of the process: its Objective-C
//! objects can outlive its instance in the main thread's autorelease pool, and
//! releasing them after the plugin is deinitialised or unloaded crashes (the
//! Surge XT editor crash). The test plugin records its `deinit` in a file, so
//! this can be seen even though a Rust plugin library is never unmapped.
//!
//! This is its own test binary, so no other test loads the plugin meanwhile.

mod common;

use std::time::{Duration, Instant};

use common::*;
use noodle_core::NodeId;
use noodle_engine::Config;
use noodle_engine::testing::Harness;
use noodle_plugins::is_running;

#[test]
fn macos_never_deinitialises_a_plugin_but_other_systems_do() {
    let marker = std::env::temp_dir().join(format!("noodle-deinit-{}", std::process::id()));
    let _ = std::fs::remove_file(&marker);
    // SAFETY: nothing else in this single-test binary reads the environment
    // while this runs.
    unsafe { std::env::set_var("NOODLE_TEST_DEINIT_MARKER", &marker) };

    let catalog = catalog();
    let node = node_type(&catalog);
    let harness = Harness::new(node, &Config::new(), &[], 48_000.0, 256).unwrap();
    assert!(is_running(NodeId(0)));
    drop(harness);

    // The instance goes on the plugin thread; wait for that, then for a
    // deinit that might follow.
    let start = Instant::now();
    while is_running(NodeId(0)) && start.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(!is_running(NodeId(0)), "the instance is destroyed");
    let deadline = Instant::now() + Duration::from_secs(2);
    while !marker.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    let deinitialised = marker.exists();
    let _ = std::fs::remove_file(&marker);

    if cfg!(target_os = "macos") {
        assert!(!deinitialised, "macOS keeps the plugin loaded");
    } else {
        assert!(deinitialised, "other systems let the plugin go");
    }
}
