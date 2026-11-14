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
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    pub audio: AudioConfig,
    /// Run each plugin in a process of its own (see
    /// `noodle_plugins::set_sandbox`).
    pub sandbox_plugins: bool,
    /// The device node monitoring plays on; empty is the main output.
    pub monitor_device: String,
    /// Whether the playback cache picks what to cache by itself. On unless
    /// turned off.
    pub auto_cache: bool,
    /// Bars of metronome before a recording starts (0, 1 or 2).
    pub count_in_bars: u8,
    /// Bars of the project played before a punch-in.
    pub pre_roll_bars: u8,
    /// Whether the cycles of a loop recording of MIDI are takes or merge.
    pub midi_record: crate::midi_take::MidiRecord,
    /// How armed Automation nodes write: Read, Touch or Latch.
    pub automation_mode: crate::session::record_automation::AutomationMode,
    /// What the graph shows of the sound as it plays.
    pub display: crate::display::Display,
    /// The step, in ticks, that clips, notes and markers snap to.
    pub snap: i64,
    /// How the MIDI of the next take is snapped.
    pub record_quantise: crate::midi_take::Quantise,
    /// Drum separation models the user has answered for, by id: whether each
    /// is enabled. A model not listed has not been asked about yet (the
    /// first-use popup). About this machine's downloads, so not exported.
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub drum_models: std::collections::BTreeMap<String, bool>,
    /// The keys the user changed, by command (see `app::commands`).
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub keymap: std::collections::BTreeMap<String, String>,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            audio: AudioConfig::default(),
            sandbox_plugins: false,
            monitor_device: String::new(),
            auto_cache: true,
            count_in_bars: 0,
            pre_roll_bars: 0,
            midi_record: Default::default(),
            automation_mode: Default::default(),
            display: Default::default(),
            snap: crate::snap::DEFAULT,
            record_quantise: Default::default(),
            drum_models: Default::default(),
            keymap: Default::default(),
        }
    }
}

impl Prefs {
    /// The settings that mean the same on another machine: everything but
    /// the audio device choices, the monitor device and the plugin sandbox,
    /// which are about this machine (its hardware, and what it trusts). What is exported, and kept in a project.
    pub fn portable(&self) -> Self {
        Self {
            audio: AudioConfig::default(),
            monitor_device: String::new(),
            sandbox_plugins: false,
            drum_models: Default::default(),
            ..self.clone()
        }
    }
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

/// Where the user's library of saved groups and presets lives: one `.ron`
/// file per entry, in the data directory, e.g.
/// `~/.local/share/noodle/library` on Linux. Copy files in or out to share
/// them.
pub fn library_dir() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "Noodle").map(|dirs| dirs.data_dir().join("library"))
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

/// Saves whether plugins run in their own processes, with the other
/// preferences, if [`init`] has been called.
pub fn remember_sandbox(on: bool) {
    CURRENT.with_borrow_mut(|current| {
        let Some((path, prefs)) = current else {
            return;
        };
        prefs.sandbox_plugins = on;
        if let Err(error) = save(path, prefs) {
            eprintln!("noodle: couldn't save preferences: {error}");
        }
    });
}

/// Saves whether the playback cache is on, with the other preferences, if
/// [`init`] has been called.
pub fn remember_auto_cache(on: bool) {
    CURRENT.with_borrow_mut(|current| {
        let Some((path, prefs)) = current else {
            return;
        };
        prefs.auto_cache = on;
        if let Err(error) = save(path, prefs) {
            eprintln!("noodle: couldn't save preferences: {error}");
        }
    });
}

/// Saves the count-in and pre-roll, with the other preferences, if [`init`]
/// has been called.
pub fn remember_recording(count_in_bars: u8, pre_roll_bars: u8) {
    CURRENT.with_borrow_mut(|current| {
        let Some((path, prefs)) = current else {
            return;
        };
        prefs.count_in_bars = count_in_bars;
        prefs.pre_roll_bars = pre_roll_bars;
        if let Err(error) = save(path, prefs) {
            eprintln!("noodle: couldn't save preferences: {error}");
        }
    });
}

