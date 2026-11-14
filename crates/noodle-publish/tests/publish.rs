use std::fs;
use std::path::Path;

use noodle_io::{Audio, decode_file, write_wav};
use noodle_nodes::Sample;
use noodle_packs::{Index, Manifest};
use noodle_publish::*;

/// A 24-bit-exact test tone, as `channels` interleaved channels.
fn tone(channels: usize, frames: usize, pitch: f32) -> Audio {
    let mut samples = Vec::new();
    for i in 0..frames {
        for c in 0..channels {
            let x = ((i as f32 * pitch + c as f32).sin() * 0.5 * 8_388_608.0).round();
            samples.push(x / 8_388_608.0);
        }
    }
    Audio {
        samples,
        channels,
        sample_rate: 48_000,
    }
}

fn hash_of(audio: &Audio) -> String {
    Sample::from_audio(audio).hash()
}

/// Writes `<root>/<id>/pack.ron` and the audio files it names.
fn make_pack(root: &Path, id: &str, files: &[(&str, &Audio, Option<&str>)]) {
    let dir = root.join(id);
    fs::create_dir_all(&dir).unwrap();
    let mut samples = String::new();
    for (name, audio, hash) in files {
        let file = format!("{name}.wav");
        write_wav(
            &dir.join(&file),
            &audio.samples,
            audio.channels,
            audio.sample_rate,
        )
        .unwrap();
        let hash = hash.map(str::to_owned).unwrap_or_else(|| hash_of(audio));
        samples += &format!(
            "(hash: \"{hash}\", name: \"{name}\", file: Some(\"{file}\"), tags: [\"drum\"]),\n"
        );
    }
    fs::write(
        dir.join("pack.ron"),
        format!(
            "(format: 1, id: \"{id}\", name: \"{id}\", version: 1, author: \"Jo\", samples: [{samples}])"
        ),
    )
    .unwrap();
}

fn fixture() -> (tempfile::TempDir, Audio, Audio) {
    let root = tempfile::tempdir().unwrap();
    let (kick, pad) = (tone(1, 3000, 0.05), tone(2, 9000, 0.02));
    make_pack(
        root.path(),
        "starter",
        &[("kick", &kick, None), ("pad", &pad, None)],
    );
    (root, kick, pad)
}

#[test]
fn builds_objects_named_by_the_hash_of_their_decoded_audio() {
    let (root, kick, pad) = fixture();
    let out = tempfile::tempdir().unwrap();
    let report = build(root.path(), out.path()).unwrap();
    assert_eq!(
        report,
        Report {
            packs: 1,
            samples: 2,
            written: 2,
            skipped: 0
        }
    );
    for audio in [&kick, &pad] {
        let hash = hash_of(audio);
        let object = out.path().join(object_path(&hash));
        // The object is the audio, exactly, in a different container.
        let decoded = decode_file(&object).unwrap();
        assert_eq!(decoded.samples, audio.samples);
        assert_eq!(hash_of(&decoded), hash);
    }
    let index = Index::from_json(&fs::read_to_string(out.path().join(INDEX)).unwrap()).unwrap();
    assert_eq!(index.packs.len(), 1);
    assert_eq!(index.packs[0].id, "starter");
    assert_eq!(index.packs[0].samples, 2);
    let manifest = Manifest::from_json(
        &fs::read_to_string(out.path().join(&index.packs[0].manifest)).unwrap(),
    )
    .unwrap();
    assert_eq!(manifest.samples[0].author, "Jo");
    let size = fs::metadata(out.path().join(object_path(&manifest.samples[0].hash)))
        .unwrap()
        .len();
    assert_eq!(manifest.samples[0].bytes, Some(size));
    assert_eq!(verify(out.path()).unwrap(), 2);
}

