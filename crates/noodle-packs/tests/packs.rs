use noodle_packs::*;

const H1: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const H2: &str = "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210";

fn sample(hash: &str, name: &str) -> SampleDef {
    SampleDef {
        hash: hash.into(),
        name: name.into(),
        file: None,
        author: None,
        licence: None,
        tags: vec!["kick".into()],
        credit: None,
    }
}

fn pack() -> PackDef {
    PackDef {
        format: FORMAT_VERSION,
        id: "starter-kit".into(),
        name: "Starter kit".into(),
        version: 1,
        description: "Drums".into(),
        author: "Jo".into(),
        licence: Licence::Default,
        samples: vec![sample(H1, "Kick"), sample(H2, "Snare")],
    }
}

/// Every problem reported for `p`, as kinds.
fn kinds(p: &PackDef) -> Vec<ProblemKind> {
    validate(p).into_iter().map(|p| p.kind).collect()
}

#[test]
fn a_good_pack_is_valid() {
    assert!(validate(&pack()).is_empty());
}

#[test]
fn ron_round_trips_and_is_versioned() {
    let mut p = pack();
    p.samples[0].licence = Some(Licence::Spdx("CC-BY-4.0".into()));
    p.samples[0].credit = Some("Kick by Jo".into());
    p.samples[1].licence = Some(Licence::Custom("Ask first".into()));
    let text = p.to_ron().unwrap();
    assert_eq!(PackDef::from_ron(&text).unwrap(), p);
    let future = text.replacen("format: 1", "format: 2", 1);
    assert!(matches!(
        PackDef::from_ron(&future),
        Err(Error::UnsupportedFormat { found: 2, .. })
    ));
}

#[test]
fn optional_fields_may_be_left_out_of_the_ron() {
    let text = format!(
        r#"(format: 1, id: "p", name: "P", version: 1, author: "A",
            samples: [(hash: "{H1}", name: "Kick")])"#
    );
    let p = PackDef::from_ron(&text).unwrap();
    assert_eq!(p.licence, Licence::Default);
    assert!(validate(&p).is_empty());
}

#[test]
fn manifest_resolves_defaults_and_round_trips() {
    let mut p = pack();
    p.samples[1].author = Some("Sam".into());
    p.samples[1].licence = Some(Licence::Spdx("CC0-1.0".into()));
    let m = p.manifest();
    assert_eq!(m.samples[0].author, "Jo");
    assert_eq!(m.samples[0].licence, Licence::Default);
    assert_eq!(m.samples[1].author, "Sam");
    assert_eq!(m.samples[1].licence, Licence::Spdx("CC0-1.0".into()));
    assert_eq!(Manifest::from_json(&m.to_json().unwrap()).unwrap(), m);
    let future = m
        .to_json()
        .unwrap()
        .replacen("\"format\": 1", "\"format\": 9", 1);
    assert!(matches!(
        Manifest::from_json(&future),
        Err(Error::UnsupportedFormat { found: 9, .. })
    ));
}

#[test]
fn index_round_trips() {
    let m = pack().manifest();
    let i = Index::new(vec![m.index_entry("starter-kit/manifest.json")]);
    assert_eq!(i.packs[0].samples, 2);
    assert_eq!(Index::from_json(&i.to_json().unwrap()).unwrap(), i);
    let future = i
        .to_json()
        .unwrap()
        .replacen("\"format\": 1", "\"format\": 3", 1);
    assert!(Index::from_json(&future).is_err());
}

#[test]
fn rule_unsupported_format() {
    let mut p = pack();
    p.format = 7;
    assert_eq!(kinds(&p), [ProblemKind::UnsupportedFormat { found: 7 }]);
}

#[test]
fn rule_pack_id_name_and_samples() {
    let mut p = pack();
    p.id = "Bad Id".into();
    p.name = "  ".into();
    p.samples.clear();
    let k = kinds(&p);
    assert!(k.contains(&ProblemKind::BadPackId));
    assert!(k.contains(&ProblemKind::EmptyPackName));
    assert!(k.contains(&ProblemKind::NoSamples));
    for bad in ["", "-a", "a-", "a--b", "A", "a_b"] {
        assert!(!is_pack_id(bad), "{bad:?}");
    }
}

#[test]
fn rule_hash_is_64_lowercase_hex() {
    for bad in [
        "",
        "abc",
        &H1.to_uppercase(),
        &format!("{}g", &H1[1..]),
        &format!("{H1}0"),
    ] {
        let mut p = pack();
        p.samples[0].hash = bad.into();
        let v = validate(&p);
        assert_eq!(
            v,
            [Problem {
                sample: Some(0),
                kind: ProblemKind::BadHash
            }],
            "{bad:?}"
        );
    }
}

#[test]
fn rule_hashes_are_unique() {
    let mut p = pack();
    p.samples[1].hash = H1.into();
    assert_eq!(
        validate(&p),
        [Problem {
            sample: Some(1),
            kind: ProblemKind::DuplicateHash { first: 0 }
        }]
    );
}

