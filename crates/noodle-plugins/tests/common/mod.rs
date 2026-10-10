//! Shared by the integration tests: the test plugin and the scanner.

#![allow(dead_code)]

use std::path::PathBuf;

use noodle_plugins::{Catalog, ClapNodeType, ScanCommand, scan_file};

/// The test plugin built from `examples/test_plugin.rs`. `cargo test` builds
/// examples first, so it's next to the test binary's `deps` folder.
pub fn plugin_path() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let target = exe.parent().unwrap().parent().unwrap();
    let path = target.join("examples").join(format!(
        "{}test_plugin{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_SUFFIX
    ));
    assert!(path.exists(), "build the example first: {}", path.display());
    path
}

pub fn scanner() -> ScanCommand {
    ScanCommand::program(env!("CARGO_BIN_EXE_noodle-plugin-scan"))
}

pub const PLUGIN_ID: &str = "noodle.test.gain-sine";

/// The catalog of just the test plugin, scanned like the app does.
pub fn catalog() -> Catalog {
    let file = scan_file(&scanner(), &plugin_path());
    assert_eq!(file.error, None);
    Catalog::new(&[file])
}

pub fn node_type(catalog: &Catalog) -> &ClapNodeType {
    catalog
        .iter()
        .find(|t| t.descriptor().id == PLUGIN_ID)
        .expect("test plugin in catalog")
}
