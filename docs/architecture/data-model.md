# Data model

The **Project** is the single source of truth. It holds one global graph:

- **Nodes** have a type ID (e.g. `noodle.osc.sine`), parameter values, config
  values, and a UI position.
- **Parameters and config.** Every setting is a parameter, and so can be
  modulated, unless it changes the node's ports or signal shapes.
  - **What counts as config:** only those few settings, e.g. the number of
    inputs on a Mix, the voice count of a Voices node, or which plugin or
    audio file a node uses.
  - **Why config can't be modulated:** ports and buffers can't change in the
    middle of a block, so changing config recompiles the graph and rebuilds
    the node.
  - **Data that isn't a scalar is config too.** The Remap node's mapping
    curve is a list of Bézier points, which a parameter (one `f32`) can't
    hold, so it is a text config value (`curve`; the empty string is the
    straight line). Rebuilding the node on an edit is what makes it
    real-time safe: `instantiate` builds a 1025-entry lookup table off the
    audio thread, and the audio thread only reads it (`Curve` in
    `noodle-nodes/src/curve.rs`). The two ranges are ordinary parameters, so
    they can be modulated.
  - **Blender has the same split:** most values are sockets, but a few are
    properties drawn on the node body, like a Math node's operation, which
    changes which inputs it shows.
- **Connections** go from an output port to an input port. An input port
  that has a connection uses the incoming signal in place of its parameter
  value, so any parameter can be modulated.
- **Frames** are labelled boxes drawn behind nodes, for organising a patch.
  They're part of the project, so they're saved and undoable, but they're
  layout only and never reach the engine.