/// Saves the Takes or Merge setting for MIDI loop recordings, with the other
/// preferences, if [`init`] has been called.
pub fn remember_midi_record(mode: crate::midi_take::MidiRecord) {
    CURRENT.with_borrow_mut(|current| {
        let Some((path, prefs)) = current else {
            return;
        };
        prefs.midi_record = mode;
        if let Err(error) = save(path, prefs) {
            eprintln!("noodle: couldn't save preferences: {error}");
        }
    });
}

/// Saves the automation mode, with the other preferences, if [`init`] has
/// been called.
pub fn remember_automation_mode(mode: crate::session::record_automation::AutomationMode) {
    CURRENT.with_borrow_mut(|current| {
        let Some((path, prefs)) = current else {
            return;
        };
        prefs.automation_mode = mode;
        if let Err(error) = save(path, prefs) {
            eprintln!("noodle: couldn't save preferences: {error}");
        }
    });
}

/// What the graph shows of the sound, as [`init`] read it or
/// [`remember_display`] last saved it.
pub fn display() -> crate::display::Display {
    CURRENT.with_borrow(|current| {
        current
            .as_ref()
            .map(|(_, prefs)| prefs.display)
            .unwrap_or_default()
    })
}

/// Saves the display switches, with the other preferences, if [`init`] has
/// been called.
pub fn remember_display(display: crate::display::Display) {
    CURRENT.with_borrow_mut(|current| {
        let Some((path, prefs)) = current else {
            return;
        };
        prefs.display = display;
        if let Err(error) = save(path, prefs) {
            eprintln!("noodle: couldn't save preferences: {error}");
        }
    });
}

/// The drum separation models answered for, and whether each is enabled.
pub fn drum_models() -> std::collections::BTreeMap<String, bool> {
    CURRENT.with_borrow(|current| {
        current
            .as_ref()
            .map(|(_, prefs)| prefs.drum_models.clone())
            .unwrap_or_default()
    })
}

/// Saves whether a drum separation model is enabled (which also records that
/// it has been answered for), with the other preferences, if [`init`] has
/// been called.
pub fn remember_drum_model(id: &str, enabled: bool) {
    CURRENT.with_borrow_mut(|current| {
        let Some((path, prefs)) = current else {
            return;
        };
        prefs.drum_models.insert(id.to_owned(), enabled);
        if let Err(error) = save(path, prefs) {
            eprintln!("noodle: couldn't save preferences: {error}");
        }
    });
}

/// The keys the user changed, as [`init`] read them or [`remember_keymap`]
/// last saved them.
pub fn keymap() -> std::collections::BTreeMap<String, String> {
    CURRENT.with_borrow(|current| {
        current
            .as_ref()
            .map(|(_, prefs)| prefs.keymap.clone())
            .unwrap_or_default()
    })
}

/// Saves the keys the user changed, with the other preferences, if [`init`]
/// has been called.
pub fn remember_keymap(keymap: &std::collections::BTreeMap<String, String>) {
    CURRENT.with_borrow_mut(|current| {
        let Some((path, prefs)) = current else {
            return;
        };
        prefs.keymap = keymap.clone();
        if let Err(error) = save(path, prefs) {
            eprintln!("noodle: couldn't save preferences: {error}");
        }
    });
}

/// Saves the snap step, with the other preferences, if [`init`] has been
/// called.
pub fn remember_snap(ticks: i64) {
    CURRENT.with_borrow_mut(|current| {
        let Some((path, prefs)) = current else {
            return;
        };
        prefs.snap = ticks;
        if let Err(error) = save(path, prefs) {
            eprintln!("noodle: couldn't save preferences: {error}");
        }
    });
}

