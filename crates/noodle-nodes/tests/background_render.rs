//! The background renderer: streamed output matches the in-memory render, the
//! cache stores exactly what was rendered, and a render that is cancelled or
//! incomplete leaves nothing in the cache.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use noodle_core::{
    CacheKey, Clip, Command, Connection, Endpoint, History, KeyBuilder, Node, NodeId, Project, Tick,
};
use noodle_engine::{Job, OUTPUT_ID, Registry, Settings, StreamError};
use noodle_io::{CacheStore, write_wav};
use noodle_nodes::{
    CHUNK_FRAMES, CacheRender, RenderRequest, TRACK_INPUT_ID, render_project_with_clips,
    spawn_render, spawn_render_to_cache,
};

const RATE: u32 = 48_000;
const SETTINGS: Settings = Settings {
    sample_rate: RATE as f32,
    max_frames: 512,
    channels: 2,
};
/// Several chunks, and not a whole number of them.
const FRAMES: usize = 5 * CHUNK_FRAMES + 123;

fn finish<T>(mut job: Job<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(60);
    while !job.is_finished() {
        assert!(Instant::now() < deadline, "render never finished");
        std::thread::sleep(Duration::from_millis(2));
    }
    job.poll().unwrap().unwrap()
}

/// A track input playing `file` from the start, wired to the output.
fn clip_project(file: &str) -> Project {
    let mut project = Project::new();
    let mut history = History::new();
    let (track, out) = (NodeId(1), NodeId(2));
    for (id, kind) in [(track, TRACK_INPUT_ID), (out, OUTPUT_ID)] {
        let node = Node::new(kind);
        let command = Command::AddNode { id, node };
        history.apply(&mut project, command).unwrap();
    }
    let connection = Connection {
        from: Endpoint::new(track, "audio"),
        to: Endpoint::new(out, "in"),
    };
    let command = Command::Connect(connection);
    history.apply(&mut project, command).unwrap();
    let id = project.new_clip_id();
    let clip = Clip::audio(track, Tick(0), file, FRAMES as u64);
    history
        .apply(&mut project, Command::AddClip { id, clip })
        .unwrap();
    project
}

/// A folder holding `tone.wav`, a stereo sine.
fn folder(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("noodle-background-{name}"));
    std::fs::create_dir_all(&dir).unwrap();
    let samples: Vec<f32> = (0..FRAMES)
        .flat_map(|i| {
            let x = (i as f32 * 0.05).sin() * 0.5;
            [x, -x]
        })
        .collect();
    write_wav(&dir.join("tone.wav"), &samples, 2, RATE).unwrap();
    dir
}

fn request(project: Project, base: &Path) -> RenderRequest {
    RenderRequest {
        project,
        base: base.to_owned(),
        settings: SETTINGS,
        frames: FRAMES,
    }
}

fn key(n: u64) -> CacheKey {
    KeyBuilder::new("background-test").u64(n).finish()
}

fn cache(name: &str) -> (CacheStore, PathBuf) {
    let dir = std::env::temp_dir().join(format!("noodle-background-cache-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    (CacheStore::open(&dir).unwrap(), dir)
}

#[test]
fn streamed_chunks_are_the_same_samples_as_the_in_memory_render() {
    let base = folder("same");
    let project = clip_project("tone.wav");
    let mut registry = Registry::with_builtins();
    let whole = render_project_with_clips(&project, &mut registry, &base, SETTINGS, FRAMES)
        .unwrap()
        .render
        .samples;
    assert!(whole.iter().any(|&s| s != 0.0));

    let (sender, receiver) = std::sync::mpsc::channel();
    let job = spawn_render(request(project, &base), move |chunk| {
        sender.send(chunk.to_vec()).map_err(|_| ())
    });
    let report = finish(job).unwrap();
    assert!(report.is_complete());
    let chunks: Vec<Vec<f32>> = receiver.try_iter().collect();
    assert_eq!(chunks.len(), 6);
    assert!(chunks.iter().all(|c| c.len() <= CHUNK_FRAMES * 2));
    assert_eq!(chunks.concat(), whole);
}

#[test]
fn a_render_into_the_cache_is_stored_and_the_next_one_is_a_hit() {
    let base = folder("store");
    let (store, dir) = cache("store");
    let project = clip_project("tone.wav");
    let mut registry = Registry::with_builtins();
    let whole = render_project_with_clips(&project, &mut registry, &base, SETTINGS, FRAMES)
        .unwrap()
        .render
        .samples;

    let job = spawn_render_to_cache(request(project.clone(), &base), store.clone(), key(1));
    let CacheRender::Stored(info, _) = finish(job).unwrap() else {
        panic!("not stored");
    };
    assert_eq!(
        (info.channels, info.sample_rate, info.frames),
        (2, RATE, FRAMES as u64)
    );
    // Read back in pieces that don't line up with the chunks it was written in.
    let mut audio = store.get(&key(1)).unwrap();
    let mut back = vec![0.0; whole.len()];
    for (i, piece) in back.chunks_mut(2 * 1000).enumerate() {
        audio.read_frames(i as u64 * 1000, piece).unwrap();
    }
    assert_eq!(back, whole);

    // Hits don't render: a project that couldn't render still answers.
    let broken = request(clip_project("missing.wav"), &base);
    let job = spawn_render_to_cache(broken, store.clone(), key(1));
    assert!(matches!(finish(job).unwrap(), CacheRender::Hit(_)));
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
}

#[test]
fn an_incomplete_render_is_not_cached() {
    let base = folder("incomplete");
    let (store, dir) = cache("incomplete");
    let job = spawn_render_to_cache(
        request(clip_project("missing.wav"), &base),
        store.clone(),
        key(1),
    );
    let CacheRender::Incomplete(report) = finish(job).unwrap() else {
        panic!("a render with a missing file was stored");
    };
    assert!(!report.is_complete());
    assert!(!store.contains(&key(1)));
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
}

#[test]
fn a_cancelled_render_leaves_nothing_behind() {
    let base = folder("cancel");
    let (store, dir) = cache("cancel");
    let mut req = request(clip_project("tone.wav"), &base);
    // Long enough that the cancel lands mid-render.
    req.frames = 10_000_000;
    let job = spawn_render_to_cache(req, store.clone(), key(1));
    while job.fraction() == 0.0 {
        std::thread::sleep(Duration::from_millis(1));
    }
    job.cancel();
    assert!(matches!(finish(job), Err(StreamError::Cancelled)));
    assert!(!store.contains(&key(1)));
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
}
