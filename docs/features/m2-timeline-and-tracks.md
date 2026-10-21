# M2: Timeline and tracks

Delivered. This is the record of what was planned and built; open work is in [ROADMAP.md](../ROADMAP.md), the design in [ARCHITECTURE.md](../ARCHITECTURE.md).

It becomes a DAW.

**Status:** Code complete for the "done when", apart from listening on a
real device. The time and automation models are in
[ARCHITECTURE.md](../ARCHITECTURE.md), and the engine has a transport (play,
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

## M2 follow-ups: tracks, groups and the mixer

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
  get the same meters, which the docs/architecture/ files (index: ../ARCHITECTURE.md) notes are missing). The Mixer
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
  the docs/architecture/ files (index: ../ARCHITECTURE.md). A device whose samples play sooner than the main
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
  the docs/architecture/ files (index: ../ARCHITECTURE.md)), and need no work.

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

## Track controls and automation sync

- **Done.** Solo mutes the other tracks again. A solo change is a recompile
  (`Effect::add` treats a `solo.N` parameter as structural), because solo is
  read when the project is flattened and no parameter reaches the other
  tracks' stages.
- **Done.** Tracks resize by dragging the bottom edge of the header (32 to 400
  points), and Alt+scroll zooms every track's height (0.5x to 4x). Both are
  view state in `TimelineState`, not in the project.
- **Done.** The Import audio button in the ruler corner is gone (File menu and
  the lane menu still import). In its place: a loop toggle and the loop's start
  and end bars. The loop is session state (`LoopRegion`), sent to the
  transport's `set_loop`; it is not saved with the project.
- **Done.** Automation tracks and Automation nodes stay in step: `+ Auto`
  already made the node and its lane in one undo step; adding an Automation
  node in the node editor now adds its lane too, and removing an Automation
  track's lane removes the node.
