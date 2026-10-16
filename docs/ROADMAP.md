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

**Done** (M3 batch 1). How it was settled is in "Modulating a parameter" and
"Parameter widgets" in ARCHITECTURE.md. The decision differs from the
proposal below in one way: offsetting in slider space is a per-parameter
setting (`ParamInfo::offset()`), not the rule for every wire, because many
ports take an exact value (pitch, gates, automation lanes, a Mix input), so
the vibrato example and its golden render are unchanged. The text below is
the original proposal.

Both of these are about control signals, so they belong with M3's LFO and
envelope nodes, which are the first sources people will wire into parameters.
The log-versus-linear decision comes first, because the meter's scale depends
on it.

- **Show a modulated parameter's live value.** *Done:* the telemetry probe
  (`Telemetry::read_param`), the properties panel, and the slider on the node
  body as a meter with ticks over the last 3 seconds. A parameter with a wire
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
- Audio takes skip the measured round trip (output latency, plugin delay
  compensation and the input's capture latency, as the driver reports them),
  but this hasn't been tried with a real microphone.
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
- Recording audio input to clips (done, with latency compensation; a real
  microphone is still to be tried).

**Done when:** you can arrange several audio clips on tracks, process them
through node graphs, automate a parameter, and mix them down live.

### M2 follow-ups: tracks, groups and the mixer

Requested by Jeremy while trying the M2 build. They're grouped by theme, with
a milestone for each theme, and the numbers are the order in his list. Items
marked **now** are small fixes or bugs worth doing before the bigger work.

**Status:** the node editor polish and the clip and arrangement items are
done (marked below), and so are the spare ports, renaming, adding tracks
through the default mixer and deleting tracks, track reordering, the mixer
view over any mixer node, the scope view and meters on mixer inputs, group
output controls as wires and automation lanes as plain wires, vertical mixer
meters, meters on Gain and Voice Mix nodes and a gain and mute parameter per
Mix input that the mixer view sets, and outputs (a scope on every Output node,
and several Output nodes each tied to one device). All of this theme is done.

**Group inputs and outputs (M2).** Boundary nodes grow ports as you wire.

- **Done.** A group has one input node and one output node, each holding all
  of that side's ports (it was a node per port), with a spare port to wire to
  and a right-click to remove a port. A port's name is the same inside and
  outside, and its gain, mute and solo are its own. Older projects migrate on
  load (format 4) and render the same.

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
- **Done.** A group output's gain and mute can be wired from other nodes (14), like any
  other parameter, which also means an automation lane can be a plain wire
  (see below).

**Tracks and the graph stay in sync (M2).**

- **Done.** Adding a track adds only the group and wires it into the default mixer,
  with no new output node (4).
- **Done.** A track's group is named after the track, and the two stay in sync both
  ways (6). A track input node is named after its group (13).
- **Done.** Tracks can be dragged by their header to reorder them. The order is
  `Project.track_order`, which the arrangement and mixer both follow, so it's
  one undo step.
- **Done.** Tracks can be deleted, with Backspace or Delete on a selected track and
  from the right-click menu (10). Deleting also removes its group.
- **Done.** A track input's outputs grey out when nothing feeds them, for example the
  `midi` output on a track with no MIDI clips (12).
- **Done.** A lane shows as an ordinary wire from an output on the track input
  node to the track output's parameter input (15). It is drawn from the
  project's lanes rather than stored as a connection; cutting it removes
  the lane.
- **Done.** **Inferred edits never delete anything (16).** When one of these edits has
  to take over an input, it replaces the connection feeding it and leaves
  existing nodes alone. Outputs can fan out, so existing wires from an
  output stay. This is one rule in the graph-editing code, tested once, that
  every inferred edit goes through.

**Mixer as a view over any mixer node (M2).**

- **Done.** The mixer view maps onto a mixer node in the graph, with a drop-down to
  choose which one, so more mixers can be added and the default one
  removed (11). "All tracks" has since been removed: the mixer always shows
  one mixer node, vertical meters and faders per channel.
- **Done.** **Node views by double-click.** Double-clicking a Scope node opens a scope
  view pane, and double-clicking a Mixer node opens the mixer view on that
  mixer. Like the mixer view, the scope view keeps no state of its own and
  has a drop-down to choose which Scope node it shows, so one pane can
  follow any scope in the graph. Both read through the telemetry API as the
  node on the canvas does. This makes "open the view for this node" a
  general mechanism that other node types can use later.
- **Done.** **Meters on mixer inputs.** Each input channel of a Mixer node shows a
  level meter inside the node, next to its port (and the mixer view's strips
  get the same meters, which ARCHITECTURE.md notes are missing). The Mixer
  node would report per-channel peak and RMS through the telemetry hub, like
  the Meter node, with one channel per input. Reporting must stay
  allocation-free, and `realtime.rs` is extended to cover it. The spare input
  (see the auto-growing ports item) has no meter.

**Outputs (M2, after the mixer view).**

- **Done.** The final output has a built-in scope (18): every Output node draws
  one in its body and the scope view can show it.
- **Done.** Several output nodes, each tied to one audio device, with at most
  one per device (19), and a view for mapping the output nodes to real devices
  (View > Outputs). The engine renders one buffer for all the devices'
  channels and the main device's callback hands the rest to the other devices'
  streams through queues; see "Playing on several devices" in
  ARCHITECTURE.md. A device whose samples play sooner than the main
  device's is held back by the difference so the two sound together (up to
  about a sixth of a second; a device slower than the main one can't be caught
  up). **Done:** an extra device that runs at another sample rate than the main
  one is fed through a converter thread (sinc resampling), so neither callback
  does the work.

