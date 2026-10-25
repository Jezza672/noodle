# Automation

Settled for M2 (Phase 0): **an automation lane is an implicit source wired
into a parameter port.** It is not a clip and not a node the user wires.

- A lane is part of the project: a target (`node` and parameter key) and a
  list of points. A point is a tick, a value and the curve to the next point
  (hold or linear to start with). Before the first point the lane holds the
  first value, and after the last it holds the last.
- When the compiler meets a lane, it adds an internal automation node that
  reads the transport position and writes the lane's value into the target
  port, as a wire would. So a lane modulates exactly what a wire does, with
  the same signal shape, and nodes need no support for it.
- **Lane and wire.** A wire replaces a parameter's value (see Nodes), so a
  wire into an automated parameter wins, and the lane is greyed with a
  diagnostic. Two sources into one input were never allowed anyway.
- **Without a transport** (a live patch) the lane holds its first value.
- **Hold steps are ramped by the automation node.** An unconnected input
  smooths its own value changes, but a lane is wired in, so its samples reach
  the node as they are. The internal automation node therefore spreads the
  jump into a hold segment (one that follows a hold segment) over the same
  smoothing length the target would have used, capped at the segment's
  length, so a stepped lane doesn't click. The value is a function of the
  tick alone, so it comes out the same however the blocks fall, and a
  stopped transport holds it.
- **How it is built.** `compile_with_lanes` flattens the graph, then adds one
  hidden `noodle.internal.automation` node per lane (category `Internal`, so
  the add-node menu skips it) wired into the target. Its ID counts down from
  `u64::MAX` by lane ID, so it can't clash with a project node and its
  instance carries over between compiles. The points travel as text in the
  node's config, so they are part of its `NodeKey`. A lane on a missing
  node is skipped without a diagnostic; one on a missing port or an audio input, or on a
  parameter with a wire, gets one.
- **Group boundaries.** A lane on a boundary port (target key `gain.<id>`
  or `mute.<id>` on the boundary node) can drive its gain or mute: compiling
  keeps a stage for that port (the same stage a set control keeps), and the
  lane is retargeted to the stage and wired into its port. A lane on
  `solo` gets a diagnostic and does nothing, since solo is read when the
  project is compiled. Solo-muting goes to its own
  `solo_mute` input on the stage (OR-ed with `mute`), so a mute lane can't
  make a track audible while another is soloed.
- **Editing a lane** rebuilds its source node with the new points. The
  source has no state, so the plan stays seamless: the new points apply from
  the next block, like a parameter being moved. If the edit changes the value
  at the playhead, the value jumps (a hold step can click); adding or
  removing a lane changes the wiring, and fades like any other. This is
  `NodeType::stateless()`: a node that returns true (lane sources, Remap) is
  rebuilt without a fade when only its config changes and its shapes don't,
  so dragging a Remap curve doesn't chop the sound. A node with state that
  must survive an edit lists the keys in `NodeType::live_config` instead: a
  change to only those keys carries the instance over, and `retune` hands the
  new values to it (the Sampler's envelope and warp).
- **The parameter widget.** For a parameter with a lane, the widget shows the
  lane's value at the playhead and is greyed like a wired parameter. The lane
  is edited as a lane.
- The lane evaluates every sample, with a binary search for the segment. The
  signal isn't flagged constant, since the node API has no way to flag an
  output.
- Lanes are drawn by the arrangement view under their track, and in the
  properties panel next to the parameter they target.
- **Free automation tracks (M9).** A lane drives one parameter, so it can't
  reach an input with no lane of its own or several at once. The Automation
  node (see Nodes) turns a lane into a signal: the timeline's "+ Auto" adds
  the node and a lane on its `value` as one undo step, and the node's `out` is
  wired like any other output. Its lane is a 0 to 1 slider-style row (not a
  switch), numbered "Automation 1, 2, ..." by node order.
