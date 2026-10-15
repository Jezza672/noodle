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
        let key = topology::topology(project.graph(), group, &layout).key;
        let fitting: Vec<_> = library
            .group_presets()
            .filter(|(_, p)| p.topology == key)
            .collect();
        assert!(fitting.len() >= 3, "{} has its presets", template.name);
    }
    assert_eq!(templates, 8, "the instruments and the subtractive voice");
    assert!(
        library
            .all()
            .any(|e| matches!(e.item, Item::GroupPreset(_)))
    );
}