**Clips and the arrangement (M2).**

- **Done.** Ctrl or Cmd-drag a clip to copy it to the new place (17). **Done:**
  S cuts the selected clips (every clip, if none is selected) at the
  playhead into two clips that play the same samples; Cmd/Ctrl+D copies the
  selection to just after itself; Shift+Alt-drag slips an audio clip's
  contents under its window. A MIDI note spanning a cut is shortened in the
  first half and sounds again at the start of the second.
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

**Buttons bound to nodes (M3).** *First part done:* the Button node, the
Metronome node and the metronome button.

- The transport bar can hold buttons that become input nodes in the graph,
  outputting the button's state (23). Wherever a UI control can be a graph
  node, it should be. **Done for the metronome:** a `Button` node
  (`noodle.input.button`) has a `state` parameter and outputs it, and the
  transport pill presses it with an ordinary undoable `SetParam`. More buttons
  can use the same node.
- **Done.** A metronome button (22). The `Metronome` node clicks on every
  beat while the transport plays, higher on the first beat of the bar, and
  follows the tempo map and signature. The first press in a project adds the
  default arrangement (Button → Metronome `on` → Output, as one undo step), and
  later presses flip the button. It is bound by wiring, not by a setting: the
  button controls the first top-level Metronome whose `on` input is fed by a
  Button.
- **Done.** A button shows a distinct state when its node is missing (dimmed;
  pressing adds one) or broken (red, with the reason on hover), and what it's
  bound to can be edited (24) by rewiring in the node editor.
- **Done.** Other buttons in the pill: every top-level Button node except the
  metronome's shows as a ⏻ button (lit while on, tooltip naming what it
  drives), and the ＋ menu adds one wired to any unwired two-valued
  (Off/On) parameter of a top-level node, as one undo step.
- **Done.** The metronome has `pitch` and `accent` parameters, and counts
  beats from the start of the time signature in force (`Transport::signature_start`),
  so the clicks and the bar accent stay on the meter's grid after a change
  of meter (from the next block, like a tempo change).

## M3: Events and polyphony

It becomes an instrument.

