use std::time::{Duration, Instant};

use super::catalogue::Entry;
use super::source::{Net, fake::Site};
use super::*;
use crate::session::{Nodes, Session};
use noodle_packs::Licence;

use super::source::fake::{BASE, site, source, tone};

fn sounds(site: &Site, dir: &std::path::Path) -> Sounds {
    Sounds::with(
        Net::new(site.clone()),
        Some(dir.join("data")),
        vec![source()],
    )
}

fn session() -> Session {
    let mut session = Session::new(Nodes::all());
    session.set_audio_config(crate::testkit::virtual_audio());
    session
}

/// Polls until nothing is being fetched, returning what arrived.
fn settle(sounds: &mut Sounds, session: &Session) -> Vec<Done> {
    let mut arrived = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        arrived.extend(sounds.poll(session));
        if sounds.refreshing.is_none() && sounds.fetching.is_empty() && sounds.installing.is_empty()
        {
            return arrived;
        }
        assert!(Instant::now() < deadline, "the jobs never finished");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn entry<'a>(sounds: &'a Sounds, name: &str) -> &'a Entry {
    sounds.entries().iter().find(|e| e.name == name).unwrap()
}

#[test]
fn refreshing_lists_every_sound_of_the_source() {
    let dir = tempfile::tempdir().unwrap();
    let (site, _) = site(dir.path(), &[220.0, 440.0]);
    let mut s = sounds(&site, dir.path());
    s.refresh();
    settle(&mut s, &session());
    assert_eq!(s.entries().len(), 2);
    assert_eq!(s.reach("fake"), Some(&Reach::Reachable));
}

#[test]
fn an_unreachable_source_keeps_what_it_listed_and_is_greyed() {
    let dir = tempfile::tempdir().unwrap();
    let (site, _) = site(dir.path(), &[220.0]);
    let mut s = sounds(&site, dir.path());
    s.refresh();
    settle(&mut s, &session());
    site.offline
        .store(true, std::sync::atomic::Ordering::Relaxed);
    // A new run of the app: only the saved catalogue is there to read.
    let mut again = sounds(&site, dir.path());
    assert_eq!(again.entries().len(), 1, "the last catalogue is listed");
    again.refresh();
    settle(&mut again, &session());
    assert_eq!(again.entries().len(), 1);
    assert!(matches!(again.reach("fake"), Some(Reach::Unreachable(_))));
}

#[test]
fn clicking_a_sound_downloads_it_once_and_plays_it() {
    let dir = tempfile::tempdir().unwrap();
    let (site, hashes) = site(dir.path(), &[220.0]);
    let mut s = sounds(&site, dir.path());
    let session = session();
    s.refresh();
    settle(&mut s, &session);
    let e = entry(&s, "Tone 220").clone();
    assert!(!s.is_cached(&e.key));
    s.request(e.clone(), Then::Audition);
    settle(&mut s, &session);
    assert_eq!(s.playing(), Some("Tone 220"));
    assert!(s.is_cached(&hashes[0]));
    assert_eq!(site.asked_for("objects/"), 1);
    s.request(e, Then::Audition);
    settle(&mut s, &session);
    assert_eq!(site.asked_for("objects/"), 1, "the second play asks no one");
}

#[test]
fn audio_that_is_not_the_one_named_is_refused_and_not_kept() {
    let dir = tempfile::tempdir().unwrap();
    let (site, hashes) = site(dir.path(), &[220.0]);
    // The host serves another sound under this name.
    let (other, _) = tone(dir.path(), 330.0);
    site.put(&format!("{BASE}/objects/{}.flac", hashes[0]), other);
    let mut s = sounds(&site, dir.path());
    let session = session();
    s.refresh();
    settle(&mut s, &session);
    let e = entry(&s, "Tone 220").clone();
    s.request(e, Then::Audition);
    settle(&mut s, &session);
    assert!(s.playing().is_none());
    assert!(!s.is_cached(&hashes[0]));
    assert!(s.message.as_deref().unwrap().contains("Couldn't get"));
}