#[test]
fn building_again_leaves_existing_objects_alone() {
    let (root, kick, _) = fixture();
    let out = tempfile::tempdir().unwrap();
    build(root.path(), out.path()).unwrap();
    let object = out.path().join(object_path(&hash_of(&kick)));
    let before = fs::metadata(&object).unwrap().modified().unwrap();

    // A new pack that reuses the kick: only the new sample is written.
    let snare = tone(1, 2000, 0.11);
    make_pack(
        root.path(),
        "extra",
        &[("kick", &kick, None), ("snare", &snare, None)],
    );
    let report = build(root.path(), out.path()).unwrap();
    assert_eq!((report.packs, report.written, report.skipped), (2, 1, 3));
    assert_eq!(fs::metadata(&object).unwrap().modified().unwrap(), before);
    let index = Index::from_json(&fs::read_to_string(out.path().join(INDEX)).unwrap()).unwrap();
    let ids: Vec<_> = index.packs.iter().map(|p| p.id.as_str()).collect();
    assert_eq!(ids, ["extra", "starter"]);
    // Nothing is left half-written.
    let partials = walk(out.path())
        .into_iter()
        .filter(|p| p.ends_with(".partial"));
    assert_eq!(partials.count(), 0);
}

fn walk(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    for e in fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            out.extend(walk(&p));
        } else {
            out.push(p.to_string_lossy().into_owned());
        }
    }
    out
}

#[test]
fn a_hash_that_does_not_match_the_audio_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let kick = tone(1, 3000, 0.05);
    let wrong = "0".repeat(64);
    make_pack(root.path(), "starter", &[("kick", &kick, Some(&wrong))]);
    let out = tempfile::tempdir().unwrap();
    let err = build(root.path(), out.path()).unwrap_err();
    match err {
        PublishError::HashMismatch {
            expected, actual, ..
        } => {
            assert_eq!(expected, wrong);
            assert_eq!(actual, hash_of(&kick));
        }
        other => panic!("{other}"),
    }
    assert!(
        !out.path().join(INDEX).exists(),
        "no index for a failed build"
    );
    assert!(matches!(
        check(root.path()),
        Err(PublishError::HashMismatch { .. })
    ));
}

#[test]
fn audio_a_flac_cannot_hold_exactly_is_refused() {
    let root = tempfile::tempdir().unwrap();
    // 0.1 is not a multiple of 2^-23.
    let audio = Audio {
        samples: vec![0.1; 100],
        channels: 1,
        sample_rate: 48_000,
    };
    make_pack(root.path(), "starter", &[("tick", &audio, None)]);
    let out = tempfile::tempdir().unwrap();
    assert!(matches!(
        build(root.path(), out.path()),
        Err(PublishError::NotLossless { .. })
    ));
    assert!(!out.path().join("objects").read_dir().unwrap().any(|_| true));
}

#[test]
fn a_sample_with_no_file_or_a_missing_file_is_refused() {
    let (root, _, _) = fixture();
    fs::remove_file(root.path().join("starter/pad.wav")).unwrap();
    let out = tempfile::tempdir().unwrap();
    assert!(matches!(
        build(root.path(), out.path()),
        Err(PublishError::Invalid { .. })
    ));
}

#[test]
fn two_packs_with_one_id_are_refused() {
    let (root, kick, _) = fixture();
    make_pack(root.path(), "copy", &[("kick", &kick, None)]);
    let text = fs::read_to_string(root.path().join("copy/pack.ron")).unwrap();
    fs::write(
        root.path().join("copy/pack.ron"),
        text.replace("id: \"copy\"", "id: \"starter\""),
    )
    .unwrap();
    assert!(matches!(
        find_packs(root.path()),
        Err(PublishError::DuplicateId { .. })
    ));
}

#[test]
fn an_empty_directory_is_not_a_publish() {
    let root = tempfile::tempdir().unwrap();
    assert!(matches!(
        find_packs(root.path()),
        Err(PublishError::NoPacks { .. })
    ));
}

#[test]
fn verify_catches_a_corrupted_object() {
    let (root, kick, _) = fixture();
    let out = tempfile::tempdir().unwrap();
    build(root.path(), out.path()).unwrap();
    verify(out.path()).unwrap();

    let object = out.path().join(object_path(&hash_of(&kick)));
    let mut bytes = fs::read(&object).unwrap();
    // Flip bits in the audio frames, past the header.
    let at = bytes.len() * 3 / 4;
    bytes[at] ^= 0x55;
    fs::write(&object, bytes).unwrap();
    assert!(verify(out.path()).is_err());

    // A missing object is caught as well.
    fs::remove_file(&object).unwrap();
    assert!(verify(out.path()).is_err());
}

