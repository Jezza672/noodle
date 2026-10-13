//! The drum samples of the songs' drum racks.
//!
//! Each rack slot plays a one-shot made by rendering the synthesised drum it
//! stands for (a kick, a snare, a hat, a clap; see [`super::kit::Drum`]) once,
//! through the engine. The audio is committed in `songs/samples`, one WAV per
//! sample named by its content hash (ADR 0004), with `index.txt` saying which
//! name is which hash, because rendering again on another machine can differ
//! in the last bit and so in the hash. A song's project names the hash, and
//! `noodle render songs/x.ron` finds the file in `songs/samples` beside it.
//!
//! `UPDATE_SONGS=1 cargo test -p noodle-eval --test songs` makes them again.
//! A test checks that what is committed is still what the synthesis makes, to
//! within rounding.

use std::path::{Path, PathBuf};

use noodle_io::Audio;
use noodle_nodes::SampleStore;

use super::kit::{Drum, Instrument};
use super::music::{Note, Pattern};
use super::{Part, Section, Song};

/// The rate the samples are made at.
pub const RATE: u32 = 48_000;

/// The folder the committed samples are in.
pub fn dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("songs/samples")
}

/// The hash of the sample called `name`.
pub fn hash(name: &str) -> String {
    // Read each time, so `write_all` is seen by the same run.
    std::fs::read_to_string(dir().join("index.txt"))
        .unwrap_or_default()
        .lines()
        .find_map(|line| {
            let (n, h) = line.split_once(' ')?;
            (n == name).then(|| h.trim().to_owned())
        })
        .unwrap_or_else(|| {
            panic!("no sample called {name}: UPDATE_SONGS=1 cargo test -p noodle-eval --test songs")
        })
}

/// A store that reads the committed samples.
pub fn store() -> SampleStore {
    let store = SampleStore::new();
    store.set_dir(dir());
    store
}

/// The drum as a mono one-shot: one hit, rendered, cut where it has died away
/// and faded over its last few milliseconds.
pub fn synthesize(drum: Drum) -> Audio {
    let hit: Pattern = Box::new(|_| {
        vec![Note {
            beat: 0.0,
            len: 0.5,
            key: 60,
            vel: 1.0,
        }]
    });
    let song = Song {
        name: "sample",
        bpm: 120.0,
        sections: vec![Section {
            name: "one",
            bars: 1,
        }],
        chords: vec![("one", vec![super::music::chord(60, super::music::MAJ)])],
        parts: vec![Part {
            name: "Drum",
            instrument: Instrument::Drum(drum),
            db: 0.0,
            pan: 0.0,
            plays: vec![("one", hit)],
            fx: Vec::new(),
        }],
        master_db: 0.0,
        master_fx: Vec::new(),
        excerpt: (0, 1),
    };
    let stereo = song
        .render(&crate::tempo::steady(120.0), song.whole(), RATE)
        .expect("a drum renders");
    // A centred pan is 1/sqrt(2) in each ear.
    let mut mono: Vec<f32> = stereo
        .chunks_exact(2)
        .map(|f| f[0] * std::f32::consts::SQRT_2)
        .collect();
    let end = mono
        .iter()
        .rposition(|x| x.abs() > 1e-4)
        .map_or(1, |i| i + 1)
        .min(mono.len());
    mono.truncate(end);
    let fade = (RATE as usize / 200).min(mono.len());
    let len = mono.len();
    for (i, x) in mono[len - fade..].iter_mut().enumerate() {
        *x *= 1.0 - (i + 1) as f32 / fade as f32;
    }
    Audio {
        samples: mono,
        channels: 1,
        sample_rate: RATE,
    }
}

/// Every sample the songs' racks use, by name.
pub fn needed() -> Vec<(String, Drum)> {
    let mut out: Vec<(String, Drum)> = Vec::new();
    for song in super::all() {
        for part in &song.parts {
            if let Instrument::Rack(rack) = &part.instrument {
                for slot in &rack.slots {
                    out.push((slot.sample.clone(), slot.drum));
                }
            }
        }
    }
    out
}

/// Makes the samples and writes them, and `index.txt`, into [`dir`].
pub fn write_all() -> std::io::Result<()> {
    let store = SampleStore::new();
    store.set_dir(dir());
    std::fs::create_dir_all(dir())?;
    // Old files go, so the folder is exactly what the index names.
    for entry in std::fs::read_dir(dir())? {
        let path = entry?.path();
        if path.extension().is_some_and(|e| e == "wav") {
            std::fs::remove_file(path)?;
        }
    }
    let mut index = String::new();
    for (name, drum) in needed() {
        let hash = store.insert(&synthesize(drum));
        store
            .save(&hash)
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        index.push_str(&format!("{name} {hash}\n"));
    }
    std::fs::write(dir().join("index.txt"), index)
}
