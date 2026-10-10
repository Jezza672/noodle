//! Scanning plugin files in a child process.

mod common;

use std::time::Duration;

use common::*;
use noodle_plugins::{ScanCache, ScanCommand, find_plugin_files, scan_file};

#[test]
fn scanning_reports_the_plugins_ports_and_parameters() {
    let file = scan_file(&scanner(), &plugin_path());
    assert_eq!(file.error, None);
    let [plugin] = file.plugins.as_slice() else {
        panic!("{:?}", file.plugins);
    };
    assert_eq!(plugin.id, PLUGIN_ID);
    assert_eq!(plugin.name, "Noodle Test Plugin");
    assert_eq!(plugin.vendor, "Noodle");
    assert!(plugin.instrument);
    assert_eq!(plugin.audio_inputs.len(), 1);
    assert_eq!(plugin.audio_inputs[0].channels, 2);
    assert_eq!(plugin.audio_outputs[0].channels, 2);
    assert_eq!((plugin.note_inputs, plugin.note_outputs), (1, 0));
    let params: Vec<_> = plugin
        .params
        .iter()
        .map(|p| (p.id, p.name.as_str(), p.min, p.max, p.default, p.stepped))
        .collect();
    assert_eq!(
        params,
        [
            (1, "Gain", 0.0, 2.0, 1.0, false),
            (2, "Invert", 0.0, 1.0, 0.0, true)
        ]
    );
    assert!(file.is_current());
}

#[test]
fn a_file_that_is_not_a_plugin_is_reported_not_fatal() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("broken.clap");
    std::fs::write(&path, b"this is not a shared library").unwrap();
    let file = scan_file(&scanner(), &path);
    assert!(file.plugins.is_empty());
    let error = file.error.expect("an error");
    assert!(error.contains("scanner failed"), "{error}");
}

#[test]
fn a_missing_scanner_is_an_error() {
    let command = ScanCommand::program("/definitely/not/a/program");
    let file = scan_file(&command, &plugin_path());
    assert!(file.error.unwrap().contains("can't start"));
}

/// Run by `a_scanner_that_hangs_is_killed` as the child: sleeps until killed.
#[test]
#[ignore = "only runs as the hanging child of another test"]
fn hang_forever() {
    std::thread::sleep(Duration::from_secs(120));
}

#[test]
fn a_scanner_that_hangs_is_killed_at_the_timeout() {
    let command = ScanCommand {
        program: std::env::current_exe().unwrap(),
        args: ["--ignored", "--exact", "hang_forever", "--nocapture", "--"]
            .map(Into::into)
            .to_vec(),
        timeout: Duration::from_millis(300),
    };
    let started = std::time::Instant::now();
    let file = scan_file(&command, &plugin_path());
    assert!(file.error.unwrap().contains("timed out"));
    assert!(started.elapsed() < Duration::from_secs(20));
}

#[test]
fn the_cache_rescans_only_changed_files() {
    let dir = tempfile::tempdir().unwrap();
    let copy = dir.path().join("copy.clap");
    std::fs::copy(plugin_path(), &copy).unwrap();
    let mut cache = ScanCache::default();
    assert_eq!(cache.refresh(&scanner(), std::slice::from_ref(&copy)), 1);
    assert_eq!(cache.files[0].plugins.len(), 1);

    // Saved and loaded, nothing is scanned again.
    let saved = dir.path().join("cache/plugins.json");
    cache.save(&saved).unwrap();
    let mut loaded = ScanCache::load(&saved);
    assert_eq!(loaded, cache);
    assert_eq!(loaded.refresh(&scanner(), std::slice::from_ref(&copy)), 0);

    // A changed file is scanned again; a deleted one drops out.
    let mut bytes = std::fs::read(&copy).unwrap();
    bytes.push(0);
    std::fs::write(&copy, bytes).unwrap();
    assert_eq!(loaded.refresh(&scanner(), std::slice::from_ref(&copy)), 1);
    std::fs::remove_file(&copy).unwrap();
    assert_eq!(loaded.refresh(&scanner(), &[]), 0);
    assert!(loaded.files.is_empty());
}

#[test]
fn finding_files_looks_for_clap_files_in_folders() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("vendor")).unwrap();
    std::fs::write(dir.path().join("a.clap"), b"").unwrap();
    std::fs::write(dir.path().join("vendor/b.clap"), b"").unwrap();
    std::fs::write(dir.path().join("vendor/readme.txt"), b"").unwrap();
    let found = find_plugin_files(&[dir.path().to_owned(), "/no/such/folder".into()]);
    let names: Vec<_> = found
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["a.clap", "b.clap"]);
}
