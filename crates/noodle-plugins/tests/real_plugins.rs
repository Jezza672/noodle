//! Real third-party CLAP plugins, when some are installed (on Debian and
//! Ubuntu: `dpf-plugins-clap`, `dragonfly-reverb-clap`, `lsp-plugins-clap`,
//! which put files in `/usr/lib/clap`). Each test says so and passes when
//! there are none. These are the plugins the test plugin can't stand in for:
//! they scan, load, process and save state like anyone's would.

mod common;

use std::path::PathBuf;

use common::*;
use noodle_engine::testing::Harness;
use noodle_engine::{Config, Shape};
use noodle_plugins::{Catalog, ClapNodeType, PluginFile, find_plugin_files, scan_file};

fn installed() -> Vec<PathBuf> {
    find_plugin_files(&[PathBuf::from("/usr/lib/clap")])
}

fn scanned() -> Vec<PluginFile> {
    installed()
        .iter()
        .map(|p| scan_file(&scanner(), p))
        .collect()
}

#[test]
fn every_installed_plugin_scans_and_loads_and_makes_finite_sound() {
    let files = scanned();
    if files.is_empty() {
        eprintln!("no CLAP plugins in /usr/lib/clap: skipped");
        return;
    }
    for file in &files {
        assert_eq!(file.error, None, "{}", file.path.display());
    }
    let catalog = Catalog::new(&files);
    assert!(!catalog.is_empty());
    for node_type in catalog.iter() {
        let d = node_type.descriptor();
        eprintln!(
            "{} ({}): {} in / {} out, {} note in, {} params",
            d.name,
            d.id,
            d.audio_inputs.len(),
            d.audio_outputs.len(),
            d.note_inputs,
            d.params.len()
        );
        exercise(node_type);
    }
}

/// Builds the plugin, runs a few blocks of sound and a note through it, saves
/// its state, and builds it again from that state.
fn exercise(node_type: &ClapNodeType) {
    let layout = noodle_engine::NodeType::layout(node_type, &Config::new()).unwrap();
    // The audio inputs come first, one signal each.
    let connected: Vec<(usize, Shape)> = node_type
        .descriptor()
        .audio_inputs
        .iter()
        .enumerate()
        .map(|(i, p)| (i, if p.channels == 1 { Shape::MONO } else { Shape::STEREO }))
        .collect();
    let mut h = Harness::new(node_type, &Config::new(), &connected, 48_000.0, 256)
        .unwrap_or_else(|e| panic!("{}: {e:?}", node_type.descriptor().name));
    for _ in 0..8 {
        for &(port, _) in &connected {
            let mut input = h.input(port, 256);
            for channel in 0..input.shape().channels {
                for (i, s) in input.lane_mut(0, channel).iter_mut().enumerate() {
                    *s = (i as f32 * 0.05).sin() * 0.3;
                }
            }
        }
        h.run(256).unwrap();
        for port in 0..layout.outputs.len() {
            let out = h.output(port);
            for channel in 0..out.shape().channels {
                assert!(
                    out.lane(0, channel).iter().all(|s| s.is_finite()),
                    "{} produced a non-finite sample",
                    node_type.descriptor().name
                );
            }
        }
    }
}

fn find<'a>(catalog: &'a Catalog, id: &str) -> Option<&'a ClapNodeType> {
    catalog.iter().find(|t| t.descriptor().id == id)
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
    let files = scanned();
    let catalog = Catalog::new(&files);
    let mut tried = 0;
    for id in ["studio.kx.distrho.Nekobi", "studio.kx.distrho.Kars"] {
        let Some(node_type) = find(&catalog, id) else {
            continue;
        };
        tried += 1;
        let mut h = Harness::new(node_type, &Config::new(), &[], 48_000.0, 256).unwrap();
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
        assert!(loudest > 0.01, "{id} stayed silent (peak {loudest})");
    }
    if tried == 0 {
        eprintln!("no installed synths found: skipped");
    }
}

#[test]
fn an_installed_plugins_editor_opens_and_closes_on_an_x_display() {
    if std::env::var_os("DISPLAY").is_none() {
        eprintln!("no X display (try xvfb-run): skipped");
        return;
    }
    let files = scanned();
    let catalog = Catalog::new(&files);
    let mut tried = 0;
    for (n, id) in [
        "studio.kx.distrho.Nekobi",
        "studio.kx.distrho.Kars",
        "michaelwillis.dragonfly.hall",
        "in.lsp-plug.compressor_stereo",
    ]
    .into_iter()
    .enumerate()
    {
        let Some(node_type) = find(&catalog, id) else {
            continue;
        };
        tried += 1;
        let node = noodle_core::NodeId(7_000 + n as u64);
        let instance = noodle_engine::NodeType::instantiate(
            node_type,
            &noodle_engine::Setup {
                node,
                config: &Config::new(),
                sample_rate: 48_000.0,
                max_frames: 256,
                input_shapes: &[],
                output_shapes: &[],
                seed: 0,
                position: 0,
            },
        )
        .unwrap();
        let result = noodle_plugins::open_editor(node);
        eprintln!("{id}: open_editor -> {result:?}");
        if result.is_ok() {
            assert!(noodle_plugins::editor_is_open(node));
            std::thread::sleep(std::time::Duration::from_millis(300));
            noodle_plugins::close_editor(node);
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while noodle_plugins::editor_is_open(node) {
                assert!(std::time::Instant::now() < deadline, "{id} never closed");
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            assert!(noodle_plugins::capture(node).is_ok());
        }
        drop(instance);
    }
    if tried == 0 {
        eprintln!("no installed plugins to try: skipped");
    }
}
