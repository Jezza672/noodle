use std::time::{Duration, Instant};

use super::catalogue::Entry;
use super::source::{Net, fake::Site};
use super::*;
use crate::session::{Nodes, Session};

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
        audio.source.starts_with("sounds/Tone_440-"),
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
