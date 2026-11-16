# Noodle as a plugin (#544)

The design and the later chunks are in #544 and `docs/ROADMAP.md`; how it works is "Noodle as a plugin" in [../architecture/plugins.md](../architecture/plugins.md).

**Chunk 1, shell split and host backend.** `noodle-app` is a library with the desktop shell in `main.rs`. `noodle-io` has the host backend. The Input and Output nodes take the device `host`, and MIDI In and Out the port `host`. Tests: odd block sizes (1, 37, 128, 4096) against an offline render; `realtime.rs` covers the host path. Checked that the tests fail: with the chunking removed, five of the seven fail; with seek detection removed, the playhead test fails.

**Chunk 2, the headless CLAP.** `noodle-plugin`: Noodle (effect) and Noodle Instrument, stereo audio, notes in and out, state, latency, the two default projects. Tested by `noodle-plugins`' host loading the built library. Allocation-free process is covered where the code is, in `realtime.rs`; the CLAP glue only copies into scratch made at activation.

**Chunk 3, transport, tempo and parameters.** The host's play, stop, locate and tempo drive the engine (`Processor::set_host_tempo`; `TempoSource::Host`). The bank of 64 parameters and the Host Parameter node; `Session::expose_to_host` (no button yet, since the plugin has no window).

**Decisions made on the open questions:** one binary with two plugin entries (effect and instrument); 64 parameters; an instance holds a full project (the group-as-instance idea is not built).

**Chunk 4, the window.** `noodle-plugin/src/gui.rs`: the CLAP `gui` extension, embedded, with `egui-baseview` (wgpu) running the app in a child window of the host's. `App::new_plugin` is the editor-only layout. The app is shared with the host's main thread through a mutex (so `App` is `Send` now: `Rc` became `Arc` in the editor's and the timeline's caches, and two trait objects gained `Send`). Right-click a parameter in the properties panel: **Expose to host**. Tested on Linux in a host window of the test's own (X11, `xvfb-run`, lavapipe): it embeds, draws, resizes, hides, goes, and comes back. Checked that the tests fail: with `destroy` doing nothing the three tests that close an editor time out, and with the minimum size removed the resize test fails.

**Chunk 5, more than one instance.** The UI waker was one global; it is a list now (`wake::register`), and an editor registers its own. The window test opens an effect and an instrument at once and closes one without disturbing the other. Checked that it fails: with the registration never removed the registry test fails.

**Chunk 6, VST3 and AU.** `packaging/` (CMake, `build.sh`, `entry.cpp`) and `.github/workflows/plugin.yml`: `noodle-plugin` as a static library with `--features wrapper`, `clap-wrapper` v0.17.0 in "CLAP first" mode, the VST3 on Linux, macOS and Windows and the AUv2 on macOS (checked with `auval` in CI). The factory gives the wrapper the AU codes. **The CLAP validator** now runs in CI on Linux.

**Import (#569).** Files dragged from the host onto the editor stay unsupported (hosts differ and CI cannot test drops). The supported route is **File > Import…** (also in the command palette): a dialog with a file picker and a paste-a-path field (`noodle-app/src/import_dialog.rs`). Audio files and folders become a Sample Set on the canvas, exactly what dropping them does; MIDI files become clips on new tracks, played by the host's transport. In the desktop app the two File items, Import Audio… and Import MIDI…, are unchanged. Tests: a picked audio file makes a Sample Set, a pasted path works and a missing one is named; shown to fail by skipping the audio import.

**Owed:** the pass in real hosts on the Mac and Windows (Reaper, Logic and Live for AU, Bitwig on Linux): the editor opens, takes keys and the mouse, resizes, survives close and reopen, plays, keeps its state and its parameters. The Mac and Windows windows, and the macOS and Windows package builds, are built by CI but nobody has opened them. See ROADMAP.
