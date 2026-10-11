//! Finding and scanning VST3 plugins.

mod common;

use common::*;
use noodle_plugins::{PluginFile, PluginFormat, find_plugin_files, scan_file};

#[test]
fn a_vst3_bundle_is_found_as_one_file_and_not_searched() {
    let dir = tempfile::tempdir().unwrap();
    let bundle = dir.path().join("Synth.vst3");
    std::fs::create_dir_all(bundle.join("Contents/x86_64-linux")).unwrap();
    std::fs::write(bundle.join("Contents/x86_64-linux/Synth.so"), b"x").unwrap();
    // Another vendor's folder, with a plain module and a CLAP file in it.
    let vendor = dir.path().join("Vendor");
    std::fs::create_dir_all(&vendor).unwrap();
    std::fs::write(vendor.join("Fx.vst3"), b"x").unwrap();
    std::fs::write(vendor.join("Other.clap"), b"x").unwrap();
    std::fs::write(vendor.join("notes.txt"), b"x").unwrap();
    let found = find_plugin_files(&[dir.path().to_owned()]);
    assert_eq!(
        found,
        [bundle, vendor.join("Fx.vst3"), vendor.join("Other.clap")]
    );
}

#[test]
fn scanning_a_vst3_bundle_describes_its_plugin() {
    let file = scan_file(&scanner(), &vst3_path());
    assert_eq!(file.error, None);
    // The factory also lists the controller class; only the plugin counts.
    assert_eq!(file.plugins.len(), 1);
    let plugin = &file.plugins[0];
    assert_eq!(plugin.format, PluginFormat::Vst3);
    assert_eq!(plugin.name, "Noodle Test VST3");
    assert_eq!(plugin.vendor, "Noodle");
    assert_eq!(plugin.id.len(), 32);
    assert!(plugin.id.bytes().all(|b| b.is_ascii_hexdigit()));
    assert_eq!(plugin.node_id(), format!("vst3.{}", plugin.id));
    assert_eq!(
        (plugin.audio_inputs.len(), plugin.audio_outputs.len()),
        (1, 1)
    );
    assert_eq!(plugin.audio_inputs[0].channels, 2);
    assert_eq!((plugin.note_inputs, plugin.note_outputs), (2, 0));
    // The hidden parameter is left out.
    let params: Vec<_> = plugin.params.iter().map(|p| (p.id, p.stepped)).collect();
    assert_eq!(params, [(1, false), (2, true)]);
    assert_eq!(plugin.params[0].name, "Gain");
    assert_eq!(plugin.params[0].default, 0.5);
}

#[test]
fn a_file_that_is_not_a_vst3_module_fails_to_scan_with_a_reason() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("Fake.vst3");
    std::fs::write(&path, b"not a library").unwrap();
    let file = scan_file(&scanner(), &path);
    assert!(file.plugins.is_empty());
    assert!(file.error.is_some());
    // A bundle with no binary in it.
    let empty = dir.path().join("Empty.vst3");
    std::fs::create_dir_all(empty.join("Contents")).unwrap();
    let file = scan_file(&scanner(), &empty);
    assert!(file.error.is_some());
}

#[test]
fn scans_saved_before_vst3_still_load_as_clap() {
    let file = scan_file(&scanner(), &plugin_path());
    let mut json: serde_json::Value = serde_json::to_value(&file).unwrap();
    for plugin in json["plugins"].as_array_mut().unwrap() {
        plugin.as_object_mut().unwrap().remove("format");
    }
    let old: PluginFile = serde_json::from_value(json).unwrap();
    assert_eq!(old.plugins[0].format, PluginFormat::Clap);
    assert_eq!(old.plugins[0].node_id(), "clap.noodle.test.gain-sine");
}