**Status:** batch 5 is done: the `Delay` node and feedback loops (the
compiler runs a loop-breaking node in two steps, see "Feedback loops" in
ARCHITECTURE.md), `Voices` taking a `busy` input from the ADSR's new `active`
output so a voice is retired when its envelope ends, and a fade before a voice
steal (`steal_fade`, with a `fade` gain output). `examples/feedback-echo.ron`
is new, and `examples/poly-synth.ron` and `examples/subtractive-synth.ron` use
the new wires. The modulation backlog (log versus linear taper, the live meter
with min and max ticks) had already landed in batch 1. M3's code is complete;
what is owed is a real-hardware MIDI check and a listening test on the Mac.
Batch 4 is done: silence skipping and finished-voice skipping
(per-lane silence flags, `Skip` and `is_idle` on `LaneKernel`, a `tail` on
`Voices`; see "Silence skipping" in ARCHITECTURE.md), and the rest of the
synthesis nodes: `Square` and `Triangle` band-limited oscillators, a `Ladder`
filter, a `Pan` and a `Math` node (arithmetic on four inputs from a typed
expression), with `examples/subtractive-synth.ron` (the synth
is a group of those primitives, unison included as a nested group, with its
controls as group inputs) and its
golden render. Batch 3 is done: the `Voices` node (voice allocation and
stealing) and polyphonic signals end to end with `Voice Mix`, played from a
MIDI keyboard and a MIDI clip (`examples/poly-synth.ron` and its golden
render); the ADSR now recomputes only its moving times, and offsetting wires
convert held lanes once. (Stealing was hard until batch 5, which fades.)
Batch 2 is done: MIDI input (a port chosen in the audio settings
dialog, the `MIDI In` node), MIDI clips on tracks, and a piano roll (see "MIDI
input" and "MIDI clips" in ARCHITECTURE.md), with `examples/midi-clip.ron` and
its golden render. Batch 1 is done: the port derive macro (`noodle-macros`), events
as a signal type with the `Key` and `Mono Note` nodes, the ADSR, LFO and VCA
nodes, a general-purpose Remap node (input range to output range along a
Bézier curve drawn in the properties panel, `examples/remap-sweep.ron`), and the modulation display (offset-in-slider-space parameters, and a
live meter with min and max ticks on wired parameters), with a monophonic
synth example (`examples/synth-pluck.ron`) and its golden render. Still to
do, in order: silence skipping and finished-voice skipping; the
rest of the synthesis nodes (band-limited oscillators, SVF and ladder
filters, unison and spread); the delay node and feedback loops.

- **Port derive macro, before writing the synthesis nodes.** *Done.*
  - **The problem:** nodes refer to ports by position
    (`const CUTOFF: usize = 1`), and if those constants drift out of step with
    the layout, the node silently reads the wrong input.
  - **The fix:** a derive macro generates the `Layout` and named accessors
    from one declaration.
  - **Why here:** M1 and M2 will have settled the node API through real use,
    and M3 roughly triples the number of nodes.
  - Nodes whose ports depend on config, such as Mix and plugin nodes, keep
    using the `Layout` builder.
- Events signal type. *Done* (the engine already carried them; batch 1 added
  the `Key` and `Mono Note` nodes, test-harness support and an end-to-end
  patch).
- MIDI input (midir), MIDI clips and a piano roll. *Done* (batch 2).
  - Live MIDI is timestamped inside the block and the app plays 128-frame
    blocks; the computer keyboard is a MIDI source (toolbar button); Record
    captures MIDI into a clip with an optional quantise grid and
    output-latency compensation.
  - The 128-frame block is a fixed choice for now (4× the per-block overhead
    of 512); the engine block could follow the device period again.
  - **Still to do:** quantise and copy/paste in the piano roll, more than one
    MIDI port at a time, overdubbing into an existing clip, a count-in,
    keeping the raw take for re-quantising. (Reopening a port that was unplugged and comes back is
    done: the session looks at the port list about once a second.)
- Voices node: voice allocation and stealing, producing polyphonic pitch, gate
  and velocity. *Done* (batch 3).
- Polyphonic signals working end to end, and the Voice Mix node. *Done*
  (batch 3).
- **Silence skipping.** *Done* (batch 4).
  - **Flags:** signals carry a per-lane silence flag, which generalises the
    current per-signal `constant` flag.
  - **The wrapper:** a reusable lane-kernel wrapper skips a lane while all its
    audio inputs are silent, and marks that lane of its output silent, so
    the saving cascades downstream.
  - **Opt-in:** generators such as oscillators make sound from silent
    inputs, so they don't use the wrapper.
  - **Tails:** a lane is only skipped once its output has also gone silent,
    so filter and reverb tails ring out.
- **Skipping finished voices.** *Done* (batch 4, as a `tail` parameter; batch
  5 added the envelope report). `Voices` marks a voice inactive once it has
  been free for `tail` seconds, or as soon as its envelope's `active` output,
  wired into `Voices`' `busy` input, falls; nothing processes an inactive
  voice until it's reused.
- **Fade before a steal.** *Done* (batch 5).
- **Synthesis nodes.** *Done* (batch 4; unison is a group of saws, `Math` detune and `Pan`):
  - Band-limited oscillators.
  - ADSR envelope and LFO.
  - SVF and ladder filters.
  - VCA, unison and spread.
- **Delay node and feedback loops.** *Done* (batch 5).
  - **Before:** the compiler dropped any wire that closed a loop, and showed a
    diagnostic on it.
  - **The change:** a node type declares the input it reads last
    (`NodeType::loop_input`). The compiler then allows loops that pass through
    such a node, running it in two steps when it is on a loop. Loops through
    anything else are still dropped. `Delay` is the first such node, and
    `Voices` (its `busy` input) the second.

**Design rule (Jeremy):** a synth is never a monolithic node. It is a group
built from primitive nodes, with its controls as the group's inputs, editable
after it is made. Add primitives (oscillators, filters, math), not synths.

**Done when:** you can build a polyphonic subtractive synth from nodes and
play it from a MIDI keyboard and from a MIDI clip.

### Live playing without the transport (done)

Requested by Jeremy: play a graph from a MIDI instrument without starting the
timeline. The design is in [ADR 0001](adr/0001-engine-runs-independently-of-the-transport.md)
and "The engine and the transport" in ARCHITECTURE.md. Done in one batch: the
engine starts with the app and the toolbar's Audio toggle turns it off; Play,
Stop and rewind only move the transport; an armed track plays live MIDI
through its own synth (live monitoring); and a failed output falls back to
the system default and then to the new virtual output, which can also be
picked in the audio settings as "Virtual (no sound)". Not done: remembering
the toggle between launches, and retrying the saved device when it returns.

## M4: Caching

**Status:** done. Batch 1: streaming offline renders, the offline renderer
as a background job with progress and cancel, and the on-disk cache store.
Batch 2: Merkle key derivation and cacheability analysis from the compiled
graph, offline nodes running from the cache (Reverse; the compiler no longer
refuses them), and freeze for nodes and groups, with a progress bar on the
node and a cached-audio player in the plan. Batch 3: the Normalize and Time
Stretch offline nodes, offline nodes taking modulated parameters, the export
dialog (WAV and FLAC, the whole project or a range, with offline and frozen
nodes rendered first), the playhead handed to a cached player when it is
swapped in (no dropout), clip files hashed off the UI thread, and clip files
watched for changes. Known limits: an offline node still gets its whole range
in memory, a frozen node plays for a fixed length (the clips' end plus a
tail, at least 30 s), a cached render is keyed by the sample rate it was made
at, and offline nodes take no events.

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

## M6: Performance and robustness

**Status:** M6 is done in code. Batches 1 and 2: plugin delay compensation,
per-node CPU meters, parallel graph processing on worker threads, and
profiling tools (`noodle profile`, block statistics). Batch 3: out-of-process
plugin hosting (off by default; View > Plugins or `--sandbox-plugins`) and
real-time priority for the worker threads. Owed: a hand check of both on the
Mac and Windows (plugins in host processes with a real plugin; `noodle profile
--threads` showing real-time workers), and the macOS audio workgroup join.
A plugin that changes its latency after loading (restart request, latency
`changed`) is not followed until the node is rebuilt.

- Running the graph in parallel across worker threads (done: step dependency
  graph, buffers that keep branches apart, an unparking worker pool, workers
  at real-time priority where the system allows).
- Plugin delay compensation (done: CLAP and VST3 latency, delay lines in the
  plan, offline renders trimmed to the timeline).
- Out-of-process plugin hosting (done: a host process per plugin node,
  audio through shared memory, silence and a restart when it dies or hangs;
  editors of sandboxed plugins embed in host-made windows on macOS and
  Windows as in the app, with the host's main thread as its plugin thread;
  only checked by CI compiling them and by the Linux emulation of that
  arrangement, so the Mac and Windows hand check is owed, including that the
  editor really embeds and gets focus from a background host).
- CPU meters per node (done: View > CPU meters), and profiling tools (done:
  `noodle profile`, per-block statistics with deadline misses).

## M7: Time warping

**Status:** All four PRs and a round of follow-ups are done (the model and Repitch playback; the
engine with Grain and Beats; the clip editor; warp quantise and grooves, below). The
decisions are in CONTEXT.md (warp, clip tempo, beat and warp markers, groove,
section) and ADR 0002 (warping is a clip feature, not nodes).

Audio clips get a warp, applied live on playback. Four PRs:

1. **The model. Done.** A clip's warp (its clip tempo curve and warp markers,
   `ClipWarp`) is separate from the sections that show it on the timeline, and
   sections that show one clip share it by ID. An edit to a shared warp forks
   it for that section (`Project::warp_edit`), so copies stay as they were.
   Clip tempo is seeded from the tempo map for takes recorded in Noodle
   (conversion off, so they sound as recorded) and saved in the project;
   detection, tapping and typing arrive with the editor. Warp markers pin a clip
   moment to a timeline beat; a hidden tempo conversion maps clip tempo to the
   timeline's. Tempo conversion and warp markers are separate switches (warp
   quantise is the third, in PR 4). The only mode is Repitch, played by the
   track input with cubic interpolation, driven by a piecewise-linear map from
   output frames to source frames. Fades stretch with the audio. (Dragging trims
   and fades, and the loop, were disabled here and arrived in PR 3.)
