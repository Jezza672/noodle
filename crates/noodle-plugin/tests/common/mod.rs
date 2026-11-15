//! Shared by the plugin's test binaries.

use std::path::PathBuf;

/// `cargo test` links the plugin's rlib but doesn't make the library a host
/// loads, so the first test builds it, in the same profile and folder.
pub fn plugin_path() -> PathBuf {
    static BUILT: std::sync::Once = std::sync::Once::new();
    let exe = std::env::current_exe().unwrap();
    let target = exe.parent().unwrap().parent().unwrap();
    BUILT.call_once(|| {
        let mut build = std::process::Command::new(env!("CARGO"));
        build.args(["build", "-p", "noodle-plugin", "--lib"]);
        if target.file_name().is_some_and(|name| name == "release") {
            build.arg("--release");
        }
        let status = build.status().expect("run cargo");
        assert!(status.success(), "building the plugin failed");
    });
    let path = target.join(format!(
        "{}noodle_plugin{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_SUFFIX
    ));
    assert!(path.exists(), "the plugin isn't built: {}", path.display());
    path
}