/// Saves how the next MIDI take is snapped, with the other preferences, if
/// [`init`] has been called.
pub fn remember_record_quantise(quantise: crate::midi_take::Quantise) {
    CURRENT.with_borrow_mut(|current| {
        let Some((path, prefs)) = current else {
            return;
        };
        prefs.record_quantise = quantise;
        if let Err(error) = save(path, prefs) {
            eprintln!("noodle: couldn't save preferences: {error}");
        }
    });
}

/// The settings as portable text, for a project to carry: `None` if [`init`]
/// has not been called.
pub fn portable_text() -> Option<String> {
    CURRENT.with_borrow(|current| {
        current
            .as_ref()
            .and_then(|(_, prefs)| to_text(&prefs.portable()).ok())
    })
}

/// Takes imported settings as the user's own: the settings that travel
/// replace this machine's, which keeps its own audio devices. Saved with the
/// other preferences, if [`init`] has been called. Returns what is now in
/// force.
pub fn adopt(imported: &Prefs) -> Prefs {
    CURRENT.with_borrow_mut(|current| {
        let Some((path, prefs)) = current else {
            return imported.clone();
        };
        *prefs = Prefs {
            audio: prefs.audio.clone(),
            monitor_device: prefs.monitor_device.clone(),
            sandbox_plugins: prefs.sandbox_plugins,
            drum_models: prefs.drum_models.clone(),
            ..imported.portable()
        };
        if let Err(error) = save(path, prefs) {
            eprintln!("noodle: couldn't save preferences: {error}");
        }
        prefs.clone()
    })
}

/// Writes the settings that travel to `path`, to take to another machine.
pub fn export_to(path: &Path) -> io::Result<()> {
    let text = portable_text().ok_or_else(|| io::Error::other("no settings to export"))?;
    write_atomically(path, &text)
}

/// Reads settings from a file the user chose: either a settings file, or a
/// project that has settings saved in it.
pub fn import_from(path: &Path) -> Result<Prefs, String> {
    let text = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    // A project has a graph; settings don't.
    #[derive(Deserialize)]
    struct Peek {
        #[allow(dead_code)]
        graph: ron::Value,
    }
    if ron::from_str::<Peek>(&text).is_ok() {
        let project = noodle_core::Project::from_ron(&text)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        let saved = project
            .user_settings()
            .ok_or_else(|| format!("{} has no settings saved in it", path.display()))?;
        return from_text(saved).map_err(|e| format!("{}: {e}", path.display()));
    }
    from_text(&text).map_err(|e| format!("{}: {e}", path.display()))
}

fn to_text(prefs: &Prefs) -> Result<String, ron::Error> {
    ron::ser::to_string_pretty(prefs, ron::ser::PrettyConfig::default()).map(|t| t + "\n")
}

/// Settings from text, such as a file's or a project's.
pub fn from_text(text: &str) -> Result<Prefs, ron::error::SpannedError> {
    ron::from_str(text)
}

/// Where project templates live: one project file each, in the data
/// directory beside the library, e.g. `~/.local/share/noodle/templates`.
pub fn templates_dir() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "Noodle").map(|dirs| dirs.data_dir().join("templates"))
}