- **Group nodes** (`noodle.group`) contain a subgraph and expose ports through
  it. Buses and sends are just wires.
  - **One graph.** A node's `parent` says which group it's inside. Wires only
    join nodes with the same parent, so the editor shows one level at a time.
  - **Ports.** A group has one `noodle.group.input` node and one
    `noodle.group.output` node inside it, and its ports are the ports listed
    in their `ports` config (`id:name` per line, `Node::boundary_ports`). An
    input node has an output port `p<id>` for each of the group's inputs, and
    an output node an input port `p<id>` for each output. The group node's
    own ports are those names, so one string names a port on both sides of
    the boundary, and a rename (`rename_group_port`) rewrites the wires on
    the group in one step. The ID never changes, so nothing inside moves: the
    wires and lanes inside use the ID, not the name. They are structure only,
    so they need no registry entry and flatten drops them. Format 4 files
    only: older files had a boundary node per port, named by its `name`
    config, and `Project::from_ron` migrates them (the lowest-ID node of each
    side keeps all the ports, in ID order, with wires, gain, mute, solo and
    lanes moved to the port's keys).
  - **Boundary parameters.** Every port carries gain (dB), mute and solo as
    parameters of its boundary node, keyed `gain.<id>`, `mute.<id>` and
    `solo.<id>` (`Node::port_controls`, `Graph::group_controls`). Flatten
    keeps a port as a `noodle.group.stage` node, under the port's
    `stage_id` (a node ID no project node can have, built from the boundary
    node's ID and the port's), and points a lane or a wire aimed at the
    port's gain or mute at it, once its gain or mute has been set at all
    (even back to its default), is wired, or it is muted, and drops the rest.
    So a group nobody has touched costs nothing and renders bit-for-bit like
    the flat patch, and a touched one costs a stage that is exact at unity.
    The stage stays once set because adding or removing a node in the audible
    path makes the engine fade the whole output out and in (5 ms each way):
    the first touch of a control on a group costs that one fade, and after
    that gain, mute and solo are parameter changes. Mute is a smoothed 0 to 1
    parameter, so it ramps rather than clicks. A control reset should write
    the default, not remove the parameter, or the stage goes and the fade
    comes back. A boundary node shows a port's `gain` and `mute` as parameter
    ports in the editor (solo has none; it is read from the project), so they
    can be wired like any other parameter. A group has one gain and one
    mute, so only its control port (`Graph::control_port`, its first output
    or else first input, the one the mixer drives) shows them. Another
    port shows them too once its gain or mute is set, wired or driven by a
    lane, so nothing that affects the sound is hidden. A wire into either
    keeps the stage, and flatten joins the wire's source to the stage's
    port. The wire replaces the value set on the node, and wins over a lane
    on the same port (the lane gets the usual "overridden" diagnostic). The
    mixer strip and the track header grey out their fader and mute while a
    lane or a wire drives them.
  - **Lanes as wires in the editor.** A lane on a boundary port's gain or
    mute shows as a wire from a `<Control> lane` output on the group's first
    track input node to that port. It isn't a stored connection: the editor
    draws it from the project's lanes, cutting it removes the lane, and it
    can't be spliced, rerouted or wired from. If a real wire is dropped on
    the port, that wire is drawn and the lane waits behind it.
  - **`create_track`** (`noodle_core::group`) makes a track as one undo step
    by instantiating the hidden library template `system:template:track`
    (see [Library and presets](synths-and-library.md#library-and-presets-m9)): the group, a group
    input `in`, a `noodle.track.input` node (outputs `audio` and `midi`), and
    a group output `out` with `audio` wired to it, in that ID order. The
    output starts with gain 0 dB and mute 0 already set, so the track's stage
    exists from creation (solo and mute act at the output, so the input needs
    none) and the first fader or mute touch is a parameter change, not a
    graph change with a fade. The track input's `midi` output is left unwired,
    and the group has no MIDI port: a track plays a synth by wiring `midi` to
    one inside the group. The arrangement view calls it.
  - **Edits.** Removing a group removes its contents, and undo restores them.
    `group_nodes` folds a selection into a group as one undo step.
- **Tracks** are group nodes of a particular shape (a steering decision from
  the project's owner):
  - **One kind of track.** Every track takes clips of both kinds, MIDI and
    audio, mixed in the same track. There are no audio tracks and MIDI
    tracks.
  - **The track input node** (`noodle.track.input`) sits inside the group. It
    has two outputs, `audio` and `midi`, and the transport plays the track's
    clips out of them at the right times: audio clips out of `audio`, MIDI
    clips out of `midi`. (The `midi` output is an events signal; it stays
    empty on a track with no MIDI clips.)
  - **Creating a track** creates the group, its track input node and the
    group's output node in one step, with the input's `audio` output wired
    into the group's output by default. Undo removes all of it. The new
    group is named "Track N", as the timeline would call it.
  - **A track's gain and mute are the track input's parameters** (`gain`,
    `mute`), passed on as outputs of the same names, which creation wires into
    the group output's gain and mute. The mixer strip, the track header and
    automation lanes all set those parameters
    (`Graph::control_target`); a track whose gain or mute is wired some other
    way falls back to the group output's own.
  - **Mixers are named** by their `name` config (`spare::mixer_name`); the
    default mixer, the one Add track feeds, is "Main". F2 in the graph and a
    text field in the mixer view rename one.
  - **A group's output port carries its controls** as parameters of the
    group's output node: gain, mute and solo. Gain and mute are ordinary
    runtime parameters, so they can be automated and wired like any other.
    For a track they are wired from the track input (below), so the track's
    buttons set the track input's parameters; only solo is set on the output.
    Solo is the exception (below).
  - Every group has such ports, not only tracks, so a nested group has the
    same controls.
  - **Solo** is a mixer-level behaviour: soloing a track mutes the tracks
    that are not soloed. It is read **at compile time** from the solo
    parameters (`Graph::solo_muted`). Among the groups sharing a parent, if
    any is soloed or has a soloed group inside it, every group is muted
    except the soloed ones and whatever they feed (found by following wires
    from a soloed group through any nodes at that level), so a reverb return
    or bus a soloed track sends to stays audible. Soloing a track inside a
    bus mutes that bus's other tracks and the buses the track doesn't feed,
    and keeps its own bus audible. Plain nodes are never muted by solo. The
    mute goes on the group's outputs (its inputs if it has none), which is
    enough to silence it. Once any group on a level has a solo parameter set,
    even to off, every group there keeps a stage, so soloing and unsoloing
    change parameters and not the shape of the graph. Solo can't be
    automated or wired (lanes and wires into a solo parameter are refused
    with a diagnostic once lanes compile), and a lane that interpolated mute
    or solo would flicker around the 0.5 threshold. A runtime solo, driven by
    one shared "any solo" value so it could be automated, can come later if
    it is wanted.
- The timeline and mixer are **views over the graph**, not separate structures.
  The timeline shows the clips that feed each track. The mixer shows one
  mixer node (the first, unless another is chosen or double-clicked): one
  strip per wired input, with a meter and a fader, and it sets the **Mix
  node's own** per-input parameters. With no mixer node it says so.
  - **Mix inputs.** A Mix node has `in1`…`inN` audio inputs, then `gain1`…
    `gainN` (dB) and `mute1`…`muteN` parameter inputs, so each channel's
    fader and mute can be wired or automated like any other parameter. Its
    per-input meters read after the gain and mute. A track's own gain, mute
    and solo stay on the track (its header); the mixer view never touches
    them.

Every change goes through a **command**. Applying a command returns its
inverse, which gives undo/redo for free and gives the UI one place to hook
dirty-tracking and recompiling.
