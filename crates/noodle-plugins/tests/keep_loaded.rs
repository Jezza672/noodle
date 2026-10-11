//! On macOS a plugin stays loaded for the life of the process: its Objective-C
//! objects can outlive its instance in the main thread's autorelease pool, and
//! releasing them after the plugin is deinitialised or unloaded crashes (the
//! Surge XT editor crash). The test plugins record their `deinit` (CLAP) or
//! module exit (VST3) in a file, so this can be seen even though a Rust
//! plugin library is never unmapped.
//!
//! This is its own test binary, so no other test loads a plugin meanwhile.

mod common;

use std::path::Path;
use std::time::{Duration, Instant};

use common::*;
use noodle_core::NodeId;
use noodle_engine::Config;
use noodle_engine::testing::Harness;
use noodle_plugins::{Catalog, PluginNodeType, is_running};

/// Builds and drops a node of `node_type` and says whether the plugin was
/// deinitialised (the marker file appeared).
fn deinitialised_after_drop(catalog: &Catalog, pick: fn(&Catalog) -> &PluginNodeType) -> bool {
    let marker = Path::new(&std::env::var_os("NOODLE_TEST_DEINIT_MARKER").unwrap()).to_owned();
    let _ = std::fs::remove_file(&marker);
    let harness = Harness::new(pick(catalog), &Config::new(), &[], 48_000.0, 256).unwrap();
    drop(harness);
    // A CLAP instance goes on the plugin thread; wait for that, then for a
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
    let found = marker.exists();
    let _ = std::fs::remove_file(&marker);
    found
}

#[test]
fn macos_never_deinitialises_a_plugin_but_other_systems_do() {
    // Scan first: the scanner is a child process that loads and unloads the
    // plugin itself, and would write the marker if it saw the variable.
    let clap = catalog();
    let vst3 = vst3_catalog();
    let marker = std::env::temp_dir().join(format!("noodle-deinit-{}", std::process::id()));
    // SAFETY: nothing else in this single-test binary reads the environment
    // while this runs.
    unsafe { std::env::set_var("NOODLE_TEST_DEINIT_MARKER", &marker) };

    let clap_gone = deinitialised_after_drop(&clap, node_type);
    let vst3_gone = deinitialised_after_drop(&vst3, vst3_node_type);

    if cfg!(target_os = "macos") {
        assert!(!clap_gone, "macOS keeps the CLAP plugin loaded");
        assert!(!vst3_gone, "macOS keeps the VST3 plugin loaded");
    } else {
        assert!(clap_gone, "other systems let the CLAP plugin go");
        assert!(vst3_gone, "other systems let the VST3 plugin go");
    }
}