#[test]
fn a_big_download_asks_first_and_a_small_one_does_not() {
    let dir = tempfile::tempdir().unwrap();
    let (site, _) = site(dir.path(), &[220.0]);
    let mut s = sounds(&site, dir.path());
    s.refresh();
    settle(&mut s, &session());
    let mut big = entry(&s, "Tone 220").clone();
    big.bytes = Some(CONFIRM_BYTES + 1);
    s.request(big, Then::Audition);
    assert!(s.confirm.is_some(), "it waits for the answer");
    assert!(s.fetching.is_empty());
    assert_eq!(site.asked_for("objects/"), 0, "nothing has been fetched");
    let small = entry(&s, "Tone 220").clone();
    s.confirm = None;
    s.request(small, Then::Audition);
    assert!(s.confirm.is_none());
    assert_eq!(s.fetching.len(), 1);
}

#[test]
fn a_sound_dropped_on_a_sampler_becomes_its_sample() {
    let dir = tempfile::tempdir().unwrap();
    let (site, hashes) = site(dir.path(), &[220.0]);
    let mut s = sounds(&site, dir.path());
    let mut session = session();
    s.refresh();
    settle(&mut s, &session);
    let id = session.new_node_id();
    session.edit([crate::session::Edit::Apply(noodle_core::Command::AddNode {
        id,
        node: noodle_core::Node::new(noodle_nodes::SAMPLER_ID),
    })]);
    let e = entry(&s, "Tone 220").clone();
    s.request(e, Then::Sampler(id));
    for done in settle(&mut s, &session) {
        apply::apply(&mut session, done);
    }
    let node = session.project().graph().node(id).unwrap();
    assert_eq!(
        node.config.get(noodle_nodes::SAMPLE_KEY),
        Some(&noodle_core::Value::Text(hashes[0].clone()))
    );
    assert!(session.samples().get(&hashes[0]).is_some());
}

#[test]
fn a_sound_dropped_on_the_timeline_plays_by_hash_with_its_credit_and_is_stored_on_save() {
    let dir = tempfile::tempdir().unwrap();
    let (site, _) = site(dir.path(), &[440.0]);
    let mut s = sounds(&site, dir.path());
    let mut session = session();
    std::fs::create_dir_all(dir.path().join("song")).unwrap();
    assert!(session.save_as(&dir.path().join("song/project.ron")));
    s.refresh();
    settle(&mut s, &session);
    let e = entry(&s, "Tone 440").clone();
    let e_key = e.key.clone();
    s.request(
        e,
        Then::Timeline {
            track: None,
            at: noodle_core::Tick::ZERO,
        },
    );
    for done in settle(&mut s, &session) {
        apply::apply(&mut session, done);
    }
    let clips: Vec<_> = session.project().clips().collect();
    assert_eq!(clips.len(), 1);
    let audio = clips[0].1.as_audio().unwrap();
    // No copy: the clip names the sound by its hash and plays the downloaded file.
    assert_eq!(audio.hash.as_deref(), Some(e_key.as_str()));
    assert!(!dir.path().join("song/sounds").exists());
    assert!(audio.file(&dir.path().join("song")).is_file());
    assert_eq!(audio.credit.as_deref(), Some("Tone 440 by Ada (CC-BY-4.0)"));
    // Saving puts the audio in the project's store.
    assert!(session.save_as(&dir.path().join("song/project.ron")));
    assert!(noodle_core::stored_audio_path(&dir.path().join("song"), &e_key).is_file());
}

#[test]
fn nothing_in_the_panel_reads_a_file_it_was_not_given_a_hash_for() {
    // The carried key is the hash as bytes and back again.
    let hash = "ab".repeat(32);
    let key = SoundKey::from_hash(&hash).unwrap();
    assert_eq!(key.hash(), hash);
    assert!(SoundKey::from_hash("../../x").is_none());
}

/// A static site over real HTTP on this computer, which wants an
/// `Authorization` header. The bodies are those of a fake site, by path.
fn serve(site: &Site, token: &str) -> String {
    use std::io::{BufRead, BufReader, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = format!("http://{}", listener.local_addr().unwrap());
    let files = site.files.clone();
    let want = format!("Bearer {token}");
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap_or_default();
            let path = line.split_whitespace().nth(1).unwrap_or("/").to_owned();
            let mut authorised = false;
            loop {
                let mut header = String::new();
                if reader.read_line(&mut header).unwrap_or(0) <= 2 {
                    break;
                }
                if let Some(value) = header.to_ascii_lowercase().strip_prefix("authorization:") {
                    authorised = value.trim() == want.to_ascii_lowercase();
                }
            }
            let body = files.lock().unwrap().get(&format!("{BASE}{path}")).cloned();
            let mut stream = stream;
            let _ = match (authorised, body) {
                (false, _) => stream.write_all(
                    b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                ),
                (true, None) => stream.write_all(
                    b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                ),
                (true, Some(body)) => stream
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        )
                        .as_bytes(),
                    )
                    .and_then(|()| stream.write_all(&body)),
            };
        }
    });
    address
}