2. **The engine. Done.** Grain and Beats are rendered by a worker thread
   (`noodle-warp`, one per track input) ahead of the playhead into a ring buffer
   per stretched section; the audio thread only copies out of it. Grain is
   waveform-similarity overlap-add (WSOLA); Beats cuts the clip at its
   transients (`noodle_io::detect_onsets`), plays each attack at natural speed
   and stretches only what follows. Seeks, jumps, stops and edits restart the
   stretch at the new place, and the loop's start is rendered ahead so a wrap is
   gapless. `ClipFeeds::set_preview` plays every warped section as cheap
   Repitch while a marker is dragged, so the editor (PR 3) can use it. Tests
   cover pitch, level, stereo balance, block-size independence, seeks, loops and
   the allocation guard (`stretched_clips.rs`, `stretched_playback.rs`,
   `track_input_realtime.rs`); `examples/warp-grain.ron` and `warp-beats.ron`
   have golden renders; `cargo bench -p noodle-nodes --bench warp` measures the
   stretcher (about 150 times real time per clip on one core). Choosing the
   mode in the app waits for the clip editor.
3. **The clip editor. Done.** In the piano roll's slot (double-click an audio
   clip): the clip's own timeline with its waveform, transient ticks, warp
   markers along the top (drag to move, double-click to add, Delete removes, snapped
   to beats; Alt drags free) and the loop brace. Marker drags play as cheap
   Repitch (`set_preview`), and the node now dips when that flips. The
   properties panel has the mode, the two switches, clip tempo (typed, tapped
   or detected from the file's transients by `noodle_io::estimate_tempo`, over
   its first 120 s), loop, gain and fades. Trims, fades, split and slip work on
   warped sections again, in warped beats. **Loop:** `ClipWarp::loop_region`
   (in clip-tempo beats, at least one beat and 0.25 s of playback, or the
   section plays once and the app says so) and `AudioClip::extent` (how far the
   section is dragged out, cutting it short or extending it). A looping section
   always plays from its start to the loop's end and then round the loop, up to
   its end, so dragging the edge never changes what plays before it. A fade-out
   is held to the last pass. The engine lays a looped section out as consecutive
   pieces, each a normal scheduled clip, so the audio thread has no loop logic
   and the repeats are gapless (stretched ones through the head lanes). Pressing
   Warp on an unwarped clip picks Beats for percussive audio (from the
   analysis) and Grain otherwise; the mode can be changed after.
