//! The loudness an export reports is the loudness `noodle-eval` measures on
//! the file it wrote, on every example song.

use std::path::Path;

use noodle_core::Project;
use noodle_engine::{Progress, Settings};
use noodle_eval::clip::Clip;
use noodle_eval::level::{integrated_lufs, peak_db};
use noodle_io::{ExportFormat, read_wav};
use noodle_nodes::{ExportRequest, RenderRequest, export};

const RATE: u32 = 48_000;
const SETTINGS: Settings = Settings {
    sample_rate: RATE as f32,
    max_frames: 512,
    channels: 2,
};
/// Long enough to hold several 400 ms loudness blocks.
const FRAMES: usize = 4 * RATE as usize;

#[test]
fn the_exports_loudness_agrees_with_the_evaluators_on_the_example_songs() {
    let examples = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples");
    let dir = tempfile::tempdir().unwrap();
    let mut checked = 0;
    let mut paths: Vec<_> = std::fs::read_dir(&examples)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "ron"))
        .collect();
    paths.sort();
    for path in paths {
        let name = path.file_stem().unwrap().to_string_lossy().into_owned();
        let project = Project::from_ron(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let out = dir.path().join(format!("{name}.wav"));
        let request = ExportRequest {
            render: RenderRequest {
                project,
                base: examples.clone(),
                settings: SETTINGS,
                frames: FRAMES,
                extend_registry: None,
            },
            start: 0,
            end: FRAMES as u64,
            mixdown: Some(out.clone()),
            tracks: Vec::new(),
            format: ExportFormat::WavFloat,
            normalize: None,
            true_peak_ceiling: None,
            metadata: Default::default(),
            cue: false,
            freezer: None,
        };
        let report = export(&request, &Progress::new()).unwrap_or_else(|e| panic!("{name}: {e}"));
        let reported = report.mixdown.unwrap();
        let audio = read_wav(&out).unwrap();
        let clip = Clip::new(&audio.samples, audio.channels, RATE);
        let measured = integrated_lufs(clip);
        if measured.is_finite() {
            assert!(
                (reported.lufs - measured).abs() < 1e-6,
                "{name}: export says {} LUFS, noodle-eval {measured}",
                reported.lufs
            );
            checked += 1;
        } else {
            assert_eq!(reported.lufs, measured, "{name}");
        }
        let sample_peak = peak_db(clip);
        if reported.sample_peak_db.is_finite() {
            assert!(
                (reported.sample_peak_db - sample_peak).abs() < 1e-6,
                "{name}"
            );
            assert!(
                reported.true_peak_db >= reported.sample_peak_db - 1e-9,
                "{name}"
            );
        }
    }
    assert!(
        checked >= 5,
        "only {checked} songs were loud enough to measure"
    );
}