#[test]
fn a_self_hosted_source_is_read_with_its_login_and_the_secret_stays_out_of_the_preferences() {
    let dir = tempfile::tempdir().unwrap();
    let (site, _) = site(dir.path(), &[220.0, 440.0]);
    let address = serve(&site, "s3cret");
    let vault = vault::Memory::default();
    let mut s =
        Sounds::with(Net::http(), Some(dir.path().join("data")), vec![]).with_vault(vault.clone());
    let id = s
        .add_remote("Mine", &address, manage::NewLogin::Bearer("s3cret".into()))
        .unwrap();
    settle(&mut s, &session());
    assert_eq!(s.entries().len(), 2);
    assert_eq!(s.reach(&id), Some(&Reach::Reachable));
    assert_eq!(vault.recall(&id).as_deref(), Some("s3cret"));
    let spec = format!("{:?}", s.source(&id).unwrap().spec());
    assert!(!spec.contains("s3cret"), "{spec}");
    assert!(s.source(&id).unwrap().is_self_hosted());

    // The sound downloads through the same login and is checked.
    let e = entry(&s, "Tone 220").clone();
    s.request(e.clone(), Then::Audition);
    settle(&mut s, &session());
    assert!(s.is_cached(&e.key));
    assert_eq!(s.usage(&id).downloaded, 1);

    // A wrong token is a source that can't be reached, not a crash; what it
    // listed stays.
    let mut wrong = Sounds::with(Net::http(), Some(dir.path().join("data2")), vec![]);
    let id = wrong
        .add_remote("Bad", &address, manage::NewLogin::Bearer("nope".into()))
        .unwrap();
    settle(&mut wrong, &session());
    assert!(matches!(wrong.reach(&id), Some(Reach::Unreachable(_))));
    assert!(wrong.entries().is_empty());
}

#[test]
fn a_bad_address_or_login_is_refused_with_a_reason() {
    let dir = tempfile::tempdir().unwrap();
    let (site, _) = site(dir.path(), &[220.0]);
    let mut s = sounds(&site, dir.path()).with_vault(vault::Memory::default());
    let n = s.sources.len();
    use manage::NewLogin::*;
    for (url, login) in [
        ("not a url", None),
        ("http://example.com", Bearer("t".into())),
        ("https://example.com", Bearer("  ".into())),
        (
            "https://example.com",
            Basic {
                user: String::new(),
                password: "x".into(),
            },
        ),
        (BASE, None),
    ] {
        assert!(s.add_remote("", url, login).is_err(), "{url}");
    }
    assert_eq!(s.sources.len(), n);
}

#[test]
fn removing_a_source_forgets_its_login_and_sounds_but_cleaning_up_keeps_shared_ones() {
    let dir = tempfile::tempdir().unwrap();
    let (site, _) = site(dir.path(), &[220.0, 440.0]);
    let address = serve(&site, "t");
    let vault = vault::Memory::default();
    let mut s = sounds(&site, dir.path()).with_vault(vault.clone());
    let session = session();
    s.refresh();
    settle(&mut s, &session);
    let id = s
        .add_remote("Mine", &address, manage::NewLogin::Bearer("t".into()))
        .unwrap();
    settle(&mut s, &session);
    // Both sources offer the same two sounds. Download one.
    let e = entry(&s, "Tone 220").clone();
    s.request(e.clone(), Then::Audition);
    settle(&mut s, &session);
    assert_eq!(s.cleanable().files, 0);

    s.remove_source(&id);
    assert!(vault.recall(&id).is_none());
    assert!(s.source(&id).is_none());
    assert!(s.entries().iter().all(|x| x.source != id));
    assert_eq!(s.cleanable().files, 0, "the other source still offers it");
    assert!(s.is_cached(&e.key));

    // Remove the one that is left and the download is nobody's.
    s.sources.retain(|x| x.id != "fake");
    s.entries.clear();
    s.scan_cache();
    assert_eq!(s.cleanable().files, 1);
    s.clean_up();
    assert_eq!(s.cleanable().files, 0);
    let cache = s.cache.clone().unwrap();
    assert!(!cache.contains(&e.key));
}