4. **Warp quantise and grooves. Done.** `ClipWarp::quantise` (`WarpQuantise`:
   grid in beats, amount 0 to 1, an optional groove and how much of it) moves
   the markers that *play* (`ClipWarp::effective_markers`) and never the stored
   ones, so turning it off brings the original timing back; the curve, the
   engine and the render cache key all read the effective markers, and a marker
   that would land on or before its neighbour, or squeeze the clip past the
   speed limits, is left out of them. In the clip editor each flag sits where
   it plays, with a line and a ring back to where it was put; the properties
   panel has Quantise, Grid, Amount, Groove and Bake (makes the quantised
   markers the markers, `ClipWarp::baked`), and Pin transients (a marker on
   every detected transient where it plays now, so there is something to
   quantise and a feel to read). A `Groove` is the mean distance of a clip's
   markers from the sixteenth grid over a bar (`ClipWarp::groove`); a clip
   takes another's groove **copied** or **linked**. A link is clip data, not a
   wire (ADR 0002): `groove_from` names the other *clip* (a warp forks when one
   section is edited), and the project refreshes the follower's groove whenever
   a warp or clip changes (`Project::sync_grooves`),
   so undo and redo need not record it. `examples/warp-quantise.ron` has a golden
   render.

5. **Follow-ups. Done.** The session tempo is a field in the transport (with
   the time signature), and View > Tempo lane shows the tempo map as an
   optional stepped lane to add, move and remove tempo changes. In the clip
   editor: **grid offset** (drag the waveform or the ruler, or the panel's
   Offset) lines the beat lines up with the transients, and the clip slips with
   them so it plays from the shifted line; **Detect** is on
   whenever there are enough transients and also sets the offset (a weak guess
   says so); flags can be picked as a group (Shift/Ctrl click, a box in the
   flags' row, Ctrl+A) and moved or deleted together; each transient has a
   handle that a double-click turns into a warp marker. Warped sections in the
   timeline draw a waveform rendered from the warp (`timeline/warped_waves.rs`).

Out of scope for now: pitch shifting, warping MIDI clips, a spectral mode.

**Done when:** you can drop a loop recorded at another tempo, pin its
transients to the grid, hear it follow tempo changes without dropouts, quantise
it non-destructively, and lend its groove to a second clip.

## M8: Warp evaluation

**Status:** Built, apart from what needs a person. Batches 1 to 5 landed earlier (metrics crate, probes and harness, the five songs, borrowed truth, tempo detection in full mixes). The closing batch added the **CI gate** (`noodle-eval/tests/gate.rs`): Noodle's modes on the probes (x0.75, x1.5 and the step) must be no worse than the worst reference stretcher's numbers in `baseline/borrowed.json` on each gated cell, except the cells listed in `baseline/gate.json`, which are worse today and may not get worse (`UPDATE_GATE=1` regenerates the list when cells improve). Only physical checks gate (where onsets land, pitch, loudness); the Roberts and Paliwal features stay out until `noodle-eval validate` has been run on the listening-test dataset (IEEE DataPort, a login). The closing batch also fixed the known detection misses: the onset detector no longer fires dozens of times on a held chord (a block is compared with the average of the 30 ms before it), and `glass-ambient` now gets its tempo from its bells (`detect_treble_onsets`). `funk-engine`'s beat phase can still be half a beat off: its onsets favour the off-beat in count and in loudness, so no rule over onsets can tell, and a downbeat model would be needed. **Needs a person:** CC0 clips for `eval/clips.csv` (`eval/fetch.sh --pin`), the IEEE DataPort download for `validate` and `fit`, and the macOS listening check.

A dev tool that scores the warp methods, so a change to Grain, Beats or Repitch
shows a before and after. Four batches:

1. **Metrics.** A `noodle-eval` crate (Rust) with the Roberts and Paliwal TSM
   features (phasiness, smoothed spectral similarity, peak delta, transient
   ratio, harmonic-percussive transient ratio) plus onset timing, pitch (f0),
   loudness and peak, and stereo width. Alignment uses the song's known source
   and target tempo maps, never Noodle's own warp map (which may come from a
   wrong detected tempo and would hide that error) and never an interpolated
   length. The metrics are validated against the published TSM
   listening-test dataset (CC BY 4.0, downloaded, not committed); one that does
   not correlate with the listeners does not gate CI.
