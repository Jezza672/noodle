//! A seek on a running engine must sound like an engine that started at the
//! seek's target (ADR 0010: the playback cache renders a pass from a fresh
//! state at its start, so a pass that began with a seek has to equal it).
//!
//! For every example project: play for a while, seek to S, and compare what
//! comes next with a second engine that was told to seek to S before its
//! first block.

use std::fs;
use std::path::Path;

use noodle_core::{Project, Tick};
use noodle_engine::{Processor, Registry, Settings, TempoTable, engine};
use noodle_nodes::register_library_blocking;

const SETTINGS: Settings = Settings {
    sample_rate: 48_000.0,
    max_frames: 128,
    channels: 1,
};
/// 5000 samples at 120 bpm, in ticks; before the second note of the MIDI clip.
const SEEK_TICK: i64 = 200;
/// Blocks played before the seek.
const BEFORE: usize = 90;
/// Blocks recorded after it.
const AFTER: usize = 500;
/// The blocks the jump's fades take (5 ms out and in): skipped.
const FADES: usize = 5;

fn build(project: &Project, base: &Path) -> (noodle_engine::Controller, Processor) {
    let (mut controller, processor) = engine(SETTINGS).unwrap();
    let mut registry = Registry::with_builtins();
    let library = register_library_blocking(&mut registry);
    library.samples.set_dir(base.join("samples"));
    let table = TempoTable::new(project.tempo_map(), SETTINGS.sample_rate);
    let problems = library
        .clips
        .update(project, &table, SETTINGS.sample_rate as u32, base);
    assert!(problems.is_empty(), "{problems:?}");
    let diagnostics = controller.update_project(project, &registry);
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    // The registry holds the nodes the plan was built from; keep it alive
    // through the instances' lifetime by leaking it for the test.
    Box::leak(Box::new((registry, library)));
    (controller, processor)
}

fn blocks(processor: &mut Processor, count: usize) -> Vec<Vec<f32>> {
    (0..count)
        .map(|_| {
            let mut block = vec![0.0; SETTINGS.max_frames * SETTINGS.channels];
            processor.process(&mut block);
            block
        })
        .collect()
}

#[test]
fn seeking_a_running_engine_sounds_like_starting_there() {
    let examples = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples");
    let mut projects: Vec<_> = fs::read_dir(&examples)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|e| e == "ron"))
        .collect();
    projects.sort();
    let mut failures = Vec::new();
    for path in &projects {
        let name = path.file_stem().unwrap().to_string_lossy().into_owned();
        let project = Project::from_ron(&fs::read_to_string(path).unwrap()).unwrap();

        let (controller, mut running) = build(&project, &examples);
        controller.transport().play();
        blocks(&mut running, BEFORE);
        controller.transport().seek(Tick(SEEK_TICK));
        let after_seek = blocks(&mut running, AFTER);

        let (fresh_controller, mut fresh) = build(&project, &examples);
        fresh_controller.transport().seek(Tick(SEEK_TICK));
        fresh_controller.transport().play();
        let from_start = blocks(&mut fresh, AFTER);

        assert_eq!(
            controller.transport().position(),
            fresh_controller.transport().position(),
            "{name}: the engines ended at different places"
        );
        let wrong = (FADES..AFTER)
            .filter(|&i| after_seek[i] != from_start[i])
            .count();
        // A pass that seeks into the middle of a note does not play it (ADR
        // 0010), and this chord began before the seek.
        let may_be_silent = name == "subtractive-synth";
        assert!(
            may_be_silent || after_seek[FADES..].iter().flatten().any(|&x| x != 0.0),
            "{name}: silent after the seek"
        );
        if wrong > 0 {
            failures.push(format!("{name}: {wrong} blocks differ"));
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}