#[test]
fn a_folder_is_a_source_read_in_place_with_an_unknown_licence() {
    let dir = tempfile::tempdir().unwrap();
    let music = dir.path().join("Mine");
    std::fs::create_dir_all(music.join("kicks")).unwrap();
    let (bytes, hash) = tone(dir.path(), 110.0);
    let file = music.join("kicks").join("Boom.flac");
    std::fs::write(&file, bytes).unwrap();
    let (site, _) = site(dir.path(), &[220.0]);
    let mut s = Sounds::with(Net::new(site), Some(dir.path().join("data")), vec![]);
    let id = s.add_folder(music.clone()).unwrap();
    assert!(s.add_folder(music.clone()).is_err(), "twice");
    let session = session();
    settle(&mut s, &session);
    let e = entry(&s, "Boom").clone();
    assert_eq!(e.key, hash);
    assert_eq!(e.licence, Licence::Unknown);
    assert_eq!(e.tags, ["kicks"]);
    assert!(s.is_cached(&e.key), "a folder's sounds are here already");

    // Used where it is: the same path comes back and the cache stays empty.
    s.request(
        e.clone(),
        Then::Canvas {
            at: egui::Pos2::ZERO,
            group: None,
        },
    );
    let done = settle(&mut s, &session);
    // The folder is kept by its canonical path, which differs from the one
    // given on macOS (/var is /private/var) and Windows.
    assert_eq!(
        done[0].result.as_ref().unwrap(),
        &std::fs::canonicalize(&file).unwrap()
    );
    let cache = s.cache.as_ref().unwrap().dir().to_owned();
    assert!(
        std::fs::read_dir(cache).map_or(true, |mut r| r.next().is_none()),
        "nothing was copied"
    );
    assert_eq!(s.usage(&id).bytes, 0);

    // Setting the licence relists it; a changed file is refused.
    s.set_folder_licence(&id, Licence::Spdx("CC0-1.0".into()));
    assert_eq!(entry(&s, "Boom").licence.label(), "CC0-1.0");
    let (other, _) = tone(dir.path(), 330.0);
    std::fs::write(&file, other).unwrap();
    s.request(
        entry(&s, "Boom").clone(),
        Then::Canvas {
            at: egui::Pos2::ZERO,
            group: None,
        },
    );
    let done = settle(&mut s, &session);
    assert!(done[0].result.is_err(), "the file is not what was listed");

    // Rescanning on Refresh finds the change; a folder that goes is unreachable.
    s.refresh();
    settle(&mut s, &session);
    assert_ne!(entry(&s, "Boom").key, hash);
    std::fs::remove_dir_all(&music).unwrap();
    s.refresh();
    settle(&mut s, &session);
    assert!(matches!(s.reach(&id), Some(Reach::Unreachable(_))));
    assert!(!s.is_cached(&entry(&s, "Boom").key));
}

#[test]
fn an_exported_site_is_added_as_a_source_and_read_from_disk() {
    let dir = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let src = dir.path().join("Kit");
    std::fs::create_dir_all(&src).unwrap();
    for (name, hz) in [("Deep", 110.0), ("High", 330.0)] {
        let (bytes, _) = tone(scratch.path(), hz);
        std::fs::write(src.join(format!("{name}.flac")), bytes).unwrap();
    }
    let (found, _) = wizard::scan(&src).unwrap();
    let mut draft = wizard::Draft::new(src, found);
    draft.author = "Jo".into();
    let out = dir.path().join("site");
    wizard::export(&draft, wizard::Format::Folder, &out).unwrap();

    let mut s = Sounds::with(Net::http(), Some(dir.path().join("data")), Vec::new());
    assert!(s.add_site(&dir.path().join("nothing"), "x").is_err());
    let id = s.add_site(&out, "Kit").unwrap();
    settle(&mut s, &session());
    let names: Vec<&str> = s
        .entries()
        .iter()
        .filter(|e| e.source == id)
        .map(|e| e.name.as_str())
        .collect();
    assert_eq!(names.len(), 2, "{names:?}");
    // And a sound is fetched from it like from any source.
    let deep = entry(&s, "Deep").clone();
    s.request(deep, Then::Sampler(noodle_core::NodeId(1)));
    let arrived = settle(&mut s, &session());
    assert!(arrived[0].result.is_ok(), "{:?}", arrived[0].result);
    assert!(s.add_site(&out, "Kit").is_err(), "twice is refused");
}

