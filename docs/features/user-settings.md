# User settings

Preferences that belong to the person, not the project, live in one settings file in the platform config directory (`prefs.ron`, `crates/noodle-app/src/prefs.rs`). Beat snap and the MIDI take quantise were the two that were forgotten at exit; they are now saved with the rest (audio devices, plugin sandbox, playback cache, count-in and pre-roll, MIDI record mode, automation mode, Display Settings and the keymap).

- **Portable.** The settings that travel are all of them except the audio device choices and the monitor device, which name this machine's hardware (`Prefs::portable`).
- **Export and import.** File > Export User Settings… writes the portable settings to a file; File > Import User Settings… reads one. The import dialog also accepts a project file and takes the settings saved in it. An import replaces the portable settings and keeps this machine's devices.
- **In the project.** Every save writes the current portable settings into the project as `user_settings` (opaque text, which `noodle-core` stores but does not read; not an undo step, not part of equality, left out of templates).
- **Opening.** A plain Open never touches the user's settings. Open with Settings (File menu, Cmd/Ctrl+Shift+O, or Shift held on Open…) loads the project and then takes its settings as the user's own.

Tests: `prefs.rs`, and `app.rs` (snap remembered, a project carries settings but only Open with Settings takes them, export and import from a file and from a project, devices kept on import).
