//! Golden renders: every project in `examples/` renders to the audio stored
//! for it in `tests/golden/`, within a small tolerance for floating-point
//! differences between platforms.
//!
//! After a change that's meant to alter how something sounds, regenerate the
//! files with `UPDATE_GOLDEN=1 cargo test -p noodle-nodes --test golden`,
//! listen to them, and commit them with the change.

use std::path::{Path, PathBuf};
use std::{env, fs};

use noodle_core::Project;
use noodle_engine::{Registry, Settings, render};
use noodle_io::{read_wav, write_wav};

const SETTINGS: Settings = Settings {
    sample_rate: 48_000.0,
    max_frames: 512,
    channels: 1,
};
/// A quarter of a second: long enough to listen to, short enough to keep the
/// files small.
const FRAMES: usize = 12_000;
/// Platforms' maths libraries can differ in the last bits of functions like
/// `sin`.
const TOLERANCE: f32 = 1e-4;

#[test]
fn examples_match_their_golden_renders() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let golden = root.join("tests/golden");
    let update = env::var_os("UPDATE_GOLDEN").is_some();
    let mut registry = Registry::with_builtins();
    noodle_nodes::register_all(&mut registry);

    let mut projects: Vec<PathBuf> = fs::read_dir(root.join("../../examples"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|e| e == "ron"))
        .collect();
    projects.sort();
    assert!(!projects.is_empty(), "no example projects found");

    let mut failures = Vec::new();
    for path in &projects {
        let name = path.file_stem().unwrap().to_string_lossy();
        let text = fs::read_to_string(path).unwrap();
        let project = Project::from_ron(&text).unwrap_or_else(|e| panic!("{name}: {e}"));
        let rendered = render(project.graph(), &registry, SETTINGS, FRAMES).unwrap();
        assert!(
            rendered.diagnostics.is_empty(),
            "{name}: {:?}",
            rendered.diagnostics
        );
        assert!(
            rendered.samples.iter().any(|&x| x != 0.0),
            "{name} is silent"
        );

        let wav = golden.join(format!("{name}.wav"));
        if update {
            write_wav(&wav, &rendered.samples, SETTINGS.channels, 48_000).unwrap();
            continue;
        }
        let expected = read_wav(&wav)
            .unwrap_or_else(|e| panic!("{name}: no golden render ({e}); run with UPDATE_GOLDEN=1"));
        if expected.samples.len() != rendered.samples.len() {
            failures.push(format!(
                "{name}: {} samples, expected {}",
                rendered.samples.len(),
                expected.samples.len()
            ));
        } else if let Some((i, (got, want))) = rendered
            .samples
            .iter()
            .zip(&expected.samples)
            .enumerate()
            .find(|(_, (got, want))| (*got - *want).abs() > TOLERANCE)
        {
            failures.push(format!("{name}: sample {i} is {got}, expected {want}"));
        }
    }
    assert!(
        failures.is_empty(),
        "renders changed:\n{}",
        failures.join("\n")
    );
}