// Groups and presets travel in packs like sounds do.
mod library_packs {
    use super::*;
    use crate::sounds::share::{self, Draft};
    use crate::sounds::wizard::{Choice, Exported, Format, Kind as LicenceKind};
    use noodle_core::library::{Item, Library, NodePreset, Origin};
    use noodle_core::{Config, Node, Value};
    use noodle_io::Audio;

    /// A session whose store holds a 24-bit-exact tone and whose library holds
    /// a Sampler preset that uses it, saved by the user.
    fn author(preset: &str) -> (Session, String) {
        let session = session();
        let samples = (0..3000)
            .map(|i| ((i as f32 * 0.05).sin() * 0.5 * 8_388_608.0).round() / 8_388_608.0)
            .collect();
        let hash = session.samples().insert(&Audio {
            samples,
            channels: 1,
            sample_rate: 48_000,
        });
        let node = Node::new(noodle_nodes::SAMPLER_ID)
            .with_config(Config::new().with(noodle_nodes::SAMPLE_KEY, Value::Text(hash.clone())));
        let mut library = Library::in_memory();
        library
            .save(Item::NodePreset(NodePreset::capture(&node, preset)))
            .unwrap();
        let mut session = session;
        session.set_library(library);
        (session, hash)
    }

    fn draft(session: &Session) -> Draft {
        let mut d = Draft::default();
        d.set_name("My boom".into());
        d.author = "Ada".into();
        d.picked = share::candidates(session)
            .into_iter()
            .map(|c| c.id)
            .collect();
        d
    }

    fn export(session: &Session, d: &Draft, out: &std::path::Path) -> Exported {
        let from = share::candidates(session);
        share::export(d, &from, session.samples(), Format::Folder, out).unwrap()
    }

    #[test]
    fn a_shared_preset_is_installed_with_the_sound_it_uses() {
        let dir = tempfile::tempdir().unwrap();
        let (mine, hash) = author("Boom");
        let out = dir.path().join("site");
        let done = export(&mine, &draft(&mine), &out);
        assert_eq!(done.samples, 2, "the preset and the sound it uses");

        // Someone else adds the pack as a source and installs from it.
        let source = Source::remote("mine", "Mine", source::file_url(&out), None);
        let mut s = Sounds::with(Net::http(), Some(dir.path().join("data")), vec![source]);
        let mut theirs = session();
        s.refresh();
        settle(&mut s, &theirs);
        let listed = entry(&s, "Boom").clone();
        assert_eq!(listed.kind, catalogue::Kind::NodePreset);
        assert_eq!(listed.needs, std::slice::from_ref(&hash));
        // The Sounds tab lists the sound and not the preset, the Library tab the reverse.
        let sounds_tab = catalogue::list(s.entries(), &s.filter, &s.cached);
        let library_tab = catalogue::list(s.entries(), &s.library_filter, &s.installed);
        assert!(sounds_tab.iter().all(|e| e.kind.is_sound()) && sounds_tab.len() == 1);
        assert_eq!(library_tab.len(), 1);
        assert_eq!(library_tab[0].name, "Boom");

        s.install(listed.clone());
        settle(&mut s, &theirs);
        let arrived = s.take_installs();
        assert_eq!(arrived.len(), 1);
        crate::sounds::apply::install(&mut theirs, arrived.into_iter().next().unwrap());
        let id = theirs
            .library()
            .installed(&listed.key)
            .expect("installed")
            .to_owned();
        let stored = theirs.library().get(&id).unwrap();
        let origin = stored.item.origin().unwrap();
        assert_eq!(
            (origin.pack.as_str(), origin.author.as_str()),
            ("My boom", "Ada")
        );
        // The sound came with it, by the hash the preset names.
        assert!(theirs.samples().get(&hash).is_some());
        assert!(theirs.installed_hashes().contains(&listed.key));

        // Installing again changes nothing.
        s.set_installed(theirs.installed_hashes());
        s.install(listed);
        assert!(s.installing.is_empty());
    }

