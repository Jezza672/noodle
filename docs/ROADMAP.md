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

**Status:** all of M1's code is on main: the app shell (the window, theme,
panels, files, undo/redo and play/stop), the node editor with parameter
widgets on the nodes, the properties panel, the Scope, Meter and Input
nodes (drawn live on their nodes), and the audio settings dialog (File →
Audio Settings…). An end-to-end test follows the "done when" line below
through the app. The review pass is done and its fixes are merged, and CI
passes on Linux and Windows. Left: the listening check on a Mac.

- eframe app shell with Canvas-style dark theme (see `theme.rs`) and panel layout.
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

### M1 follow-ups: wire editing

**Status:** done. Delete and Backspace delete the selection (Backspace is
the key a Mac calls Delete, and was the missing one), double-clicking a wire
breaks it, and dropping a node onto a wire splices it in as the design below
describes.

Small node-editor changes requested after M1 landed. They share the editor's
hit-testing and wire code, so they go in one PR, alongside the M2 app work.

- **Delete key on a selected node.** Pressing Delete (and Backspace) with
  nodes selected deletes them, as one undo step. "Delete" is already listed
  under M1's node editor, so first check why the key doesn't work and fix
  that rather than adding a second path.
- **Double-click a wire to break it.** One `Command` removing that
  connection, as Ctrl+right-drag already does. The click's hit-test is the
  same distance-to-curve test the cut gesture uses.
- **Drop a node onto a wire to splice it in.** The wire's source goes to the
  node's input and the node's output goes to the wire's destination.
  *Proposed default design:*
  - **Trigger:** while a node is dragged, the wire nearest the node's body
    (within a small radius, and only if the node has no connections yet)
    highlights. Releasing over it splices. Holding Alt while dragging turns
    splicing off.
  - **Port choice:** the first input and the first output whose signal type
    matches the wire's (audio onto audio, control onto control), preferring
    the main signal port over a parameter port. If nothing matches, the wire
    doesn't highlight and nothing happens. A node with several candidate
    ports takes the first by position, and the user can rewire from there.
  - **One undo step:** remove the old wire, add the two new ones. The
    compiler's usual shape and cycle checks apply, and a splice that would
    create a cycle is refused with the normal diagnostic.

### Backlog: modulation display (M3)

Both of these are about control signals, so they belong with M3's LFO and
envelope nodes, which are the first sources people will wire into parameters.
The log-versus-linear decision comes first, because the meter's scale depends
on it.

- **Show a modulated parameter's live value.** A parameter with a wire
  into it currently greys out. Instead, its slider becomes a meter:
  - **Value:** the fill shows the parameter's current effective value, read
    every frame.
  - **Range marks:** two ticks mark the minimum and maximum over the last few
    seconds (default about 3), so the modulation depth is visible against
    the parameter's range.
  - **Plumbing:** a connected parameter port gets a telemetry tap, which
    keeps a per-block minimum, maximum and last value in atomics, in the same
    way as `Meter`'s peak. The tap is only added to ports that are wired and
    visible, and is off the audio path otherwise. Real-time rules apply: no
    allocation, and `realtime.rs` is extended to cover it.
  - **Scaling:** the meter uses the parameter's own taper, so it lines up
    with the slider it replaces.
  - **Oscillator levels:** a source's output is shown relative to the
    target's range ("this LFO sweeps 30% of cutoff"), not in dB.
- **Log versus linear for control signals.** *Proposed default design:*
  - **Signals stay linear.** Audio and control signals are plain floats, and
    gain is linear amplitude. Only the UI shows dB, as in the Meter node.
  - **Parameters carry a taper** (linear or log, already in `ParamInfo` for
    frequency) that decides how a value maps to the slider. Modulation adds
    in the parameter's *tapered* space, so a bipolar LFO of depth 0.5 moves
    a log cutoff by the same number of octaves at any base frequency, and a
    linear parameter moves by the same amount everywhere.
  - **Where it's applied:** the engine converts at the parameter, once per
    block: `value = from_taper(to_taper(base) + depth * signal)`, clamped to
    the range. A wire into a parameter port therefore means "offset in the
    slider's space".
  - **Exceptions:** a dedicated exponential converter node (for V/oct pitch
    and dB gain) is available for cases where the user wants the other
    behaviour.
  - **To confirm:** this changes what a wire into a log parameter does today
    (it adds in raw units), so it needs a golden-render check, and
    `examples/vibrato.ron` is the first one to look at.

## M2: Timeline and tracks

It becomes a DAW.

