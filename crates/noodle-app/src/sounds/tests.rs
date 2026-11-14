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
        if sounds.refreshing.is_none() && sounds.fetching.is_empty() {
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
fn a_sound_dropped_on_the_timeline_is_copied_in_with_its_credit() {
    let dir = tempfile::tempdir().unwrap();
    let (site, _) = site(dir.path(), &[440.0]);
    let mut s = sounds(&site, dir.path());
    let mut session = session();
    std::fs::create_dir_all(dir.path().join("song")).unwrap();
    assert!(session.save_as(&dir.path().join("song/project.ron")));
    s.refresh();
    settle(&mut s, &session);
    let e = entry(&s, "Tone 440").clone();
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
    assert!(
        // Windows writes the separator its own way.
        audio
            .source
            .replace('\\', "/")
            .starts_with("sounds/Tone_440-"),
        "{}",
        audio.source
    );
    assert!(dir.path().join("song").join(&audio.source).is_file());
    assert_eq!(audio.credit.as_deref(), Some("Tone 440 by Ada (CC-BY-4.0)"));
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
    assert_eq!(done[0].result.as_ref().unwrap(), &file);
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
