//! Without a window to embed in (as on Linux, where the app makes none yet),
//! a VST3 editor can't open, and says why. A binary of its own: the window
//! factory is process-wide.

mod common;

use common::*;
use noodle_core::NodeId;
use noodle_engine::testing::Harness;
use noodle_engine::{Config, Shape};
use noodle_plugins::{capture, editor_is_open, open_editor};

#[test]
fn an_editor_without_a_window_says_so_and_the_plugin_keeps_working() {
    let catalog = vst3_catalog();
    let node_type = vst3_node_type(&catalog);
    let mut h = Harness::new(
        node_type,
        &Config::new(),
        &[(0, Shape::STEREO)],
        48_000.0,
        256,
    )
    .expect("harness");
    let error = open_editor(NodeId(0)).expect_err("no window");
    assert!(error.to_string().contains("window"), "{error}");
    assert!(!editor_is_open(NodeId(0)));
    // Capturing still works, and the plugin still runs.
    assert!(capture(NodeId(0)).is_ok());
    h.run(256).unwrap();
}
