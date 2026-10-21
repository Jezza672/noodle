# UI

The UI is egui, rendered on the GPU, in the "Canvas" look chosen from the design
workshop (blue-black ground, a lime accent, rounded cards, cable-style wires;
all in `theme.rs`), with
Blender's keyboard-driven editing.
It has these views:

- **Node editor** (`noodle-app/src/editor`): pan and zoom, Shift+A to search
  for and add a node, box select, drag to connect, Ctrl+right-drag to cut
  wires, reroute points, frames, and groups: Tab (or a double-click) enters the selected group
  and leaves the current one, Ctrl+G folds the selection into a group, and a
  breadcrumb leads back up. Inside a group the editor shows only that level;
  frames are top-level only for now.
  - **Tab and focus.** The canvas holds focus and asks egui to pass it Tab,
    which egui otherwise uses to move focus, so Tab can't leave the canvas by
    keyboard. `app.rs` knows the canvas isn't a text field, so the app's
    shortcuts still work. `group_nodes` takes a shared `&Project` and an ID
    allocator, so the editor can call it while the session owns the project.
  - **Auto-arrange** (`editor/arrange.rs`, Ctrl/Cmd+L or Edit > Arrange
    Nodes): a pure function from node sizes and wires to positions, applied
    as one batch of `MoveNode`s, so one undo step. Feedback wires are cut by
    a depth-first walk, nodes go in columns by longest path from the left
    (sources with nothing feeding them move up beside what they feed),
    barycentre sweeps reduce crossings starting from the on-screen order,
    and rows are relaxed towards their neighbours without overlapping.
    Separate pieces are stacked. It moves the selection, or everything if
    nothing is selected, and anchors at the top-left of what it moved.
    Frames, and nodes inside them, stay put and the layout is shifted clear
    of them; arranging inside a frame is left for later.
  - **A custom canvas**, not `egui-snarl`, so the interactions can follow
    Blender's exactly: picking a wire up off an input, cutting and rerouting
    with a stroke, frames that carry their nodes. The full list of inputs is
    at the top of `editor/mod.rs`.
  - **Rebuilt every frame.** `layout::Scene` works out every node's box,
    port and wire end from the project and the registry each frame, so
    nothing is cached that an undo or a recompile could make stale. Input is
    hit-tested against it, then it's drawn.
  - **Edits, not changes.** The editor never touches the project. It returns
    `Edit`s, and a node drag is a run of `Edit::Drag`s ending in
    `Edit::EndDrag`, so it's one undo step.
  - **Reroutes are nodes** (`noodle.util.reroute`, a pass-through), so a
    wire can fan out from one, as in Blender. **Frames** are layout only:
    they're saved in the project, edited through commands, and never reach
    the engine. Dragging a frame moves the nodes inside it.
  - **Node layout.** A node is a title bar (the name centred), then rows
    with inputs down the left and outputs down the right. The two sides fill
    rows independently, except that an input with a parameter field takes
    its whole row. A side with a single port puts its socket on the title
    bar with no label (its name is a tooltip), so such nodes need no row.
  - **Spare ports** (`noodle_core::spare`). A mixer shows its wired inputs
    plus one greyed spare, and a group shows a spare input and output after
    its real ones. The spare is only drawn: nothing is stored until a wire is
    dropped on it (or dragged from it), and then the same undo step raises the
    mixer's `inputs` or adds a port to the group's input or output node, named
    `in1`, `in2`, … or `out1`, … Config beyond the last wired mixer input is
    hidden, not removed, and a group port is never removed on its own,
    because the port may be used inside. A group's input and output nodes have
    a spare port of their own, drawn under their ports, which adds a port the
    same way (the group's spare follows). A port is removed from its node
    with a right-click on its row (`remove_group_port`, which takes the wires,
    controls and lanes on it too, as one undo step). Every edit made on the
    user's behalf goes through `spare::wire`, which replaces whatever fed the
    input, never removes a node, and leaves wires leaving an output alone.
  - **Names.** F2 on a selected group, group input or group output renames it,
    and double-clicking a port row of a group node, or of a group input or
    output node, renames that port in place: the box opens over the port's
    row, where its label is drawn (a double-click elsewhere on a group enters
    it). A group input or output node always labels its ports on rows, and F2
    on one renames its first port. A group's name is its
    `name` config, which a track shares, and the track input inside shows its
    group's name as its title. A group's port is found by its name, so
    `rename_group_port` moves the wires on it in the same step, and refuses an
    empty name, one with a line break, or one another port on that side has. An empty group name puts the default back.
  - **Idle outputs.** A track input's `audio` output is greyed while no clip
    plays on the track, and `midi` is greyed until there are MIDI clips.
  - **Any-type group ports.** A group port nothing is wired to on either
    side takes a wire of any kind (`PortKind::Any`, drawn half in the signal
    colour and half in the event colour; a spare is a grey ring with two
    coloured arcs). Its kind is never stored: `layout::Kinds` works it out
    each frame from the wires on either side of the boundary (the kind of
    what feeds it, else of what it feeds, looking through other groups up to
    a few levels). So the first wire to either side solidifies the port, and
    the far side then only takes that kind; removing the wires frees it
    again. The engine needs nothing: flattening follows port keys, whatever
    the wire carries.
  - **Shown parameters.** `Node::param_shown` holds the user's choice, per
    parameter, to draw it on the node in the graph or leave it off
    (`Command::SetParamShown`, display only). The dot beside each parameter
    in the properties panel makes the choice. With no choice, `Layout::param_shown`
    decides: every parameter on a node with at most four inputs and four
    outputs; on one with more, the parameters its type marks
    `ParamInfo::featured` (the few most worth automating), or the first
    four if it marks none. A parameter with a wire or an automation lane
    stays on the node whatever the choice, and a mixer's per-input gain and
    mute are not subject to it.
  - **Fit.** `F` (or Home) with the pointer on the canvas, or View → Fit
    Graph, fits the nodes of the level being edited in view.
  - **Port order** is display only. `Node::port_order` lists port keys in the
    order the editor shows them, set by dragging a port's label up or down its
    column (`Command::SetPortOrder`, one undo step). Ports are still found
    by key, the compiler ignores the order, and ports it doesn't name go
    last in the node's own order.
  - **Wire editing.** Double-click a wire to break it, or drop a node with
    no wires onto one to splice it in (Alt turns that off). A splice picks
    the first input and output of the wire's signal type, the main signal
    port before a parameter port, and is part of the move's undo step.
    Copy, Cut and Paste (Ctrl/Cmd+C, X, V) keep the nodes, the wires
    between them and frames in an in-app clipboard, pasted at the pointer
    into the group being edited as one undo step. The window turns Ctrl/Cmd+V
    into a paste only while the system clipboard holds text, so a copy also
    puts a short line of text there.
  - **Problems** from compiling are drawn where they belong: a red outline
    and a warning sign on the node, or a red wire, with the message on hover.
  - **Parameters on nodes** are `ParamField`s (see below), one per
    parameter input without a wire. Each node's fields are shown straight
    after the node is painted, so nodes in front cover them, and a field
    under a node in front ignores the pointer so the front node gets it.
  - **Custom node bodies.** Nodes that draw something other than parameters,
    such as meters and scopes, reserve space and draw it in `editor/body.rs`.
- **Timeline** (`noodle-app/src/timeline`, the arrangement above the node
  editor): a lane per track input node, a ruler of bars and beats from the
  tempo map, and clips as wide as their audio at that tempo.
  - **Tempo.** The session's tempo and time signature are fields in the
    transport (`app.rs`, `tempo.rs`): the tempo field edits the stretch of the
    tempo map the playhead is in. View > Tempo lane shows the map as an
    optional stepped lane under the ruler (`timeline/tempo_lane.rs`): click to
    add a change on the beat, drag a point to move it and set its tempo, and
    right-click or Delete removes it (the first change stays at the start).
    Right-clicking the ruler (`timeline/ruler_menu.rs`) offers "Add tempo
    change" and "Add time signature change" at the bar nearest the click;
    signature changes live in the same tempo map, on bar lines only, and show
    as markers on the ruler. Right-click a marker to change or remove it (the
    first signature is the transport's).
    Every edit is a `SetTempoMap` command, so undo and the engine's table follow
    as for any tempo change.
  - **Warped sections' waveforms** (`timeline/warped_waves.rs`) are not the
    file's peaks stretched evenly: a worker thread renders the section as the
    engine plays it (the same `WarpMap`, and for Grain and Beats the same
    `Stretcher` and transients) and the timeline draws its peaks. The render is
    kept under a key of everything it came from (file load, window, warp, tempo
    map; the start only when the tempo changes), and forgotten 30 seconds
    after the last time a section asked for it. While one is being edited only
    the newest request is rendered, and the section keeps drawing its previous
    render until the next is back (a section with none, and looping sections,
    whose runs are drawn separately, show the file's own waveform). The worker
    keeps the last file it decoded, so the steps of an edit do not read it
    again. There is no on-disk cache of it: a render is cheap next to
    the stretcher's speed, and the engine itself stretches live.
  - **Seconds on the x axis**: time is linear, so a clip is as wide as its
    audio is long. In a slower section a bar lasts longer, so its bar lines
    sit further apart. Every tick/x conversion (clips, automation, tempo
    lane, playhead, ruler, snapping, drags) goes through `Axis`, which
    follows the tempo map. Zoom is points per second.
  - **Moving and trimming** are `SetClip` commands, one undo step per drag.
    Each frame's edit is worked out from the clips as they were when the
    drag began, so snapping can't accumulate error. Moves and the grabbed
    edge snap to the snap step (`Axis::snap`: the beat lines, or finer
    divisions of the beat); Alt turns that off. Trimming turns the dragged tick
    back into file frames (`timeline/clips.rs`), and shortens fades that no
    longer fit.
  - **Copying.** Ctrl/Cmd-dragging a clip adds copies in the drag's own undo
    step and moves those, leaving the originals. With a clip selected,
    Ctrl/Cmd+Left and Right take the playhead to the start of the earliest
    selected clip or the end of the latest, scrolling to keep it in view.
  - **Fades** are dragged by the handles on a clip's top corners (clips too
    narrow to trim have none). A handle turns the pointer's tick into
    frames, never snaps, and stops where the other fade begins. Each drag is
    one undo step.
  - **Adding audio.** Dropping files on a lane, or Import audio (the button
    in the corner and File → Import Audio…, which open a file dialog and
    target the highlighted track (its header picked), else the selected
    clip's track, else the first, at the playhead; the
    right-click menu on a lane targets the lane and tick clicked), creates one clip per file, laid end
    to end from the drop position (snapped to the snap step; Alt turns that off).
    A drop on the empty space under the last track makes a new track for the
    files, in the same undo step.
    The clip's length is the file's frame count, read when it's added; a file
    that can't be read, or doesn't say how long it is, is left out and the
    status line says why. All the clips are one `Batch`, so one undo removes
    them. A file inside the project's folder is stored relative to it.
  - **Files and waveforms.** A clip's file is opened for its sample rate and
    length straight away, and its waveform (`noodle_io::Peaks`) is worked
    out by two background workers, so a long file never stalls the UI. The
    columns drawn are cached per clip and only worked out again when the
    zoom, scroll, clip or file changes. Every couple of seconds each file is
    looked at again: one that couldn't be read is retried, a failed waveform
    gets another go, and a changed modification time (a re-export) reads
    the file afresh.
  - **The playhead** is drawn from the transport's position, and clicking or
    dragging the ruler seeks (to the nearest beat; Alt for free). Seeking
    isn't a project edit, so `show` returns it beside the edits.
  - **Track headers** show the controls on the track group's output node:
    mute and solo buttons and a gain slider in decibels (double-click
    resets), written as `SetParam` commands on the group's control port, so a slider drag is one undo
    step. A track input node outside any group has no controls. Solo counts
    if any port has it, so turning it off clears every one, and
    a track silenced by another's solo is dimmed.
  - **Track names** are the track group's `name` config setting. Double-click
    a header's name to type one; Enter keeps it, Escape throws it away, and
    an empty name puts the default ("Track N", by position) back. One undo
    step per rename. A rename whose header scrolls out of view, or whose track
    goes away, is dropped.
  - **Add track:** the button after the last header runs
    `group::create_track`, one undo step, and the new track shows up with
    its controls at once. It also wires the track into the **default
    mixer**, the first top level Mix node, at the spare input after its last
    wired one (raising its `inputs` in the same step). A project with no
    mixer gets a Mix node wired to an Output node, using a top level one whose
    input is free or adding one. So a new track is audible without further
    wiring, and later tracks add only their own group.
  - **Deleting a track:** click its header to select it (Delete or Backspace
    then removes it, when no clip or lane point is selected to take the key)
    or right-click the header for "Delete track". It is one `RemoveNode` of
    the track's group, which takes the track input, its clips and its lanes
    with it, in one undo step.
  - **Automation lanes** (`timeline/automation.rs`): under each track, a
    row for every lane that drives a boundary node of the track's group.
    The track header's `~` menu adds a gain or mute lane, starting as one
    point holding the control's value now, so adding it changes nothing you
    hear. Click an empty spot to add a point (on the nearest beat, Alt for
    free; a mute lane holds and snaps to off or on, a gain lane is
    linear), drag a point to move it in time and value (it can't pass its
    neighbours or go before the start), and right-click it or press Delete
    to remove it. A lane's header has a button to remove the lane. Each
    edit is one undo step. A lane on solo is drawn greyed out with a note,
    since solo is read at compile time and the lane does nothing. A lane
    overrides the control it drives, so the track header's gain slider and
    M button are greyed out with a note while one exists. Selecting a
    point drops the clip selection and vice versa, so Delete only ever
    acts on one.
  - The mixer is still to come.
- **Mixer:** a view over the track groups.
- **Properties panel:** the selected node's config settings, parameters
  and compile problems. The Remap node's curve gets a curve editor
  (`widgets/curve.rs`): drag points and handles, click the line to add a
  point, double-click or right-click a point to delete it; a drag is one undo step. A parameter with a wire into it is greyed out and shows the live value.
  (Its live value and range come from the editor, as on the node.)

The UI only changes the Project by issuing commands, and only reads engine state
through the telemetry API.

## Parameter widgets

`widgets::ParamField` draws any `ParamInfo`, in the properties panel and
(with `compact`) on node bodies, so both behave the same:

- **Sliders** for continuous and unlabelled stepped parameters, in the style
  of Blender's number fields. The fill follows the taper, so a log
  frequency field puts 200 Hz a third of the way along 20 Hz to 20 kHz.
  Drag to change (Shift for fine control), click to type, Backspace while
  hovering or the right-click menu to reset. With keyboard focus, the
  arrow keys nudge the value and Enter starts typing; AccessKit's increment
  and decrement actions work too.
- **Choices** for stepped parameters with labels: a toggle button when the
  labels are Off and On, radio-style segments when up to four short options
  fit the row, and otherwise a drop-down. The drop-down is not egui's
  `ComboBox` but a button that opens a plain area under it (closed by a pick,
  Escape or a press elsewhere), so it does not depend on how the platform
  delivers a click: the combo box would not open for a Mac user. Every
  drop-down in the app is `widgets::Dropdown`, which keeps `ComboBox`'s API
  and reports itself as a combo box to accessibility; don't use `ComboBox`.
- **Units:** values are stored in the unit they're shown in (a 0 to 100
  `Percent` parameter shows 50 as "50.0 %"). Hz and seconds switch to kHz
  and ms, and typed values accept the unit and those prefixes ("2k",
  "250 ms").
- **Edits, not mutation:** a field reports a `ParamEdit` and a `Gesture`,
  and `ParamOutput::edits` turns them into session edits. A drag sends
  `Edit::Drag` each frame and `Edit::EndDrag` on release, so it's one undo
  step, and each step goes straight to the parameter cells.
- **Wired parameters** show what the wire is doing, on the node:
  - **A wire that offsets** leaves the field editable, since it sets the base
    the wire moves. The fill stays at the base, two ticks mark the lowest
    and highest effective value over the last 3 seconds (with a bar between
    them), and a dot marks the value now, all on the parameter's own taper so
    they line up with the slider. How far the ticks reach is the depth of the
    modulation against the range.
  - **A wire that replaces** turns the field into a disabled meter: the fill
    and the number follow the live value, with the same ticks.
  - Stepped (choice) parameters show nothing while wired.
  - The numbers come from the parameter probes described under "Wired parameters report their signal".
- **Config fields** (`ConfigField`) only commit when a drag or typing
  finishes, because changing config recompiles the node.

## Node graphs

Some nodes show what they do as a graph (`widgets/graph.rs`): the ADSR its
envelope, the State Variable and Ladder filters their frequency response, the
LFO its wave. The graph is small on the node (a node body, `editor/body.rs`)
and full size at the top of the properties panel's parameters
(`GraphEditor`). Shapes are drawn from the node's current parameters by pure
functions in `noodle-nodes` (`AdsrShape`, `svf_response`, `ladder_response`,
`lfo_wave`), each tested against the real DSP, so the graph is the sound's
shape and not a sketch. A wired parameter is drawn at its live value.

In the panel the ADSR and the filters have points to drag, which set the
parameters they stand for (`Edit::Drag`s, one undo step per drag): the
attack point's time, the decay point's time and sustain level, the release
point's time, and the filter's cutoff (along a log frequency axis) and
resonance (the height of the peak at the cutoff). A point moves only along the axes whose
parameters have no wire, and is greyed when neither is free. The envelope's time axis fits the
envelope when idle and holds still during a drag, so a point follows the
pointer; dragging past the right edge asks for longer times, and the graph
rescales when you let go. To add a graph for a node type, add a `Kind` and
its drawing and handles to `graph.rs`, and a height in `body::height`.

`widgets/mixing_graph.rs` draws two more. Dynamics shows its transfer curve
(input against output level, from `reduction_db`) with points for threshold,
ratio, knee and makeup, a dot at the level passing through now, and beside it
the step response, the gain reduction climbing at the attack and falling at
the release, each with a point. The node reports its input level and gain
reduction as the telemetry params `level` and `reduction` (the editor reads
them whether or not anything is wired, and `special_value` hands them to the
graph as `@level` and `@reduction`). Parametric EQ shows the combined response
(`eq_response_db`) with a point per band along frequency and gain; a cut or
notch has no gain, so its point moves in frequency only. A node's config can
reach a graph too (`bands`, through `special_value`).

## Mixer

The mixer (`noodle-app/src/mixer.rs`, View > Mixer) is a view over the
project and keeps no state of its own. It shows **one mixer node**: the
default mixer (the first top-level Mix node, which Add Track feeds), or the
one double-clicked in the editor, with a drop-down when there are several.
There is one strip per wired input in input order, named for the track
feeding it, each with a vertical level meter, a fader, a reading that resets
the gain to 0 dB, and a mute. The strip reads and sets the Mix node's own
`gainN`/`muteN` parameters, so a fader move is a `SetParam` (one drag is one
undo step). Resets write the default and never remove the parameter, since a
set control keeps its stage in the compiled graph and removing it would
fade the whole output. A strip's fader or mute is greyed, with the track
header's tooltip, while a lane or wire drives that parameter. Solo stays on
the track header. With no mixer node the view says so.

The Mix node reports each input's peak and RMS through the telemetry hub
(`MixNode` in `noodle-nodes/src/mix.rs`, one meter channel per input, voices
summed and channels averaged), allocation-free like the Meter node and
covered by `realtime.rs`. The editor reads it with the other meters
(`Bodies::input_meters`), and draws a small bar on each input row of the Mix
node and on the mixer view's strips.

## Track order

`Project.track_order` is a list of group IDs, set by `Command::SetTrackOrder`
(one undo step; it has no engine effect). The arrangement and the mixer's
strip names read it through `Project::sort_tracks` (Add Track always
puts a new track last):
groups it names come first in that order, the rest follow by ID, and IDs
that no longer exist are ignored, so deleting then undoing a track keeps its
place. Dragging a track's header on the arrangement writes it, keeping only the
groups the arrangement shows. Saving drops IDs of groups that no longer exist,
so a reused ID can't inherit a deleted track's place.

## Outputs view

View > Outputs lists the project's Output nodes, each with a drop-down of the
output devices (`noodle-app/src/outputs.rs`). Choosing one is a `SetConfig`
of the node's `device`, one undo step, and the session restarts playback on
the devices that result. Choosing "Main output" clears the setting. A device
another Output node has is greyed and names its user, since there can be only
one; the main device isn't offered separately; a saved device that isn't
connected stays chosen and is labelled so. A row also says whether the device
opened (and with how many channels) or why not. "Add output" adds an Output
node tied to the first device that has none. Like the audio settings, the
list of devices is made when the view opens and on Refresh, never per frame.
The properties panel shows only where an Output node plays, and points here.

## Scope view

View > Scope, or double-clicking a Scope or Output node, opens a pane with a
drop-down over the Scope and Output nodes and a larger drawing of the chosen
one. It keeps no
state of its own: it reads the capture the editor already reads from the
telemetry hub (`EditorState::scope_view`).

## Spectrum view and the surround panner

The `Spectrum` node (`noodle.view.spectrum`, category Views) is a Scope that
is read as frequencies: the audio thread only sends samples (voices summed,
the same telemetry channel as the Scope), and `SpectrumAnalyser`
(`noodle-nodes/src/spectrum.rs`) works on the UI thread: a Hann-windowed
4096-sample FFT, scaled so a full-scale sine reads 0 dB, folded onto 128
log-spaced columns from 20 Hz to 20 kHz (the loudest bin where a column spans
several, interpolated where it falls between two). `Bodies` keeps a
`SpectrumTrace` per node (the curve falling at 60 dB/s after a rise, held peaks,
and 120 columns of history at 30 a second); the node's body draws the curve and
the properties panel adds the spectrogram. It needs the engine's sample rate
(`Session::engine_sample_rate`). The spectral wire itself (ADR 0007) still has
no viewer; a Synthesize into a Spectrum shows what a spectral chain does.

A Surround Pan node shows a panner (`widgets/panner.rs`): the layout's
speakers around a ring, each dot sized by the gain `pan_gains` gives it, and
the source at the azimuth's angle, nearer the middle as `spread` grows. In the
properties panel the source can be dragged (angle sets the azimuth, distance
the spread, one undo step per drag; a wired parameter isn't set).

An Output node has a `routing` setting: for each channel of its signal, the
device channel (from 1) it plays on, or `-` for none (`3 4 1 2` swaps the pairs
of a quad signal). Empty keeps channel n on channel n. The plan turns it into
the same matrix the downmix uses, and replaces the downmix: a routing
says where every channel goes, and a channel it doesn't list plays nowhere. The properties panel lists the speakers of a
chosen layout, each with a drop-down of output channels.
