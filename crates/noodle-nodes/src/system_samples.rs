//! Audio shipped inside the program, for the system library's sample-based
//! instruments (the Steel Drum, the Banjo, the Body Percussion kit).
//!
//! A system template names its samples by content hash like any other
//! instrument (ADR 0004), and [`SampleStore`](crate::SampleStore) looks here
//! after memory and before the project's folder, so they play with no
//! `samples` folder beside the project. A project that uses one still copies
//! it there when saved, so the project stays portable.
//!
//! The files are in `assets/samples/`, each named in [`SAMPLES`];
//! `assets/samples/index.txt` says which name is which hash, because the hash
//! is of the decoded audio, not the file. Where each recording came from and
//! its licence is in `assets/samples/SOURCES.md`. A test checks the table, the
//! index and the folder agree; `UPDATE_SAMPLE_INDEX=1 cargo test -p
//! noodle-nodes system_samples` rewrites the index.

use std::collections::HashMap;
use std::sync::OnceLock;

use noodle_io::Audio;

macro_rules! samples {
    ($($name:literal),* $(,)?) => {
        /// Every shipped sample: its name and its WAV file.
        pub const SAMPLES: &[(&str, &[u8])] = &[
            $(($name, include_bytes!(concat!("../assets/samples/", $name, ".wav")))),*
        ];
    };
}

samples![
    "steel-drum-60",
    "steel-drum-63",
    "steel-drum-66",
    "steel-drum-69",
    "steel-drum-72",
    "steel-drum-75",
    "steel-drum-78",
    "steel-drum-81",
    "banjo-Ds3",
    "banjo-G3",
    "banjo-As3",
    "banjo-Cs4",
    "banjo-E4",
    "banjo-G4",
    "banjo-As4",
    "banjo-Cs5",
    "body-stomp",
    "body-clap",
    "body-snap",
    "body-finger-snap",
    "body-belly",
    "body-chest-thump",
    "body-chest-slap",
    "body-cheek-pop",
];

const INDEX: &str = include_str!("../assets/samples/index.txt");

fn by_hash() -> &'static HashMap<&'static str, &'static [u8]> {
    static MAP: OnceLock<HashMap<&'static str, &'static [u8]>> = OnceLock::new();
    MAP.get_or_init(|| {
        INDEX
            .lines()
            .filter_map(|line| {
                let (name, hash) = line.split_once(' ')?;
                let bytes = SAMPLES.iter().find(|(n, _)| *n == name)?.1;
                Some((hash.trim(), bytes))
            })
            .collect()
    })
}

/// The hash of the shipped sample called `name`.
///
/// # Panics
/// If there is no such sample.
pub fn hash(name: &str) -> &'static str {
    INDEX
        .lines()
        .find_map(|line| {
            let (n, h) = line.split_once(' ')?;
            (n == name).then(|| h.trim())
        })
        .unwrap_or_else(|| panic!("no shipped sample called {name}"))
}

/// The decoded audio of the shipped sample with this hash, if there is one.
pub fn audio(hash: &str) -> Option<Audio> {
    noodle_io::read_wav_bytes(by_hash().get(hash)?).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Sample;

    fn hash_of(bytes: &[u8]) -> String {
        Sample::from_audio(&noodle_io::read_wav_bytes(bytes).unwrap()).hash()
    }

    #[test]
    fn the_index_matches_the_files() {
        let made: String = SAMPLES
            .iter()
            .map(|(name, bytes)| format!("{name} {}\n", hash_of(bytes)))
            .collect();
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/samples/index.txt");
        if std::env::var_os("UPDATE_SAMPLE_INDEX").is_some() {
            std::fs::write(&path, &made).unwrap();
        }
        assert_eq!(
            INDEX, made,
            "index.txt is out of date: UPDATE_SAMPLE_INDEX=1 cargo test -p noodle-nodes system_samples"
        );
    }

    #[test]
    fn every_file_in_the_folder_is_in_the_table() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/samples");
        for entry in std::fs::read_dir(dir).unwrap() {
            let name = entry.unwrap().file_name().into_string().unwrap();
            if let Some(stem) = name.strip_suffix(".wav") {
                assert!(
                    SAMPLES.iter().any(|(n, _)| *n == stem),
                    "{name} is not in system_samples::SAMPLES"
                );
            }
        }
    }

    #[test]
    fn the_store_finds_a_shipped_sample_with_no_folder() {
        let store = crate::SampleStore::new();
        let sample = store.get(hash("steel-drum-60")).expect("shipped");
        assert!(sample.frames() > 40_000);
        assert!(store.get(&"0".repeat(64)).is_none());
    }
}
