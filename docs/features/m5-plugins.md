# M5: Plugins

Delivered. This is the record of what was planned and built; open work is in [ROADMAP.md](../ROADMAP.md), the design in [ARCHITECTURE.md](../ARCHITECTURE.md).

**Status:** batches 1 to 5 are done; the Mac check with a real plugin is owed. Batch 1: CLAP hosting through
clack (`noodle-plugins`): scanning in a child process with a cache, plugin nodes
whose ports are the plugin's audio ports, note ports and parameters, saved
state in the project, registration through the render hook. Batch 2: one shared
plugin thread for every instance (timers, callbacks), floating editor windows
opened from the properties panel, state and parameter values captured back
into the node when the editor closes, and a plugin browser (View > Plugins)
with search, Rescan and failed files. Batch 3: the plugin thread can be the
app's UI thread (macOS, Windows), editors embedded in host-made windows
(`src/window/`), every note port read and written, folders named `.clap`
searched, and tests against real third-party plugins (`real_plugins.rs`,
Debian's DISTRHO, Dragonfly and LSP CLAP plugins on Linux CI). Batch 4: VST3
hosting (`src/vst3/`): scanning `.vst3` bundles in the same child process and
cache, plugin nodes `vst3.<class id>` with parameters, audio buses, note
buses, MIDI controllers through `IMidiMapping`, saved state, a VST3 test
plugin (`examples/test_vst3.rs`) and tests against DISTRHO and Dragonfly's
VST3 builds on Linux CI. Batch 5: VST3 editors (`IPlugView` embedded in the
host-made windows, `src/vst3/gui.rs`; resize, drag, close and capture as for
CLAP), the controller's edits passed to the processor, `IMessage` and
`IConnectionPoint` between component and controller, and a bar position that
follows the denominator. Not yet for VST3: an editor window on Linux (the app
makes none). Owed: a real plugin (Surge XT, Vital) played on the Mac, including
its editor and the new windows, a real VST3 one, and the same on Windows.

- **CLAP hosting through clack:**
  - Scanning in a child process (done).
  - Plugin nodes (done).
  - Parameters as ports (done).
  - Editor windows (done: embedded in host windows on macOS and Windows, floating on Linux).
  - Plugin state saved in the project, captured from the editor (done).
- Plugin browser (done).
- VST3 hosting (done; editors on macOS and Windows).

**Done when:** common free CLAP and VST3 plugins (instruments and effects)
load, play, save and restore on all three platforms.
