//! Shared by the integration tests: the test plugin and the scanner.

#![allow(dead_code)]

use std::path::PathBuf;

use noodle_plugins::{Catalog, PluginNodeType, ScanCommand, scan_file};

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

pub fn node_type(catalog: &Catalog) -> &PluginNodeType {
    catalog
        .iter()
        .find(|t| t.descriptor().id == PLUGIN_ID)
        .expect("test plugin in catalog")
}

/// The VST3 test plugin (`examples/test_vst3.rs`) as a bundle, made once in
/// the target folder in the layout each system uses.
///
/// Nextest gives every test its own process, so `Once` only stops this process
/// repeating the work: others may be making the bundle at the same moment.
/// Each file therefore appears whole (a private name, then a rename), and the
/// plist goes before the library, so a bundle that has its library scans.
pub fn vst3_path() -> PathBuf {
    use std::sync::Once;
    static MADE: Once = Once::new();
    let exe = std::env::current_exe().unwrap();
    let target = exe.parent().unwrap().parent().unwrap();
    let library = target.join("examples").join(format!(
        "{}test_vst3{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_SUFFIX
    ));
    assert!(
        library.exists(),
        "build the example first: {}",
        library.display()
    );
    let bundle = target.join("vst3-bundle").join("NoodleTest.vst3");
    MADE.call_once(|| {
        let (folder, name) = if cfg!(target_os = "macos") {
            ("MacOS", "NoodleTest")
        } else if cfg!(windows) {
            ("x86_64-win", "NoodleTest.vst3")
        } else {
            ("x86_64-linux", "NoodleTest.so")
        };
        let dir = bundle.join("Contents").join(folder);
        std::fs::create_dir_all(&dir).unwrap();
        if cfg!(target_os = "macos") {
            put(&bundle.join("Contents/Info.plist"), |path| {
                std::fs::write(
                    path,
                    r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
<key>CFBundleExecutable</key><string>NoodleTest</string>
<key>CFBundleIdentifier</key><string>org.noodle.test</string>
<key>CFBundlePackageType</key><string>BNDL</string>
</dict></plist>"#,
                )
                .unwrap();
            });
        }
        put(&dir.join(name), |path| {
            std::fs::copy(&library, path).unwrap();
        });
    });
    bundle
}

/// Makes `path` appear whole: writes it under a private name, then renames it
/// into place. Another process may be making the same file, so one that is
/// already there is kept — either is the same bytes, and Windows cannot
/// replace a library another process has loaded.
fn put(path: &std::path::Path, write: impl FnOnce(&std::path::Path)) {
    if path.exists() {
        return;
    }
    let name = path.file_name().unwrap().to_string_lossy().into_owned();
    let partial = path.with_file_name(format!(".{name}.{}.partial", std::process::id()));
    write(&partial);
    if std::fs::rename(&partial, path).is_err() {
        let _ = std::fs::remove_file(&partial);
        assert!(path.exists(), "{} was not made", path.display());
    }
}

pub fn vst3_catalog() -> Catalog {
    let file = scan_file(&scanner(), &vst3_path());
    assert_eq!(file.error, None);
    Catalog::new(&[file])
}

pub fn vst3_node_type(catalog: &Catalog) -> &PluginNodeType {
    catalog
        .iter()
        .find(|t| t.descriptor().name == "Noodle Test VST3")
        .expect("test VST3 plugin in catalog")
}

/// The VST3 test plugin's library inside its bundle (the file the host loads,
/// so its counters are the ones the host's calls move).
pub fn vst3_library() -> PathBuf {
    let bundle = vst3_path();
    let (folder, name) = if cfg!(target_os = "macos") {
        ("MacOS", "NoodleTest")
    } else if cfg!(windows) {
        ("x86_64-win", "NoodleTest.vst3")
    } else {
        ("x86_64-linux", "NoodleTest.so")
    };
    bundle.join("Contents").join(folder).join(name)
}

/// How many times the host has called `process` on the test plugin in
/// `library`, which loading again shares with the host's copy.
pub fn process_calls(library: &std::path::Path) -> u64 {
    // SAFETY: the test plugins export this plain function.
    unsafe {
        let library = libloading::Library::new(library).expect("load the plugin");
        let calls: libloading::Symbol<unsafe extern "C" fn() -> u64> = library
            .get(b"noodle_test_process_calls")
            .expect("the counter");
        calls()
    }
}