/// Saves the device the node monitor plays on, with the other preferences, if
/// [`init`] has been called.
pub fn remember_monitor_device(device: &str) {
    CURRENT.with_borrow_mut(|current| {
        let Some((path, prefs)) = current else {
            return;
        };
        device.clone_into(&mut prefs.monitor_device);
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
    let text = to_text(prefs).map_err(io::Error::other)?;
    write_atomically(path, &text)
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
                channels: Some(6),
                midi_inputs: vec!["Keystation 49".into(), "Pad Controller".into()],
                threads: 2,
            },
            count_in_bars: 0,
            pre_roll_bars: 0,
            sandbox_plugins: false,
            monitor_device: String::new(),
            auto_cache: false,
            midi_record: crate::midi_take::MidiRecord::Takes,
            automation_mode: crate::session::record_automation::AutomationMode::Latch,
            display: Default::default(),
            snap: crate::snap::DEFAULT,
            record_quantise: Default::default(),
            drum_models: Default::default(),
            keymap: Default::default(),
        }
    }

    #[test]
    fn the_keymap_is_saved_and_absent_means_the_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("prefs.ron");
        init(path.clone());
        assert!(keymap().is_empty());
        remember_keymap(&[("file.save".to_owned(), "Cmd+J".to_owned())].into());
        assert_eq!(load(&path).unwrap().keymap["file.save"], "Cmd+J");
    }

    #[test]
    fn the_display_switches_are_saved() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("prefs.ron");
        init(path.clone());
        assert_eq!(display(), crate::display::Display::default());
        let chosen = crate::display::Display {
            audio_wires: false,
            ..Default::default()
        };
        remember_display(chosen);
        assert_eq!(load(&path).unwrap().display, chosen);
        assert_eq!(display(), chosen);
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
            r#"(audio:(host:Some("alsa"),output:Some("alsa:hw:CARD=USB"),input:Device("alsa:hw:CARD=Mic"),sample_rate:Some(48000),buffer_size:Some(256),channels:Some(6),midi_inputs:["Keystation 49","Pad Controller"],threads:2),sandbox_plugins:false,monitor_device:"",auto_cache:false,count_in_bars:0,pre_roll_bars:0,midi_record:Takes,automation_mode:Latch,display:(performance:false,event_wires:true,audio_wires:true,input_meters:true,output_meters:true,clip_flash:true,silent_dim:true,param_motion:true,voice_leds:true,beat_pulse:true,cpu_heat:false),snap:960,record_quantise:Off)"#
        );
        // Files from before the MIDI input was added still load.
        let old = r#"(audio:(host:Some("alsa"),output:None,input:Off,sample_rate:None,buffer_size:Some(256)))"#;
        let prefs: Prefs = ron::from_str(old).unwrap();
        assert!(prefs.audio.midi_inputs.is_empty());
        assert_eq!(prefs.audio.threads, 0);
        assert_eq!(prefs.audio.buffer_size, Some(256));
        assert!(!prefs.sandbox_plugins);
    }

    #[test]
    fn the_automation_mode_is_saved_and_defaults_to_touch() {
        use crate::session::record_automation::AutomationMode;
        assert_eq!(Prefs::default().automation_mode, AutomationMode::Touch);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("prefs.ron");
        init(path.clone());
        remember_automation_mode(AutomationMode::Read);
        assert_eq!(load(&path).unwrap().automation_mode, AutomationMode::Read);
    }

    #[test]
    fn the_playback_cache_is_on_unless_turned_off_and_the_choice_is_saved() {
        assert!(Prefs::default().auto_cache);
        // A file from before the setting has the cache on.
        let old: Prefs = ron::from_str(r#"(sandbox_plugins:true)"#).unwrap();
        assert!(old.auto_cache);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("prefs.ron");
        init(path.clone());
        remember_auto_cache(false);
        assert!(!load(&path).unwrap().auto_cache);
        remember_auto_cache(true);
        assert!(load(&path).unwrap().auto_cache);
    }

    #[test]
    fn the_count_in_and_pre_roll_are_saved() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("prefs.ron");
        init(path.clone());
        remember_recording(2, 3);
        let saved = load(&path).unwrap();
        assert_eq!((saved.count_in_bars, saved.pre_roll_bars), (2, 3));
    }

    #[test]
    fn the_plugin_process_choice_is_saved_beside_the_audio_settings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("prefs.ron");
        init(path.clone());
        remember_audio(&chosen().audio);
        remember_sandbox(true);
        let saved = load(&path).unwrap();
        assert!(saved.sandbox_plugins);
        assert_eq!(saved.audio, chosen().audio);
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
        assert_eq!(
            load(&path).unwrap(),
            Prefs {
                audio: chosen().audio,
                ..Prefs::default()
            }
        );
        let aside = dir.path().join("prefs.ron.bad");
        assert_eq!(fs::read_to_string(aside).unwrap(), "not ron");
    }
}