2. **Probes and the harness.** Short controlled signals (click trains, a held
   chord, sweeps, noise bursts, a lone kick) and a harness that renders a
   project natively at a target tempo or tempo map (the *native render*), warps
   the same audio to it (headline: bare mix, detected clip tempo, automatic
   mode; also stems and fixed Grain and Beats), and scores both ways plus the
   round trip A to B to A. The warp under test is tempo conversion only, with no
   warp markers (no pinned transients, no quantise). Repitch is reported as the
   floor, not scored as a contender, because it cannot keep pitch. Tempo grid:
   constant ratios 0.5, 0.75, 0.9, 1.1, 1.5 and 2 (target tempo divided by
   source tempo, so 2 is twice as fast), plus a ramp, a step and a wobble from
   fixed seeds. Output is a metric table with a rank summary, diffed against a
   committed baseline of metric numbers (no audio), which shows a before and
   after and is never a threshold. Once the metrics are validated, CI gates on
   the probes (plus one small stand-in project if needed); a threshold is a
   distance from the reference that must stay no worse than the worst reference
   stretcher's on the same material, and is never set from Noodle's own
   current numbers. (Built: everything above except the gate and the stems.
   The reference stretchers it would be measured against arrive in batch 4.)
3. **Songs.** Five songs of 60 to 90 s with intro, verse, chorus, breakdown and
   drop, written as `.ron` projects through the normal commands (drums, plucks,
   pads, bass, arpeggios; synths and triggered one-shots). A 15 to 20 s excerpt
   of each joins the CI gate. Features a song needs
   that are missing get built if the song cannot exist without them, otherwise
   an issue is filed; the batch ends with the list of gaps found. (Built: the
   songs in `noodle-eval/src/song/`, their `.ron` projects in
   `noodle-eval/songs/`, the `songs` command and `baseline/songs.json`. Every
   instrument is a group node of primitive nodes. Nothing the songs needed was
   missing, so nothing was built for them; the drums are synthesised, since
   there is no sampler. Gaps found, filed as issues: tempo detection found nothing in
   any of the five mixes (#141, fixed in batch 5: it now reads the bass onsets too and finds four of the five within 0.2%, on the excerpt and on the whole song; `glass-ambient` first got none and now gets its tempo from the bells in the treble. The beat's phase is the most common position, and funk-engine's off-beat bass can put it half a beat off), the nodes have no tempo-synced echo or LFO
   (#142), and there is no sampler or drum rack (#143). The excerpts go through the harness in tests that
   check wiring only, like the probes'. Stems are scored from batch 5 on, with `songs --stems`.)
4. **Borrowed truth.** For audio we cannot generate: a fetch script for CC0
   speech, solo instrument and full-mix clips (pinned checksums), a gitignored
   `eval/local/` for any file dropped in, and a `noodle-eval borrowed` command
   that makes reference stretches with Rubber Band R3, Signalsmith and
   SoundTouch (whichever install), each stretcher's latency measured and
   removed before scoring and the audio cached outside git, with only metric
   numbers committed. There is no true time map here, so the map comes from the
   tempo case, the same one asked of every contender. Scores are relative to the
   references' spread, with reference-free checks beside them (onsets land where
   the tempo map says, pitch is kept against the source, loudness is kept); hard
   thresholds come only from the known-truth regime. The ramp, step and wobble
   cases need references that accept a time map: Rubber Band does
   (`--timemap`), SoundTouch and Signalsmith's simple API are constant-ratio, so
   varying tempo has one reference and no spread. (Built: all of it, with the
   one change that Rubber Band's R3 engine is constant-ratio only, because its
   3.3.0 offline R3 returns the wrong length with a time map; its R2 engine is
   the changing-tempo reference. `borrowed --probes` runs the references on the
   probes too, where the native render is known, and `baseline/borrowed.json`
   holds those rows: the reference numbers the CI gate will measure against.
   Not built: the clip list in `eval/clips.csv` is empty, because the build
   environment could not reach a host with CC0 audio to pick clips and pin
   their checksums; add rows, run `eval/fetch.sh --pin`, and paste the
   checksums.)

Out of scope: a learned quality predictor, and the authors' MATLAB evaluation
code.

**Done when:** one command prints the scores of every song, probe and reference
clip for each warp mode, and a change that smears drums or shifts pitch moves
a number and fails the CI excerpt.

## M9: Library, presets and more nodes

**Status:** batch C (#156) restored the 30 Hz low cut and whole-mix compressor on
funk-engine and breakbeat-run; the tempo detector copes with them since the
M8 closing onset fixes, and a regression test covers it. Batches 2 and 3 have
landed: the nodes (Sampler, Note Split, Dynamics, Parametric EQ, Reverb, Arpeggiator, with graphs for Dynamics and EQ
and a "Load sample…" picker), and the warp evaluation songs, whose drums are a
drum rack of committed samples (`noodle-eval/songs/samples`) and whose mixes
run through Dynamics nodes set by the compressor and limiter node presets, a
Parametric EQ and a Reverb. Batch 1 (library and
presets) is done in code: it left out samples by content hash in the library
(the Sampler's store now exists) and shipped no node presets (batch 5 added
them); the system presets are group presets of the instrument templates. The
design is in `CONTEXT.md` (Presets and the group library) and ADRs 0004 and
0005.

**Batch 6:** "Update from template" on a group made from a library template
(right-click; replaces the inside, keeps wires at its ports and its own level,
one undo step); see ARCHITECTURE.md. The same batch fixed #167: the eval
harness warped Auto as if the project were already at the target tempo, so a
clip with no detectable tempo (lone kick, sweep) was never stretched. Auto is
now warped as a user does it (imported at the song's tempo, tempo changed
afterwards), and it scores like Grain on those probes.

**Batch 4 (more nodes and free automation tracks):** ten nodes (Envelope, a
drawn curve played on a trigger with a multi-voice or single-voice setting;
Distortion; Bitcrusher; Chorus; Phaser; Envelope Follower; Sample & Hold; Lag;
Comparator; Clock) and the Automation node, which turns a timeline lane into a
0 to 1 signal wired to any input. The timeline's "+ Auto" button adds one and
draws its lane in a row of its own. Batch 5 added the
Envelope's length in beats (`time` Seconds or Beats, and `beats`), a curve
preview on its body, and system presets for every node (`nodes.ron`, Dynamics
included: Compressor, Limiter, Gate, Expander and variants).

1. **Library and presets.** Node presets (parameters and port-neutral config, same node
   type and port layout, one undo step, never touching wires, lanes, mute or
   solo). Group
   presets, which apply only to a group with the same topology (node types, wires, boundary ports
   and port layouts; IDs, labels, positions and config that does not shape
   ports ignored).
   Group templates: unlinked copies, system ones embedded and read-only, user
   ones one `.ron` file each in the user data dir. A track is a hidden
   template, instantiated only by Add track. UI: a Presets control in the
   properties panel, a searchable Groups section in the Add menu, and "Save
   group to library". Samples are stored by content hash. The existing
   instrument examples ship as the first system templates.
2. **New nodes.** `Note Split` (one output is a note filter; per-output note
   and velocity ranges, `transpose`, optional `rest` output), `Sampler` (one
   sample, root key, loop, one-shot or gate), `Dynamics` (compressor, limiter,
   gate and expander as a `mode`; sidechain; gain-reduction output; lookahead
   reported to plugin delay compensation; a transfer-curve graph with
   draggable points and an attack and release plot), Parametric EQ, Reverb and
   Arpeggiator. Drum rack and multi-zone instruments are groups, and Compressor, Limiter and
   Gate are presets of Dynamics, never monolithic nodes. Each node ships with
   presets.
3. **Warp evaluation songs.** Once the new nodes exist, the generated songs in
   `noodle-eval/src/song/` use them: the drums become a drum rack (a Note
   Split in front of Samplers, closing the sampler gap filed as #143), and the
   instruments and mixes gain Dynamics, EQ and Reverb. The songs' `.ron`
   projects and `baseline/songs.json` are regenerated, and the native renders
   and scores are checked so the warp evaluation sees sampled drums, compressed
   mixes and reverb tails, which the synthesised drums and dry mixes do not
   test.

## M10: Stems

**Status:** Designed (CONTEXT.md, "Stems"; ADR 0006). Nothing built.

AI stem separation (HT-Demucs through ONNX Runtime), used two ways. Research and the
sources are in the project folder `research/stem-warp/findings.md`. Three batches:

1. **Ceiling experiment.** Before any model. `songs --stems` is not enough: `Harness::run_stems`
   scores each part alone against that part's native render, and the song's parts are
   instruments (Kick, Clap, ...), not drums, bass, vocals and other. This batch adds a mapping
   from parts to stems and a new harness mode that warps each stem (Beats for drums, Grain for
   the rest), adds the results and scores the sum against the native mix, next to the
   whole-mix modes and a harmonic/percussive baseline. With perfect stems the residual is zero,
   so it also adds simulated leakage between stems to test the residual. The generated songs
   have no vocals, so the vocals default is untested by this batch. If there is no clear gain,
   stop here. The ADR and docs landed in #166.
2. **Separation and Split to stems.** The separator trait in its own crate (added to
   CLAUDE.md's crate table), with a fake separator for tests, the ONNX backend, the consented weight
   download, a cancellable background job with progress, and **Split to stems** on an audio
   clip: one new track under the source for each stem and for the residual, named
   "<clip> (drums)" and so on, stems written as WAVs in a project-relative folder, all one undo
   step. Audio clips have no mute today (`AudioClip` has no such field), so this batch adds clip
   mute, and the source clip stays in place, muted. Each new clip gets its own new warp copied
   from the source's clip tempo, beat markers and warp settings (never the source's `WarpId`,
   which would link them).
3. **Stem warp mode.** A warp mode alongside Repitch, Grain and Beats. Choosing it separates in
   the background into a hidden cache (clip content, model id and version) in the existing
   per-user `CacheStore`, plays as Repitch until ready, then warps each stem with a fixed mode
   (drums Beats; bass, vocals, other and the residual Grain) and adds them back. A change of model
   or version acts like a `DSP_GENERATION` bump. Splitting afterwards copies from the cache. A
   per-stem mode picker comes only if the evaluation shows the defaults are wrong. Prerequisite:
   `eval/clips.csv` gets real mixes, vocals included, because the generated songs have none and
   M8's borrowed-truth set is empty. It ships only if the evaluation shows a clear gain on most
   songs and those clips.

Out of scope for now: streaming separation, models other than HT-Demucs, and note-level
warping. The note-level idea (neural note onsets placing Beats cuts inside a stem) is a later
step, after Stem warp; true per-note audio extraction is not planned.

## M11: Spectral signals

**Status:** batch 1 done in code (the signal type, Analyze and Synthesize and six
spectral nodes). The design is ARCHITECTURE.md ("Spectral") and ADR 0007.

1. **The signal type and the first nodes (done).** A third wire kind that
   carries FFT frames (one size for the whole engine, 2048 with a hop of 512),
   its plumbing through the compiler, plan, test harness, derive macro and
   editor (violet squares), `Analyze` and `Synthesize` (latency reported to
   plugin delay compensation), `Spectral Gate`, `Spectral Filter`, `Spectral
   Shift`, `Spectral Freeze`, `Spectral Blur`, `Cross`, node presets for each,
   `examples/spectral-shift.ron` + golden, and the allocation test.
2. **Later in M11.** A spectrum display (a spectrogram or analyser node, fed by
   telemetry), more nodes (pitch shift and time stretch on frames, spectral
   compressor, convolution reverb from an impulse response, a noise-profile
   denoiser) and a choice of frame size (a config on Analyze that every
   spectral node on the wire adopts). Delay compensation across spectral wires
   is done (a `SpectralDelay` in front of an early spectral input).

## Scripting node (done)

`Script` (`noodle.util.script`): a per-sample program whose text declares the
node's ports, with state, `if`, the Math functions and `rand`. See
ARCHITECTURE.md ("There are no synth nodes", the `Script` paragraph). Math
stays as the one-line form.

## MIDI 2.0 (done)

Universal MIDI Packets on the MIDI bus (`noodle_io::ump`) and MIDI 2.0 channel
voice messages played by `MIDI In`: 16-bit velocity, note pitch attributes,
per-note pressure, pitch bend and controllers as note expressions, the rest
narrowed to MIDI 1.0 events. See ARCHITECTURE.md, "MIDI input". Delay lines
for spectral wires (`SpectralDelay`) came in the same batch.

## Later

- AU hosting.
- Script node follow-ups: loops with a step limit, arrays or delay lines in
  scripts, MIDI/event ports.
- MIDI 2.0 follow-ups: device input as UMP (midir delivers MIDI 1.0 bytes),
  32-bit values end to end (events and clips), MIDI-CI.
- Surround and multichannel buses.