#[test]
fn rule_names_are_unique_and_not_empty() {
    let mut p = pack();
    p.samples[1].name = "Kick".into();
    assert_eq!(
        validate(&p),
        [Problem {
            sample: Some(1),
            kind: ProblemKind::DuplicateName { first: 0 }
        }]
    );
    p.samples[1].name = " ".into();
    assert_eq!(kinds(&p), [ProblemKind::EmptyName]);
}

#[test]
fn rule_tags_are_not_empty() {
    let mut p = pack();
    p.samples[0].tags.push("".into());
    assert_eq!(kinds(&p), [ProblemKind::EmptyTag]);
}

#[test]
fn rule_every_sample_has_an_author() {
    let mut p = pack();
    p.author = String::new();
    assert_eq!(
        kinds(&p),
        [ProblemKind::MissingAuthor, ProblemKind::MissingAuthor]
    );
    p.samples[0].author = Some("Al".into());
    p.samples[1].author = Some("Bo".into());
    assert!(validate(&p).is_empty());
}

#[test]
fn rule_licences_that_need_a_credit_have_one() {
    for id in [
        "CC-BY-4.0",
        "CC-BY-SA-4.0",
        "CC-BY-NC-4.0",
        "CC-BY-3.0",
        "MIT",
    ] {
        let mut p = pack();
        p.licence = Licence::Spdx(id.into());
        assert_eq!(
            kinds(&p),
            [ProblemKind::MissingCredit, ProblemKind::MissingCredit],
            "{id}"
        );
        for s in &mut p.samples {
            s.credit = Some("By Jo".into());
        }
        assert!(validate(&p).is_empty(), "{id}");
    }
    // A blank credit is no credit; CC0 and the default need none.
    let mut p = pack();
    p.samples[0].licence = Some(Licence::Spdx("CC-BY-4.0".into()));
    p.samples[0].credit = Some("  ".into());
    assert_eq!(kinds(&p), [ProblemKind::MissingCredit]);
    p.samples[0].licence = Some(Licence::Spdx("CC0-1.0".into()));
    assert!(validate(&p).is_empty());
}

#[test]
fn rule_licence_ids_and_text() {
    let mut p = pack();
    p.licence = Licence::Spdx("GPL-9".into());
    assert_eq!(kinds(&p), [ProblemKind::UnknownSpdx { id: "GPL-9".into() }]);
    p.licence = Licence::Custom("".into());
    assert_eq!(kinds(&p), [ProblemKind::EmptyCustomLicence]);
    p.licence = Licence::Default;
    p.samples[0].licence = Some(Licence::Custom(" ".into()));
    assert_eq!(
        validate(&p),
        [Problem {
            sample: Some(0),
            kind: ProblemKind::EmptyCustomLicence
        }]
    );
}

#[test]
fn licence_facts() {
    assert!(!Licence::Default.allows_redistribution());
    assert!(!Licence::Custom("x".into()).allows_redistribution());
    assert!(Licence::Spdx("CC0-1.0".into()).allows_redistribution());
    assert!(!Licence::Spdx("CC0-1.0".into()).requires_credit());
    assert!(!Licence::Default.requires_credit());
}

#[test]
fn rule_files_exist_when_given_a_directory() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("kick.flac"), b"x").unwrap();
    let mut p = pack();
    p.samples[0].file = Some("kick.flac".into());
    p.samples[1].file = Some("sub/snare.flac".into());
    assert!(
        validate(&p).is_empty(),
        "without a directory files are not checked"
    );
    assert_eq!(
        validate_dir(&p, dir.path()),
        [Problem {
            sample: Some(1),
            kind: ProblemKind::MissingFile {
                file: "sub/snare.flac".into()
            }
        }]
    );
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    std::fs::write(dir.path().join("sub/snare.flac"), b"x").unwrap();
    assert!(validate_dir(&p, dir.path()).is_empty());
}

#[test]
fn rule_files_stay_inside_the_pack() {
    let dir = tempfile::tempdir().unwrap();
    for bad in ["../kick.flac", "/etc/passwd", "", "a/../b.flac"] {
        let mut p = pack();
        p.samples[0].file = Some(bad.into());
        let expected = [Problem {
            sample: Some(0),
            kind: ProblemKind::UnsafePath { file: bad.into() },
        }];
        assert_eq!(validate(&p), expected, "{bad:?}");
        assert_eq!(validate_dir(&p, dir.path()), expected, "{bad:?}");
    }
}

#[test]
fn all_problems_are_reported_and_printable() {
    let mut p = pack();
    p.samples[0].hash = "x".into();
    p.samples[1].name = "".into();
    let v = validate(&p);
    assert_eq!(v.len(), 2);
    assert_eq!(
        v[0].to_string(),
        "sample 0: hash must be 64 lowercase hex digits"
    );
}