fn built() -> (tempfile::TempDir, tempfile::TempDir, Audio) {
    let (root, kick, _) = fixture();
    let site = tempfile::tempdir().unwrap();
    build(root.path(), site.path()).unwrap();
    (root, site, kick)
}

#[test]
fn a_contributor_uploads_objects_and_ci_publishes_from_definitions_alone() {
    let (root, site, kick) = built();
    let store = tempfile::tempdir().unwrap();
    let done = upload_objects(site.path(), &DirStore(store.path().to_owned())).unwrap();
    assert_eq!(
        done,
        Uploaded {
            objects_stored: 2,
            objects_present: 0
        }
    );
    // Only objects so far: no manifest, no index.
    assert!(!store.path().join(INDEX).exists());
    assert!(!store.path().join("packs").exists());
    let again = upload_objects(site.path(), &DirStore(store.path().to_owned())).unwrap();
    assert_eq!((again.objects_stored, again.objects_present), (0, 2));

    // CI has the definitions and not the audio.
    for f in ["kick.wav", "pad.wav"] {
        fs::remove_file(root.path().join("starter").join(f)).unwrap();
    }
    let published = publish(root.path(), &DirStore(store.path().to_owned())).unwrap();
    assert_eq!(
        published,
        Published {
            packs: 1,
            samples: 2
        }
    );
    // The store is now a whole site, and it verifies.
    assert_eq!(verify(store.path()).unwrap(), 2);
    let index = Index::from_json(&fs::read_to_string(store.path().join(INDEX)).unwrap()).unwrap();
    let manifest = Manifest::from_json(
        &fs::read_to_string(store.path().join(&index.packs[0].manifest)).unwrap(),
    )
    .unwrap();
    let kick_hash = hash_of(&kick);
    let sample = manifest
        .samples
        .iter()
        .find(|s| s.hash == kick_hash)
        .unwrap();
    let stored = fs::metadata(store.path().join(object_path(&kick_hash)))
        .unwrap()
        .len();
    assert_eq!(sample.bytes, Some(stored));
}

#[test]
fn publish_refuses_a_definition_whose_object_was_never_uploaded() {
    let (root, _site, _) = built();
    let store = tempfile::tempdir().unwrap();
    assert!(matches!(
        publish(root.path(), &DirStore(store.path().to_owned())),
        Err(PublishError::MissingObject { .. })
    ));
    assert!(
        !store.path().join(INDEX).exists(),
        "no index for a failed publish"
    );
}

#[test]
fn publish_refuses_an_object_that_does_not_hash_to_its_name() {
    let (root, site, kick) = built();
    let store = tempfile::tempdir().unwrap();
    upload_objects(site.path(), &DirStore(store.path().to_owned())).unwrap();
    // The kick's name now holds the pad's audio.
    let pad = fs::read_dir(store.path().join("objects"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| !p.to_string_lossy().contains(&hash_of(&kick)))
        .unwrap();
    fs::copy(pad, store.path().join(object_path(&hash_of(&kick)))).unwrap();
    assert!(matches!(
        publish(root.path(), &DirStore(store.path().to_owned())),
        Err(PublishError::HashMismatch { .. })
    ));
    assert!(!store.path().join(INDEX).exists());
}

#[test]
fn upload_never_replaces_an_object() {
    let (_root, site, kick) = built();
    let store = tempfile::tempdir().unwrap();
    upload_objects(site.path(), &DirStore(store.path().to_owned())).unwrap();
    let served = store.path().join(object_path(&hash_of(&kick)));
    let mut older = fs::read(&served).unwrap();
    older.extend_from_slice(&[0; 7]);
    fs::write(&served, &older).unwrap();
    upload_objects(site.path(), &DirStore(store.path().to_owned())).unwrap();
    assert_eq!(fs::read(&served).unwrap(), older, "the object was replaced");
}

#[test]
fn definitions_can_be_checked_without_the_audio() {
    let (root, _site, _) = built();
    fs::remove_file(root.path().join("starter/kick.wav")).unwrap();
    check_defs(root.path()).unwrap();
    assert!(check(root.path()).is_err());
}