    #[test]
    fn what_a_source_serves_must_be_what_its_hash_and_manifest_say() {
        let dir = tempfile::tempdir().unwrap();
        let (mine, _) = author("Boom");
        let out = dir.path().join("site");
        export(&mine, &draft(&mine), &out);
        let source = Source::remote("mine", "Mine", source::file_url(&out), None);
        let mut s = Sounds::with(Net::http(), Some(dir.path().join("data")), vec![source]);
        let theirs = session();
        s.refresh();
        settle(&mut s, &theirs);
        let listed = entry(&s, "Boom").clone();

        // Another entry's bytes under this name are not accepted.
        let object = out.join(noodle_packs::item_object(&listed.key));
        std::fs::write(&object, "not what its name says").unwrap();
        s.install(listed.clone());
        settle(&mut s, &theirs);
        let got = s.take_installs().pop().unwrap();
        assert!(got.result.is_err());

        // A manifest that leaves out a sound the entry uses is refused too.
        let mut lies = listed;
        lies.needs.clear();
        let text = noodle_publish::item_file(&Item::NodePreset(NodePreset::capture(
            &Node::new(noodle_nodes::SAMPLER_ID).with_config(
                Config::new().with(noodle_nodes::SAMPLE_KEY, Value::Text("c".repeat(64))),
            ),
            "Boom",
        )));
        std::fs::write(&object, &text).unwrap();
        lies.key = noodle_publish::sha256_hex(text.as_bytes());
        std::fs::copy(&object, out.join(noodle_packs::item_object(&lies.key))).unwrap();
        s.install(lies);
        settle(&mut s, &theirs);
        let err = s.take_installs().pop().unwrap().result.err().unwrap();
        assert!(err.contains("doesn't list"), "{err}");
    }

    #[test]
    fn an_entry_that_came_from_a_restricted_pack_cannot_be_shared_and_a_missing_sound_is_said() {
        let (mut mine, _) = author("Boom");
        // Installed from a pack under the default licence: not passed on.
        let mut library = Library::in_memory();
        let mut item = mine.library().all().last().unwrap().item.clone();
        item.set_origin(Some(Origin {
            hash: "d".repeat(64),
            pack: "Theirs".into(),
            author: "Bo".into(),
            licence: "Default".into(),
            credit: None,
        }));
        library.save(item).unwrap();
        mine.set_library(library);
        let c = share::candidates(&mine);
        assert!(c[0].blocked.as_deref().unwrap().contains("Theirs"));
        let dir = tempfile::tempdir().unwrap();
        let err = share::export(
            &draft(&mine),
            &c,
            mine.samples(),
            Format::Folder,
            &dir.path().join("o"),
        )
        .unwrap_err();
        assert!(err.contains("Theirs"), "{err}");

        // A sound that isn't on this computer is named, not skipped.
        let (mine, _) = author("Boom");
        let empty = noodle_nodes::SampleStore::new();
        let err = share::export(
            &draft(&mine),
            &share::candidates(&mine),
            &empty,
            Format::Folder,
            &dir.path().join("o2"),
        )
        .unwrap_err();
        assert!(err.contains("isn't on this computer"), "{err}");

        // The licence that asks for a credit gets one made for it.
        let (mine, _) = author("Boom");
        let mut d = draft(&mine);
        d.licence = Choice {
            kind: LicenceKind::Spdx,
            spdx: "CC-BY-4.0".into(),
            text: String::new(),
        };
        let out = dir.path().join("o3");
        export(&mine, &d, &out);
        let index = noodle_packs::Index::from_json(
            &std::fs::read_to_string(out.join("index.json")).unwrap(),
        )
        .unwrap();
        let manifest = noodle_packs::Manifest::from_json(
            &std::fs::read_to_string(out.join(&index.packs[0].manifest)).unwrap(),
        )
        .unwrap();
        assert_eq!(
            manifest.items[0].credit.as_deref(),
            Some("Boom by Ada (CC-BY-4.0)")
        );
    }

    #[test]
    fn a_draft_says_what_is_missing() {
        let mut d = Draft::default();
        assert_eq!(d.problems().len(), 4);
        d.set_name("A pack".into());
        d.author = "Me".into();
        d.picked.insert("user:node:x".into());
        assert!(d.problems().is_empty());
        assert_eq!(d.id, "a-pack");
    }
}