**Status:** Code complete for the "done when", apart from listening on a
real device. The time and automation models are in
[ARCHITECTURE.md](ARCHITECTURE.md), and the engine has a transport (play,
stop, seek, loop, a tempo table), group nodes and tracks, the track input
with audio clips (decoding, resampling, disk streaming, gapless loops), and
automation lanes, including lanes on a track's gain and mute (solo can't be
automated). The app has the transport bar, the arrangement's lanes, ruler,
playhead and movable, trimmable clips with waveforms and draggable fades,
adding audio by drop or import, track headers with mute, solo and gain, Add
track, an editor for gain and mute automation lanes, a mixer view, and
recording into the arrangement. The session feeds the project's lanes,
tempo and clips to the running engine, and the playhead can be set and read
while stopped. The UI follows the Studio direction: inspector on the left,
the arrangement on top, and the selected track's node graph below.

Two acceptance tests check the "done when". The engine-side one
(`noodle-nodes/tests/m2_acceptance.rs`) arranges clips on three tracks, runs
one through a gain node, automates another's output gain and mixes down,
offline and live, checking the levels. The app-side one
(`noodle-app/src/acceptance.rs`) does the user's part through the UI: it adds
tracks, drops clips on them, makes and clicks in a mute lane, opens the
mixer, then mixes down offline (checking the levels) and plays live on a null
device (checking that every clip streams).

**Still open for M2:**

- Nothing has been listened to on a real device. CI now runs Linux, macOS
  and Windows on every PR, so the remaining check is a short `noodle play`
  on the Mac.
- Recording has no latency compensation for takes yet, and hasn't been tried
  with a real microphone.
- The follow-ups below (group ports, track order and delete, mixer
  nodes, several outputs, clip copying) are the rest of the milestone's
  planned work.

- **Transport:** play, stop, loop, tempo map and time signature.
- Group nodes, with Tab to enter and leave them (done; group controls and
  solo come with the tracks).
- Tracks as group nodes. Every track takes MIDI and audio clips alike: a
  track input node with `audio` and `midi` outputs, wired by default into the
  group's output node. Group input and output nodes carry gain, mute and solo
  as parameters, which the track's buttons show.
- Importing audio clips: symphonia decoding, resampling, and disk streaming
  on a worker thread.
- **Arrangement view:** tracks, and moving, trimming and fading clips.
- Mixer view over the track groups.
- Automation lanes.
- Recording audio input to clips (done, apart from latency compensation).

**Done when:** you can arrange several audio clips on tracks, process them
through node graphs, automate a parameter, and mix them down live.

### M2 follow-ups: tracks, groups and the mixer

Requested by Jeremy while trying the M2 build. They're grouped by theme, with
a milestone for each theme, and the numbers are the order in his list. Items
marked **now** are small fixes or bugs worth doing before the bigger work.

**Status:** the node editor polish and the clip and arrangement items are
done (marked below), and so are the spare ports, renaming, adding tracks
through the default mixer and deleting tracks. Still to do: dragging tracks
to reorder them (which needs a stored track order), group output controls
and automation lanes as plain wires (which need parameter ports on boundary
nodes in the engine), then the mixer and node views, and outputs.

**Group inputs and outputs (M2).** Boundary nodes grow ports as you wire.

- **Checked.** The Add Node list doesn't offer group input and output nodes
  (1). It does inside a group (a test now covers it); at the top level they
  would have no group to belong to, so they aren't offered there. If that
  isn't what was meant, say where it was missed.
- **Done.** A group's inputs and outputs are always one more than the number wired,
  with the spare one greyed out, so you can wire into it without limit (2).
  A mixer's inputs do the same (5). Both use one shared "spare port" rule
  that adds ports as wires arrive (a mixer's extra inputs are only hidden,
  never removed, and group ports are removed by hand). Ports are config, so a
  change recompiles; the spare port itself carries no signal and must not
  trigger a recompile when it's only drawn.
