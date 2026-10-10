//! Preferences that outlive a session, such as which audio devices to use.
//! They're a small RON file in the user's config directory, read at startup
//! and written when a setting is applied.
//!
//! [`init`] reads the file and keeps the preferences in memory, per thread:
//! the app's UI runs on the thread that started it, and each test runs on
//! its own, so a test can point the app at a temporary file without
//! touching the real preferences or other tests. Saving writes what's in
//! memory, never re-reading the file.

use std::cell::RefCell;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use noodle_io::AudioConfig;
use serde::{Deserialize, Serialize};

use crate::session::write_atomically;

/// Everything saved. Missing fields take their defaults, so the file stays
/// readable as fields are added.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    pub audio: AudioConfig,
}

thread_local! {
    /// Where [`init`] said the preferences live, and what they are now.
    static CURRENT: RefCell<Option<(PathBuf, Prefs)>> = const { RefCell::new(None) };
}

/// Where the preferences live on this platform, e.g.
/// `~/.config/noodle/prefs.ron` on Linux and
/// `~/Library/Application Support/Noodle/prefs.ron` on macOS.
pub fn default_path() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "Noodle").map(|dirs| dirs.config_dir().join("prefs.ron"))
}

/// Reads the preferences at `path` and keeps them for [`remember_audio`].
/// A missing or unreadable file gives the defaults: losing preferences
/// shouldn't stop the app starting. A damaged file is moved aside to
/// `prefs.ron.bad`, so the next save can't quietly replace what's in it.
pub fn init(path: PathBuf) -> Prefs {
    let prefs = load(&path).unwrap_or_else(|error| {
        eprintln!("noodle: ignoring preferences: {error}");
        if let PrefsError::Parse(..) = error {
            let aside = path.with_extension("ron.bad");
            match fs::rename(&path, &aside) {
                Ok(()) => eprintln!("noodle: moved them to {}", aside.display()),
                Err(error) => eprintln!("noodle: couldn't move them aside: {error}"),
            }
        }
        Prefs::default()
    });
    CURRENT.set(Some((path, prefs.clone())));
    prefs
}

/// Saves the audio settings, with the other preferences, if [`init`] has
/// been called. A failure is reported but not fatal.
pub fn remember_audio(audio: &AudioConfig) {
    CURRENT.with_borrow_mut(|current| {
        let Some((path, prefs)) = current else {
            return;
        };
        prefs.audio = audio.clone();
        if let Err(error) = save(path, prefs) {
            eprintln!("noodle: couldn't save preferences: {error}");
        }
    });
}

#[derive(Debug)]
pub enum PrefsError {
    Io(PathBuf, io::Error),
    Parse(PathBuf, Box<ron::error::SpannedError>),
}

impl fmt::Display for PrefsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(path, error) => write!(f, "{}: {error}", path.display()),
            Self::Parse(path, error) => write!(f, "{}: {error}", path.display()),
        }
    }
}

/// Reads preferences. A file that doesn't exist yet gives the defaults.
pub fn load(path: &Path) -> Result<Prefs, PrefsError> {
    match fs::read_to_string(path) {
        Ok(text) => {
            ron::from_str(&text).map_err(|error| PrefsError::Parse(path.into(), Box::new(error)))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Prefs::default()),
        Err(error) => Err(PrefsError::Io(path.into(), error)),
    }
}

/// Writes preferences, creating the directory if needed. Like a project, it
/// writes and syncs a temporary file and renames it over the old one, so a
/// failed or interrupted save never destroys the last good copy.
pub fn save(path: &Path, prefs: &Prefs) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let text = ron::ser::to_string_pretty(prefs, ron::ser::PrettyConfig::default())
        .map_err(io::Error::other)?;
    write_atomically(path, &(text + "\n"))
}

#[cfg(test)]
mod tests {
    use noodle_io::InputChoice;

    use super::*;

    fn chosen() -> Prefs {
        Prefs {
            audio: AudioConfig {
                host: Some("alsa".into()),
                output: Some("alsa:hw:CARD=USB".into()),
                input: InputChoice::Device("alsa:hw:CARD=Mic".into()),
                sample_rate: Some(48_000),
                buffer_size: Some(256),
                midi_input: Some("Keystation 49".into()),
            },
        }
    }

    #[test]
    fn saved_preferences_load_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("new").join("prefs.ron");
        save(&path, &chosen()).unwrap();
        assert_eq!(load(&path).unwrap(), chosen());
        // Saving again replaces the file.
        save(&path, &Prefs::default()).unwrap();
        assert_eq!(load(&path).unwrap(), Prefs::default());
    }

    #[test]
    fn no_file_means_the_defaults() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            load(&dir.path().join("prefs.ron")).unwrap(),
            Prefs::default()
        );
    }

    #[test]
    fn missing_fields_take_their_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("prefs.ron");
        fs::write(&path, "(audio: (buffer_size: Some(128)))").unwrap();
        let prefs = load(&path).unwrap();
        assert_eq!(prefs.audio.buffer_size, Some(128));
        assert_eq!(prefs.audio.input, InputChoice::Off);
        fs::write(&path, "()").unwrap();
        assert_eq!(load(&path).unwrap(), Prefs::default());
    }

    #[test]
    fn a_broken_file_is_an_error_that_names_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("prefs.ron");
        fs::write(&path, "(audio: (buffer_size: \"lots\"))").unwrap();
        let error = load(&path).unwrap_err();
        assert!(matches!(error, PrefsError::Parse(..)), "{error:?}");
        assert!(error.to_string().contains("prefs.ron"), "{error}");
    }

    /// The on-disk format: changing it would lose users' settings.
    #[test]
    fn the_file_format_is_stable() {
        let text = ron::ser::to_string(&chosen()).unwrap();
        assert_eq!(
            text,
            r#"(audio:(host:Some("alsa"),output:Some("alsa:hw:CARD=USB"),input:Device("alsa:hw:CARD=Mic"),sample_rate:Some(48000),buffer_size:Some(256),midi_input:Some("Keystation 49")))"#
        );
        // Files from before the MIDI input was added still load.
        let old = r#"(audio:(host:Some("alsa"),output:None,input:Off,sample_rate:None,buffer_size:Some(256)))"#;
        let prefs: Prefs = ron::from_str(old).unwrap();
        assert_eq!(prefs.audio.midi_input, None);
        assert_eq!(prefs.audio.buffer_size, Some(256));
    }

    #[test]
    fn init_loads_and_remember_saves_there() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("prefs.ron");
        // Nothing is saved before init.
        remember_audio(&chosen().audio);
        assert!(!path.exists());

        save(&path, &chosen()).unwrap();
        assert_eq!(init(path.clone()), chosen());
        let quieter = AudioConfig {
            buffer_size: Some(1024),
            ..chosen().audio
        };
        remember_audio(&quieter);
        assert_eq!(load(&path).unwrap().audio, quieter);
    }

    #[test]
    fn a_broken_file_starts_with_the_defaults_and_is_kept_aside() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("prefs.ron");
        fs::write(&path, "not ron").unwrap();
        assert_eq!(init(path.clone()), Prefs::default());
        remember_audio(&chosen().audio);
        assert_eq!(load(&path).unwrap(), chosen());
        let aside = dir.path().join("prefs.ron.bad");
        assert_eq!(fs::read_to_string(aside).unwrap(), "not ron");
    }
}
