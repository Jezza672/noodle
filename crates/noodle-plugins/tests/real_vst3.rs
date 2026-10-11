//! Real third-party VST3 plugins, when some are installed (on Debian and
//! Ubuntu: `dpf-plugins-vst3` and `dragonfly-reverb-vst3`, which put bundles
//! in `/usr/lib/vst3`). Each test says so and passes when there are none.

mod common;

use std::path::PathBuf;

use common::*;
use noodle_engine::testing::Harness;
use noodle_engine::{Config, Shape};
use noodle_plugins::{
    Catalog, PluginFile, PluginFormat, PluginNodeType, find_plugin_files, scan_file,
};

fn scanned() -> Vec<PluginFile> {
    let files: Vec<PathBuf> = find_plugin_files(&[PathBuf::from("/usr/lib/vst3")]);
    // CI sets this, so a change to its packages can't quietly skip the tests.
    assert!(
        !files.is_empty() || std::env::var_os("NOODLE_REQUIRE_REAL_PLUGINS").is_none(),
        "NOODLE_REQUIRE_REAL_PLUGINS is set but /usr/lib/vst3 has no plugins"
    );
    files.iter().map(|p| scan_file(&scanner(), p)).collect()
}

fn find<'a>(catalog: &'a Catalog, name: &str) -> Option<&'a PluginNodeType> {
    catalog.iter().find(|t| t.descriptor().name == name)
}

fn harness(node_type: &PluginNodeType, frames: usize) -> Harness {
    let connected: Vec<(usize, Shape)> = node_type
        .descriptor()
        .audio_inputs
        .iter()
        .enumerate()
        .map(|(i, p)| (i, Shape::new(1, p.channels as usize)))
        .collect();
    Harness::new(node_type, &Config::new(), &connected, 48_000.0, frames)
        .unwrap_or_else(|e| panic!("{}: {e:?}", node_type.descriptor().name))
}

fn fill_inputs(h: &mut Harness, node_type: &PluginNodeType, frames: usize) {
    for port in 0..node_type.descriptor().audio_inputs.len() {
        let mut input = h.input(port, frames);
        for channel in 0..input.shape().channels {
            for (i, s) in input.lane_mut(0, channel).iter_mut().enumerate() {
                *s = (i as f32 * 0.05).sin() * 0.3;
            }
        }
    }
}

#[test]
fn every_installed_plugin_scans_loads_and_makes_finite_sound() {
    let files = scanned();
    if files.is_empty() {
        eprintln!("no VST3 plugins in /usr/lib/vst3: skipped");
        return;
    }
    for file in &files {
        assert_eq!(file.error, None, "{}", file.path.display());
    }
    let catalog = Catalog::new(&files);
    assert!(!catalog.is_empty());
    for node_type in catalog.iter() {
        let d = node_type.descriptor();
        assert_eq!(d.format, PluginFormat::Vst3);
        assert!(d.node_id().starts_with("vst3."));
        let outputs = noodle_engine::NodeType::layout(node_type, &Config::new())
            .unwrap()
            .outputs
            .len();
        let mut h = harness(node_type, 256);
        for _ in 0..8 {
            fill_inputs(&mut h, node_type, 256);
            h.run(256).unwrap();
            for port in 0..outputs {
                let out = h.output(port);
                for channel in 0..out.shape().channels {
                    assert!(
                        out.lane(0, channel).iter().all(|s| s.is_finite()),
                        "{} produced a non-finite sample",
                        d.name
                    );
                }
            }
        }
    }
}

fn note_on(time: u32, id: u32, key: u8) -> noodle_engine::Event {
    noodle_engine::Event {
        time,
        kind: noodle_engine::EventKind::NoteOn {
            note: noodle_engine::NoteId(id),
            channel: 0,
            key,
            velocity: 1.0,
        },
    }
}

#[test]
fn installed_synths_make_sound_from_a_note() {
    let catalog = Catalog::new(&scanned());
    let mut tried = 0;
    for name in ["Nekobi", "Kars"] {
        let Some(node_type) = find(&catalog, name) else {
            continue;
        };
        tried += 1;
        let mut h = harness(node_type, 256);
        h.send_events(0, &[note_on(0, 1, 57)]);
        let mut loudest = 0.0f32;
        for block in 0..40 {
            h.run(256).unwrap();
            if block == 0 {
                h.send_events(0, &[]);
            }
            let out = h.output(0);
            for channel in 0..out.shape().channels {
                for &s in out.lane(0, channel) {
                    assert!(s.is_finite());
                    loudest = loudest.max(s.abs());
                }
            }
        }
        assert!(loudest > 0.01, "{name} stayed silent (peak {loudest})");
    }
    if tried == 0 {
        eprintln!("no installed synths found: skipped");
    }
}

#[test]
fn installed_plugins_save_state_that_restores_to_the_same_state() {
    let files = scanned();
    let mut tried = 0;
    for file in files.iter().filter(|f| f.error.is_none()).take(12) {
        for plugin in &file.plugins {
            let Ok(first) = noodle_plugins::capture_state(&file.path, &plugin.id, None) else {
                continue;
            };
            let again = noodle_plugins::capture_state(&file.path, &plugin.id, Some(&first))
                .unwrap_or_else(|e| panic!("{}: {e:?}", plugin.name));
            assert_eq!(first, again, "{}", plugin.name);
            tried += 1;
        }
    }
    if tried == 0 {
        eprintln!("no installed plugins with state: skipped");
    }
}