- **Done.** Groups can be renamed, and so can their inputs and outputs (7, 8). Track
  renaming already exists (#71), so this extends it to any group and port.
- A group output's gain and mute can be wired from other nodes (14), like any
  other parameter, which also means an automation lane can be a plain wire
  (see below).

**Tracks and the graph stay in sync (M2).**

- **Done.** Adding a track adds only the group and wires it into the default mixer,
  with no new output node (4).
- **Done.** A track's group is named after the track, and the two stay in sync both
  ways (6). A track input node is named after its group (13).
- Tracks can be dragged by their header to reorder them. The order is the
  order of the groups, which the mixer also follows, so it's one undo step
  that changes that order and the arrangement and mixer both follow it.
- **Done.** Tracks can be deleted, with Backspace or Delete on a selected track and
  from the right-click menu (10). Deleting also removes its group.
- **Done.** A track input's outputs grey out when nothing feeds them, for example the
  `midi` output on a track with no MIDI clips (12).
- Adding automation to a track connects a generic automation output on the
  track input node to the track output's parameter input (15), so a lane is
  an ordinary wire.
- **Done.** **Inferred edits never delete anything (16).** When one of these edits has
  to take over an input, it replaces the connection feeding it and leaves
  existing nodes alone. Outputs can fan out, so existing wires from an
  output stay. This is one rule in the graph-editing code, tested once, that
  every inferred edit goes through.

**Mixer as a view over any mixer node (M2).**

- The mixer view maps onto a mixer node in the graph, with a drop-down to
  choose which one, so more mixers can be added and the default one
  removed (11). Today it has one strip per track group
  (see "Mixer" in ARCHITECTURE.md), so this changes the strips to follow the
  mixer node's channels.
- **Node views by double-click.** Double-clicking a Scope node opens a scope
  view pane, and double-clicking a Mixer node opens the mixer view on that
  mixer. Like the mixer view, the scope view keeps no state of its own and
  has a drop-down to choose which Scope node it shows, so one pane can
  follow any scope in the graph. Both read through the telemetry API as the
  node on the canvas does. This makes "open the view for this node" a
  general mechanism that other node types can use later.
- **Meters on mixer inputs.** Each input channel of a Mixer node shows a
  level meter inside the node, next to its port (and the mixer view's strips
  get the same meters, which ARCHITECTURE.md notes are missing). The Mixer
  node would report per-channel peak and RMS through the telemetry hub, like
  the Meter node, with one channel per input. Reporting must stay
  allocation-free, and `realtime.rs` is extended to cover it. The spare input
  (see the auto-growing ports item) has no meter.

**Outputs (M2, after the mixer view).**

- The final output has a built-in scope (18).
- Several output nodes, each tied to one audio device, with at most one per
  device (19), and a view for mapping the output nodes to real devices. This
  needs the engine to drive several streams from one plan, so it's the
  largest item here. Today there is one stream and one device
  (see "Choosing a device" under the audio output in ARCHITECTURE.md).

**Clips and the arrangement (M2).**

- **Done.** Ctrl or Cmd-drag a clip to copy it to the new place (17). More clip
  editing (split, duplicate, slip) is expected after this.
- **Done.** Import audio moves to the File menu, and the timeline's right-click menu
  offers it too (21). **Now.**
- **Done.** With a clip selected, Ctrl or Cmd+Left and Right move the playhead to the
  clip's start or end, and the arrangement scrolls to keep the playhead in
  view. The playhead can already be set while stopped, so this is a key
  binding plus a scroll-into-view call. **Now.**
- Clip waveforms (25) are **already done** (see "Files and waveforms" in
  ARCHITECTURE.md), and need no work.

**Node editor polish (M1 follow-ups).**

- **Done.** **Bug:** selecting a node resets the right-hand panel (the properties
  panel) to its default width. A resized panel should keep its width.
  Likely the panel's egui id changes with the selection, or its width is
  set every frame, so check that first. **Now.**
- **Done.** Centre node names on the title bar (9). **Now.**
- **Done.** Inputs and outputs on a node are separate columns that grow independently,
  rather than sharing a row (26). **Now**, and this includes how wires attach.
- **Done.** A node with a single input or a single output doesn't show a label for it.
  The port sits directly beside the title bar, on the left for an input and
  the right for an output, which also makes such nodes shorter. This is part
  of the column layout change above, and ports keep their names for tooltips
  and the properties panel.
- **Done.** Drag a node's ports up and down to reorder them, so wires can be uncrossed.
  Nodes refer to ports by position, so this is a per-node *display order*
  saved with the node's editor layout, and never changes port indices or the
  compiled graph. The order applies within a column (see the separate input
  and output columns above), and new ports on a growing node
  (see the group items) go last. One undo step per drag. Builds on the
  column layout fix, so it follows it.
- **Done.** Edit menu can delete the selected object (27). **Now.** Same command as
  the Delete key (see "M1 follow-ups: wire editing").
- **Done.** Copy and paste for nodes, including a multi-node selection and the wires
  between them (20). Duplicate already exists, so paste reuses its code and
  adds a clipboard. Pasting is one undo step.
- **Done.** **Auto-arrange (3).** A layout command, bound to a shortcut and to a menu
  item, that tidies the selected nodes or the whole graph. This is complex
  and so gets its own PR: layered layout by topological depth, crossing
  reduction, and one undo step that moves every node. Frames and the nodes in them are left in place, and
  the layout keeps clear of them.

**Buttons bound to nodes (M3).**

- The transport bar can hold buttons that become input nodes in the graph,
  outputting the button's state (23). Wherever a UI control can be a graph
  node, it should be.
- A metronome button (22) is the first one: a default arrangement of a
  button input wired to the mute of the mixer channel for the default
  metronome node. This needs a metronome node, so it comes with M3's
  events work.
- A button shows a distinct state when its node is missing or broken, and
  what it's bound to can be edited (24).

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
