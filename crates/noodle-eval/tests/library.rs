//! The system library's files are made from `song::system`, and stay in step
//! with it: `UPDATE_LIBRARY=1 cargo test -p noodle-eval --test library`
//! rewrites them.

use std::path::Path;

use noodle_core::library::{Item, Library, topology};
use noodle_eval::song::system::files;

fn dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../noodle-core/library/system")
}

#[test]
fn the_system_files_are_what_the_code_makes() {
    let update = std::env::var_os("UPDATE_LIBRARY").is_some();
    for file in files() {
        let path = dir().join(format!("{}.ron", file.name));
        let made = file.to_ron();
        if update {
            std::fs::write(&path, &made).unwrap();
        }
        let current = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            current, made,
            "{} is out of date; run with UPDATE_LIBRARY=1",
            file.name
        );
    }
}

/// Every template adds a valid group, and each of its presets fits it.
#[test]
fn every_system_preset_fits_its_template() {
    let library = Library::in_memory();
    let mut registry = noodle_engine::Registry::with_builtins();
    let _ = noodle_nodes::register_library(&mut registry);
    let layout = |node: &noodle_core::Node| registry.layout_signature(node);
    let mut templates = 0;
    for (id, template) in library.templates() {
        templates += 1;
        let mut project = noodle_core::Project::new();
        let (group, command) =
            template.instantiate(id, None, noodle_core::Position::default(), || {
                project.new_node_id()
            });
        noodle_core::History::new()
            .apply(&mut project, command)
            .unwrap();
        let order = topology::topology(project.graph(), group);
        let fitting: Vec<_> = library
            .group_presets()
            .filter(|(_, p)| p.fits(project.graph(), group, &order, &layout))
            .collect();
        assert!(fitting.len() >= 3, "{} has its presets", template.name);
    }
    assert_eq!(
        templates, 11,
        "the instruments, the subtractive voice, two synced effects and the rack"
    );
    assert!(
        library
            .all()
            .any(|e| matches!(e.item, Item::GroupPreset(_)))
    );
}

/// Nodes that have nothing worth a preset (no settings, or only wiring).
const WITHOUT_PRESETS: &[&str] = &[
    "noodle.spectral.analyze",
    "noodle.spectral.synthesize",
    "noodle.event.mono",
    "noodle.event.control",
    "noodle.group.stage",
    "noodle.input.button",
    "noodle.internal.automation",
    "noodle.io.input",
    "noodle.io.output",
    "noodle.mod.automation",
    "noodle.noise.white",
    "noodle.offline.reverse",
    "noodle.poly.voice_mix",
    "noodle.util.script",
    "noodle.track.input",
    "noodle.util.mix",
    "noodle.util.merge_channels",
    "noodle.util.reroute",
    "noodle.util.split_channels",
    "noodle.view.meter",
    "noodle.view.scope",
    "noodle.view.spectrum",
];

/// Every built-in node has presets (bar a few), each fits a node of its type
/// with the preset's config (some, like Note Split's rest output, need it) and sets only parameters that exist, inside their range.
#[test]
fn every_node_has_presets_that_fit() {
    let library = Library::in_memory();
    let mut registry = noodle_engine::Registry::with_builtins();
    let _ = noodle_nodes::register_library(&mut registry);
    let layout = |node: &noodle_core::Node| registry.layout_signature(node);
    let mut covered = std::collections::BTreeMap::<String, usize>::new();
    for (id, preset) in library.node_presets() {
        let mut node = noodle_core::Node::new(&preset.node_type);
        node.config = preset.values.config.clone();
        assert!(preset.applies_to(&node, &layout), "{id} fits its node");
        let ty = registry.get(&preset.node_type).expect("a known node");
        let ports = ty.layout(&node.config).unwrap();
        for (key, value) in &preset.values.params {
            let info = ports
                .inputs
                .iter()
                .find_map(|p| match &p.kind {
                    noodle_engine::InputKind::Param(info) if p.key == key.as_str() => Some(info),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("{id} sets {key}, which {} lacks", preset.node_type));
            assert!(
                (info.min..=info.max).contains(value),
                "{id}: {key} = {value} is outside {}..{}",
                info.min,
                info.max
            );
        }
        *covered.entry(preset.node_type.clone()).or_default() += 1;
    }
    for ty in registry.iter() {
        let id = ty.info().id;
        if WITHOUT_PRESETS.contains(&id) {
            assert!(
                !covered.contains_key(id),
                "{id} is listed as without presets"
            );
        } else {
            assert!(
                covered.get(id).copied().unwrap_or(0) >= 2,
                "{id} needs at least two presets"
            );
        }
    }
}

/// Compressor, Limiter, Gate and Expander are presets of the Dynamics node.
#[test]
fn the_dynamics_modes_are_presets_of_the_dynamics_node() {
    let library = Library::in_memory();
    let names: Vec<&str> = library
        .node_presets()
        .filter(|(_, p)| p.node_type == noodle_nodes::DYNAMICS_ID)
        .map(|(_, p)| p.name.as_str())
        .collect();
    for wanted in ["Compressor", "Limiter", "Gate", "Expander"] {
        assert!(names.contains(&wanted), "{wanted} in {names:?}");
    }
}
