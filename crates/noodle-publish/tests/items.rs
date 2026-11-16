//! Library entries (templates and presets) in a pack.

use std::fs;
use std::path::Path;

use noodle_core::library::{Item, NodePreset, PresetValues};
use noodle_core::{Config, Value};
use noodle_io::write_wav;
use noodle_nodes::Sample;
use noodle_packs::{Index, ItemKind, Manifest};
use noodle_publish::*;

/// A node preset whose Sampler uses `sample`.
fn preset(name: &str, sample: &str) -> Item {
    let mut config = Config::new();
    config.set("sample", Value::Text(sample.into()));
    Item::NodePreset(NodePreset {
        name: name.into(),
        slug: "kept-out".into(),
        node_type: "sampler".into(),
        values: PresetValues {
            params: [("gain".to_string(), 0.5)].into(),
            config,
        },
        origin: None,
    })
}

/// A pack `p` with a kick and a preset using it. `needs` is what the item
/// declares.
fn fixture(needs: &[&str]) -> (tempfile::TempDir, String, String) {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("p");
    fs::create_dir_all(&dir).unwrap();
    let samples: Vec<f32> = (0..3000)
        .map(|i| ((i as f32 * 0.05).sin() * 0.5 * 8_388_608.0).round() / 8_388_608.0)
        .collect();
    write_wav(&dir.join("kick.wav"), &samples, 1, 48_000).unwrap();
    let audio = noodle_io::decode_file(&dir.join("kick.wav")).unwrap();
    let kick = Sample::from_audio(&audio).hash();
    let file = item_file(&preset("Boom", &kick));
    let hash = sha256_hex(file.as_bytes());
    fs::write(dir.join("boom.ron"), &file).unwrap();
    let needs = needs
        .iter()
        .map(|n| format!("\"{}\"", if *n == "kick" { kick.as_str() } else { n }))
        .collect::<Vec<_>>()
        .join(",");
    fs::write(
        dir.join("pack.ron"),
        format!(
            "(format: 1, id: \"p\", name: \"P\", version: 1, author: \"Jo\",\n\
             samples: [(hash: \"{kick}\", name: \"Kick\", file: Some(\"kick.wav\"))],\n\
             items: [(hash: \"{hash}\", name: \"Boom\", kind: NodePreset, file: Some(\"boom.ron\"), needs: [{needs}])])"
        ),
    )
    .unwrap();
    (root, kick, hash)
}

fn walk(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_owned()];
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            } else {
                out.push(
                    p.strip_prefix(dir)
                        .unwrap()
                        .to_string_lossy()
                        .replace('\\', "/"),
                );
            }
        }
    }
    out.sort();
    out
}

#[test]
fn an_entry_is_published_as_an_object_named_by_the_hash_of_its_file() {
    let (root, kick, hash) = fixture(&["kick"]);
    let out = tempfile::tempdir().unwrap();
    let report = build(root.path(), out.path()).unwrap();
    assert_eq!((report.samples, report.items, report.written), (1, 1, 2));
    assert!(walk(out.path()).contains(&format!("items/{hash}.ron")));
    let bytes = fs::read(out.path().join(item_object(&hash))).unwrap();
    assert_eq!(sha256_hex(&bytes), hash);
    // The published file has no slug, and loads as the same preset.
    let item = parse_item(&bytes, ItemKind::NodePreset).unwrap();
    assert_eq!(item.slug(), "");
    assert_eq!(item_samples(&item), [kick]);
    let index = Index::from_json(&fs::read_to_string(out.path().join(INDEX)).unwrap()).unwrap();
    assert_eq!(index.packs[0].items, 1);
    let manifest = Manifest::from_json(
        &fs::read_to_string(out.path().join(&index.packs[0].manifest)).unwrap(),
    )
    .unwrap();
    assert_eq!(manifest.items[0].bytes, Some(bytes.len() as u64));
    assert_eq!(verify(out.path()).unwrap(), 2);
    // Again: nothing new.
    let again = build(root.path(), out.path()).unwrap();
    assert_eq!((again.written, again.skipped), (0, 2));
}

#[test]
fn an_entry_that_is_not_what_its_hash_says_or_uses_an_undeclared_sample_is_refused() {
    let (root, _, hash) = fixture(&["kick"]);
    fs::write(root.path().join("p/boom.ron"), "changed").unwrap();
    assert!(matches!(
        check_defs(root.path()),
        Err(PublishError::HashMismatch { expected, .. }) if expected == hash
    ));

    let (root, _, _) = fixture(&[]);
    let err = check_defs(root.path()).unwrap_err();
    assert!(
        matches!(&err, PublishError::BadItem { message, .. } if message.contains("needs")),
        "{err}"
    );

    // Declared but not in the pack: a definition problem.
    let (root, _, _) = fixture(&["kick", &"a".repeat(64)]);
    assert!(matches!(
        check_defs(root.path()),
        Err(PublishError::Invalid { .. })
    ));
}

#[test]
fn a_file_of_the_wrong_kind_or_not_ron_is_refused() {
    assert!(parse_item(b"nonsense", ItemKind::Template).is_err());
    let text = item_file(&preset("x", &"b".repeat(64)));
    assert!(parse_item(text.as_bytes(), ItemKind::Template).is_err());
    assert!(parse_item(text.as_bytes(), ItemKind::NodePreset).is_ok());
}

#[test]
fn ci_publishes_entries_from_git_and_the_samples_come_from_the_store() {
    let (root, _, hash) = fixture(&["kick"]);
    let site = tempfile::tempdir().unwrap();
    build(root.path(), site.path()).unwrap();
    let store = tempfile::tempdir().unwrap();
    let up = upload_objects(site.path(), &DirStore(store.path().to_owned())).unwrap();
    assert_eq!(up.objects_stored, 2);
    // CI has the definitions and the entry files, not the audio.
    fs::remove_file(root.path().join("p/kick.wav")).unwrap();
    fs::remove_file(store.path().join(item_object(&hash))).unwrap();
    let done = publish(root.path(), &DirStore(store.path().to_owned())).unwrap();
    assert_eq!((done.samples, done.items), (1, 1));
    assert_eq!(verify(store.path()).unwrap(), 2);
}
