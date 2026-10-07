# Noodle roadmap

Each milestone ends with something you can run and check. See
[ARCHITECTURE.md](ARCHITECTURE.md) for the design these milestones build.

## M0: Real-time core, headless

The graph engine works end to end without a UI.

**Status:** done. `noodle play` played through a Mac's speakers and swapped
in an edit; structural edits fade rather than click (#8); the tests pass on
Linux and Windows CI and on macOS. Still to do when the user is back: a
listening check on their Mac, and re-running the macOS tests on the latest
main.

- **CI:** GitHub Actions running fmt, clippy and tests on macOS, Windows and Linux.
- **`noodle-core`:**
  - Graph model: node IDs, ports, connections, parameter values.
  - Commands with undo/redo.
  - Serialization.
- **`noodle-engine`:**
  - `Node` trait and node registry.
  - Signal buffers with `[voices][channels][frames]` shapes and broadcasting.
  - Compiler steps: cycle check, shape inference, topological sort, buffer allocation.
  - `RenderPlan`, and plan swapping that migrates node instances.
  - Return queue, so old plans are freed off the audio thread.
  - Shared parameter cells, with smoothing.
  - Offline renderer that writes WAV.
- **`noodle-nodes`:** sine and saw oscillators, noise, gain, sum, a state-variable
  filter, and output.
- **`noodle-io`:** a cpal output stream that drives the engine.
- **Dev tools:** a command to render a graph file to WAV, and one to play a graph file.
- **Tests:**
  - Golden renders.
  - Compiler tests.
  - Audio-thread allocation check.
  - A plan swap mid-render produces no clicks.

**Done when:** a graph file plays through the speakers, an edited version can
be swapped in during playback without a click, and CI passes on all three
platforms.

## M1: Patch and hear

The first build that feels like the product.

**Status:** on main are the app shell (the window, theme, panels, files,
undo/redo and play/stop), the parameter widgets and properties panel, the
Scope, Meter and Input nodes, and the audio settings dialog (File → Audio
Settings…). Still open: the node editor canvas (#15), drawing meters and
scopes on their nodes (#18), the widgets on the nodes themselves, and an
end-to-end check of the "done when" line below.

- eframe app shell with Blender-style dark theme and panel layout.
- **Node editor:**
  - Pan and zoom.
  - Shift+A to search for and add a node.
  - Drag to connect, Ctrl+right-drag to cut wires.
  - Box select, delete, duplicate.
  - Reroute points and frames.
- Parameter widgets on the nodes themselves and in a properties panel.
- Live editing: structural edits recompile, parameter edits go straight to the parameter cells.
- Compile errors shown on the wires and nodes they come from.
- Audio input node and a device and settings picker.
- Scope and meter nodes, which exercise the path from engine back to UI.
- Save and load, and undo/redo from the UI.

**Done when:** you can build a patch from scratch, tweak it while it plays
with no glitches, save it, and reopen it exactly as it was.

## M2: Timeline and tracks

It becomes a DAW.

- **Transport:** play, stop, loop, tempo map and time signature.
- Group nodes, with Tab to enter and leave them.
- Tracks as group nodes, fed by clip player nodes.
- Importing audio clips: symphonia decoding, resampling, and disk streaming
  on a worker thread.
- **Arrangement view:** tracks, and moving, trimming and fading clips.
- Mixer view over the track groups.
- Automation lanes.
- Recording audio input to clips.

**Done when:** you can arrange several audio clips on tracks, process them
through node graphs, automate a parameter, and mix them down live.

## M3: Events and polyphony

It becomes an instrument.

- **Port derive macro, before writing the synthesis nodes.**
  - **The problem:** nodes refer to ports by position
    (`const CUTOFF: usize = 1`), and if those constants drift out of step with
    the layout, the node silently reads the wrong input.
  - **The fix:** a derive macro generates the `Layout` and named accessors
    from one declaration.
  - **Why here:** M1 and M2 will have settled the node API through real use,
    and M3 roughly triples the number of nodes.
  - Nodes whose ports depend on config, such as Mix and plugin nodes, keep
    using the `Layout` builder.
- Events signal type.
- MIDI input (midir), MIDI clips and a piano roll.
- Voices node: voice allocation and stealing, producing polyphonic pitch, gate
  and velocity.
- Polyphonic signals working end to end, and the Voice Mix node.
- **Silence skipping.**
  - **Flags:** signals carry a per-lane silence flag, which generalises the
    current per-signal `constant` flag.
  - **The wrapper:** a reusable lane-kernel wrapper skips a lane while all its
    audio inputs are silent, and marks that lane of its output silent, so
    the saving cascades downstream.
  - **Opt-in:** generators such as oscillators make sound from silent
    inputs, so they don't use the wrapper.
  - **Tails:** a lane is only skipped once its output has also gone silent,
    so filter and reverb tails ring out.
- **Skipping finished voices.** Envelopes report when a voice's release has
  finished, so the Voices node can mark it inactive and nothing processes it
  until it's reused. Until then, every voice is processed all the time.
- **Synthesis nodes:**
  - Band-limited oscillators.
  - ADSR envelope and LFO.
  - SVF and ladder filters.
  - VCA, unison and spread.
- **Delay node and feedback loops.**
  - **Now:** the compiler drops any wire that closes a loop, and shows a
    diagnostic on it.
  - **The change:** a node type can declare that its output doesn't depend on
    its input within the same block, as with a delay of at least one block.
    The compiler then allows loops that pass through such a node.

**Done when:** you can build a polyphonic subtractive synth from nodes and
play it from a MIDI keyboard and from a MIDI clip.

## M4: Caching

- The offline renderer as a background service, with progress reporting to the UI.
- **Streaming offline renders.** Offline nodes currently get the whole range in
  memory, about 230 MB for 10 minutes of stereo. Render in chunks instead,
  with random access for nodes like Reverse that need it.
- Content-addressed cache store on disk, with Merkle-style keys.
- Analysing which nodes are cacheable.
- Freezing nodes and groups, with a progress bar on the node.
- **Offline-only node API:** the first nodes are reverse, normalize and
  time-stretch.
- Export dialog for WAV and FLAC, covering the whole project or a range.

**Done when:** you can freeze a heavy group and watch CPU use drop, an
offline node re-renders automatically when something upstream changes, and
export produces output identical to live playback.

## M5: Plugins

- **CLAP hosting through clack:**
  - Scanning in a child process.
  - Plugin nodes.
  - Parameters as ports.
  - Editor windows.
  - Plugin state saved in the project.
- Plugin browser.
- VST3 hosting.

**Done when:** common free CLAP and VST3 plugins (instruments and effects)
load, play, save and restore on all three platforms.

## M6: Performance and robustness

- Running the graph in parallel across real-time worker threads.
- Plugin delay compensation.
- Out-of-process plugin hosting.
- CPU meters per node, and profiling tools.

## Later

- Spectral signal type.
- AU hosting.
- A scripting or expression node.
- MIDI 2.0.
- Surround and multichannel buses.
