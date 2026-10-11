# Noodle architecture

Noodle is a node-based DAW. You build sounds and mixes by wiring nodes
together in an editor modelled on Blender's compositor, and you arrange them in
time on a conventional timeline. This document describes how the system is put
together and the rules each part has to follow. For what gets built when, see
[ROADMAP.md](ROADMAP.md).

## Goals

- Real-time processing with live audio input and output on macOS, Windows and Linux.
- Edit the graph while it plays, without clicks, dropouts or lost state.
- Cached pre-processing: freezing a subgraph to save CPU, offline-only nodes
  that need their whole input, and faster-than-real-time export, all built on
  one mechanism.
- Hosting third-party plugins: CLAP first, then VST3.
- Synthesis and audio processing carry equal weight, and polyphony is part of
  the signal model from the start.

## Stack

| Concern | Choice |
|---|---|
| Language | Rust (stable, edition 2024) |
| UI | egui (via eframe), in the same process as the engine |
| Audio devices | `cpal` (CoreAudio, WASAPI, ASIO, ALSA, JACK) |
| MIDI devices | `midir` |
| File decoding | `symphonia`; resampling with `rubato` |
| Real-time queues | `rtrb` (single-producer, single-consumer ring buffers) |
| Plugin hosting | `clack` for CLAP; the `vst3` bindings or a C++ shim over Steinberg's MIT-licensed SDK for VST3 |
| Licence | GPL-3.0-or-later |

The crates listed here are the starting candidates. They get evaluated as
each milestone needs them.

## Crate layout

```
crates/
  noodle-core     Project document: graph, tracks, clips, commands, undo, serialization.
                 No DSP and no UI. Node types are referred to by string ID.
  noodle-engine   Graph compiler, RenderPlan, real-time executor, offline renderer,
                 the Node trait and the node registry.
  noodle-macros   Derive macros for the node API (`#[derive(Ports)]`). Used through
                 `noodle_engine::Ports`.
  noodle-nodes    Built-in nodes (DSP), registered with the engine's registry.
  noodle-io       Device I/O (cpal, midir), file decoding, disk streaming.
                 Calls into the engine from device callbacks.
  noodle-app      The egui application.
  noodle-plugins  (M5) CLAP hosting (VST3 later), exposed to the graph as nodes.
```

Dependencies only point downwards: `app → io/nodes/plugins → engine → core`. The engine
never opens a device and never touches UI code. It exposes a command/event API,
so it can run headless in tests, in the offline renderer, or behind another UI
later.

## Real-time rules

Code that runs on the audio thread (everything reachable from
`Engine::process`) must never:

- allocate or free memory
- take a lock, including the locks hidden inside `Arc` drop or channel internals
- make a system call: file I/O, logging, thread spawning, sleeping
- run a loop whose length isn't bounded by the block size, voice count or
  graph size

Debug builds and tests enforce the allocation rule with an allocator that
panics if it's used on the audio thread (`assert_no_alloc` or equivalent).
Anything that needs memory or I/O happens on another thread and arrives
through a lock-free queue.

**Subnormals are flushed.** A filter or envelope whose input goes silent
decays towards zero, and its state can get stuck among the subnormal floats,
which most CPUs handle many times more slowly. `Processor::process` sets
flush-to-zero (MXCSR FTZ and DAZ on x86_64, FPCR.FZ on aarch64) while it
runs and restores the previous mode after, so this covers offline renders
too. Stateful nodes also flush their own tiny state once per block (the SVF's
integrators and the Meter's mean square, below 1e-30), so they behave the
same where the engine can't set the mode, such as in the test harness.

**Bad values don't stick.** One infinite or NaN value reaching an
oscillator's phase or a filter's state would otherwise keep it outputting NaN
until it's rebuilt.
- Unconnected inputs ignore non-finite values from `set_param`, and a
  non-finite value in the project is replaced by the port's default.
- Connected signals are deliberately unclamped, so nodes with state check it
  once per block and reset it if it isn't finite. New stateful nodes must do
  the same.
- Parameter ranges (`ParamInfo::min/max`) are for the UI and aren't enforced
  on the audio thread, so a node must cope with any finite value.

## Data model (`noodle-core`)

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
  - **Ports.** A group's ports are the boundary nodes inside it: a
    `noodle.group.input` (output port `out`) per input and a
    `noodle.group.output` (input port `in`) per output, named by their `name`
    config. In M2's first cut they're structure only, so they need no
    registry entry and flatten drops them.
  - **Boundary parameters.** Every group's input and output nodes carry
    gain (dB), mute and solo as ordinary node parameters (`Node::controls`,
    `Graph::group_controls`). Flatten keeps a boundary node as a
    `noodle.group.stage` node, under the boundary node's ID so a lane aimed
    at it reaches it, once its gain or mute has been set at all (even back to
    its default), is wired, or it is muted, and drops the rest. So a group nobody has
    touched costs nothing and renders bit-for-bit like the flat patch, and a
    touched one costs a stage that is exact at unity. The stage stays once
    set because adding or removing a node in the audible path makes the
    engine fade the whole output out and in (5 ms each way): the first touch
    of a control on a group costs that one fade, and after that gain, mute and
    solo are parameter changes. Mute is a smoothed 0 to 1 parameter, so it
    ramps rather than clicks. A control reset should write the default, not
    remove the parameter, or the stage goes and the fade comes back. Boundary
    nodes show `gain` and `mute` as parameter ports in the editor (solo has
    none; it is read from the project), so they can be wired like any other
    parameter. A wire into either keeps the stage, and flatten joins the
    wire's source to the stage's port. The wire replaces the value set on
    the node, and wins over a lane on the same port (the lane gets the usual
    "overridden" diagnostic). The mixer strip and the track header grey out
    their fader and mute while a lane or a wire drives them.
  - **Lanes as wires in the editor.** A lane on a boundary node's gain or
    mute shows as a wire from a `<Control> lane` output on the group's first
    track input node to that port. It isn't a stored connection: the editor
    draws it from the project's lanes, cutting it removes the lane, and it
    can't be spliced, rerouted or wired from. If a real wire is dropped on
    the port, that wire is drawn and the lane waits behind it.
  - **`create_track`** (`noodle_core::group`) makes a track as one undo step:
    the group, a `noodle.track.input` node (outputs `audio` and `midi`), a
    group output `out` with `audio` wired to it, and a group input `in`. The
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
    into the group's output by default. Undo removes all of it.
  - **Group input and output nodes carry the track's controls** as
    parameters: gain, mute, solo and the like. The track's gain, mute and
    solo buttons in the arrangement view show and set those parameters. Gain and mute are ordinary runtime parameters, so they can be
  automated and wired like any other. Solo is the exception (below).
  - Every group gets such an input and output node, not only tracks, so a
    nested group has the same controls.
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

## Signals

There are two kinds of signal, plus a possible third later:

- **Audio:** f32 samples shaped `[voices][channels][frames]`. Normal signals
  have `voices = 1`. Shapes **broadcast** the way arrays do in numpy: in each
  dimension the sizes must match or one of them must be 1. A node runs at
  the broadcast shape of its inputs, so a mono LFO modulates all 8 voices of
  a polyphonic filter without anything special being done. A *Voice Mix* node
  sums voices back down to 1. Audio-rate modulation is just audio, so there's
  no separate CV type.
- **Events:** a per-block list of timestamped events (sample offset within the
  block, note ID, payload) covering notes, note expressions and MIDI. A
  *Voices* node turns events into polyphonic pitch, gate and velocity signals,
  doing voice allocation and stealing.
  - **In the graph:** event ports are their own kind (`Layout::event_input`
    and `event_output`, drawn as diamonds in a different colour). An event
    wire only joins event ports, the compiler gives each event output a
    buffer with room for 1024 events a block (more are dropped, and a node
    that pushes a note-on it can't fit must not count the note as started),
    and an unconnected event input is an empty list. Events don't broadcast
    over voices; they carry their own note IDs.
  - **Nodes so far:** `Key` (`noodle.event.key`) makes notes from a gate
    parameter and a key number, so a Button or any signal can play a note.
    `Mono Note` (`noodle.event.mono`) plays the latest held note as `pitch`
    (Hz), `gate` and `velocity` signals, falling back to an earlier held note
    on release and holding its pitch after the last so an envelope's release
    stays in tune. The track input's `midi` output is also an events port.
    `Voices` (`noodle.poly.voices`) plays many notes at once: its `voices`
    config (1 to 64, default 8; config because it is the outputs' shape) sets
    the lanes of its `pitch`, `gate` and `velocity` outputs, one voice per
    note. A new note takes the free voice that has been free longest, so
    releases ring out; with none free it steals the voice whose note started
    first. A steal fades: the voice keeps its old note while the `fade`
    output (a fourth output, a gain to multiply in after the envelope) ramps
    down over `steal_fade` seconds (a parameter, default 5 ms), then switches
    to the new note with the gate dropping for a sample so envelopes
    retrigger, as `fade` ramps back up. The new note therefore starts
    `steal_fade` late. At 0 the steal is hard: the pitch changes at once,
    which can click on a bright patch. A
    note-on for a key already held retakes its voice. Pitch
    expressions bend only their own note's voice, and a pitch holds after
    release until the voice goes inactive, either when its envelope reports
    it is done (the `busy` input, a feedback loop) or after `tail` seconds (a
    parameter, default 10): see "Silence skipping" below.
    `Voice Mix` (`noodle.poly.voice_mix`) sums voices.
    `MIDI In` (`noodle.event.midi_in`) plays the app's MIDI input port: see
    "MIDI input" below.
  - **Testing:** `Harness::send_events` and `Harness::events` feed and read a
    node's event ports.
- **Spectral:** may be added later, for FFT-frame processing.

### Synths are groups

There are no synth nodes. A synth is a group (`noodle.group`) of primitive
nodes (oscillators, filters, envelopes, VCAs, `Math`), with its controls as the
group's inputs, so it stays editable once created: open it and change it.
An input left unwired leaves the node behind it at the value set inside.
`examples/subtractive-synth.ron` is one, and its unison is a nested group
(five `Saw`s, each detuned by a `Math` node and placed by a `Pan`, into a
`Mix`), so a unison of squares or of anything else is the same group with
another oscillator. `Math` (`noodle.util.math`) takes an
`expr` config such as `a * b + c` over inputs `a` to `d`; it is compiled to a
postfix program when the node is built and run on a fixed stack per sample.
`Pan` (`noodle.util.pan`) is the equal-power stereo placement it uses.
`Delay` (`noodle.util.delay`) delays its input by `time` (seconds, up to 2,
interpolated linearly, offset-style modulation so an LFO can sweep it) and
outputs only the delayed signal; an echo is a Mix, the Delay and a Gain wired
round in a loop (`examples/feedback-echo.ron`). Every lane has its own line,
and a lane silent for a whole line's length is flagged silent.

### Feedback loops

A wire can close a loop only through a node that does not need its input to
write its output. A node type declares that with `NodeType::loop_input`, the
key of the signal input it reads *last*: `Delay`'s `in`, and `Voices`' `busy`.

- **Two steps.** A loop-breaking node that lies on a loop runs as two steps in
  the schedule (`Phase::Output`, then `Phase::Input`): the output half writes
  every output from state it already has (and reads every other input, and
  the event inputs), and the input half, later in the block, reads the loop
  input and updates state. The node implements `Node::process_output` and
  `Node::process_input` for this, and `Node::process` for when it is not on a
  loop (it must be equivalent to the two in a row). In the compiler the
  node becomes two vertices of the dependency graph, the output half
  ahead of everything downstream and the input half behind everything
  upstream, so the loop becomes a chain. Both halves belong to one plan slot
  and one instance; the input half holds no instance of its own.
- **What stays refused.** A loop that reaches the node through any other
  input (a `time` wire, say) or through nodes that are not loop-breakers
  gets the usual `Loop` diagnostic on the wire that closes it.
- **Cost of a loop.** The output half runs before this block's input exists.
  `Delay` therefore reads at least `max_frames` samples back while in a loop
  (the engine's largest block, a constant, so the delay doesn't change with
  the size of the block in hand; about 2.7 ms at the app's 128 frames); shorter times are held to that. A `Delay` that
  is not on a loop has no minimum.
- **Shapes.** The output half is shaped before the loop input is known. The
  compiler assumes a mono loop input, finishes, and if the real input
  differs, starts shape inference again with that assumption, up to four
  passes; a loop that never settles reports a node diagnostic and the node
  does not run.
- **Buffers.** Nothing special: the output half's buffers are allocated at
  its step like any other output, and the input half reads its input buffer
  at its own, later step, so the usual lifetime rules hold.

### Silence skipping

Every lane (one voice of one channel) of a signal carries a *silent* flag:
"this block is exactly zero". Nodes that know it set it (`SignalOut::set_silent`,
or `silence`, which also writes the zeros) and nodes downstream read it
(`SignalIn::is_silent`). A lane is also silent when its whole signal is the
constant 0, such as an unconnected audio input. The flags are cleared when a
node's outputs are handed to it each block, so a node that says nothing is
never claimed silent; they live in a plan-wide array parallel to the buffer
pool, so nothing allocates.

- **The wrapper does it.** A `LaneKernel` opts in with `skip()`:
  `Skip::AnySilent(ports)` for a multiplier or a generator driven by a
  pitch, `Skip::AllSilent(ports)` for a mixer. `PerLane` then skips a lane
  whose inputs are silent *and* whose `is_idle(state)` is true, writing zeros
  to every output and flagging them silent, so the saving cascades. Without
  `is_idle` a filter's tail would be cut off: a lane that is still ringing runs
  until its state has decayed (SVF and ladder below about -140 dB, an ADSR
  that has finished its release). Generators that make sound from silent
  inputs (noise, anything with a reverb tail) leave `skip()` at `Never`.
  The oscillators skip a lane whose *frequency* is silent, since a voice with
  no pitch is not sounding; this differs from a true 0 Hz, which would hold
  a constant, but no parameter range reaches 0.
- **Where it starts.** `Voices` flags a voice's `gate` silent for any block
  in which it stays low, and flags all three of its outputs silent (writing 0
  for the pitch) for a voice that has finished (see below) or never been
  used. Then the envelope goes idle and flags its output, the VCA and the filter
  skip the lane, and `Voice Mix` adds only voices that sound.
- **When a voice is finished.** Either the envelope says so or the `tail`
  does. Wire the ADSR's `active` output (1 while the gate is high or the
  release is still going, polyphonic like the envelope) into `Voices`'s `busy`
  input: a voice that has been busy since its note began goes inactive the
  block its envelope finishes, however long the release was. That wire is a
  feedback loop (the envelope follows the voice), allowed by the loop rules
  below. Without a `busy` wire a voice waits `tail` seconds, which should be
  at least the longest release in the patch; and `tail` also caps a `busy` that
  never falls. A voice stops sounding when it goes inactive.
- **A skipped oscillator doesn't run its phase.** A note on a voice that was
  inactive therefore starts the oscillator at phase 0 rather than wherever it
  free-ran to.
- **Real-time rules** hold: a skip is a branch and a memset.

### MIDI input

A MIDI port is chosen in the audio settings dialog (`AudioConfig::midi_input`,
a port name, saved with the other device preferences). The choice belongs to
the app, not the project, like the audio device, and changing it doesn't
restart the audio.

- **From the port to the graph.** `noodle-io/src/midi.rs` opens the port with
  `midir`. The driver calls back on its own thread, which keeps only whole
  channel messages (system exclusive and real-time bytes are dropped) and
  pushes each as three bytes into a ring buffer per `MidiBus` subscriber.
  Each `MIDI In` node subscribes when it is built and pops its own buffer on
  the audio thread, so neither side locks, waits or allocates; a node that
  falls 1024 messages behind misses the newest. Dropping the node
  unsubscribes it.
- **What `MIDI In` outputs.** Note-ons and note-offs become note events (a
  note-on with velocity 0 is a note-off, velocity is 0 to 1), with a note ID
  made from the channel and key and its top bit set, which keeps it apart
  from the IDs of notes in clips. Controllers, pitch bend, pressure and
  program changes pass on as raw `Midi` events. All notes off and all sound
  off end the notes it still holds. Its `channel` parameter (0 for all)
  filters.
- **Timing.** Every message is stamped with its arrival time on a monotonic
  clock (`noodle_io::now_micros`), under the bus's lock so stamps are in
  queue order. Each device callback tells the bus's `AudioClock` when it ran,
  and for each engine block inside it which span of arrivals that block
  plays: the callback's length before the block's own place in it. A note
  played at some point of one device period therefore sounds at the same
  point of the next, in whichever block that falls, and what is later than
  the block's span waits for the next. Live playing is one device period late
  with no jitter beyond the callbacks' own, however long the period is. The
  callback also publishes an anchor, a pair of the clock and the transport's
  sample, that recording reads (see "MIDI takes"). Without a device the node
  plays whatever arrived since its previous block began. The app and
  `noodle play` use 128-frame engine blocks (about 2.7 ms), and renders do
  too, so a render matches what was heard; that costs more per-block overhead
  than 512 did. Events a node emits are kept in time order even if the bus
  hands over two messages out of order.
- **The computer keyboard.** A toolbar button (or M) turns the home row into
  an octave of keys, as in Ableton: A W S E D F T G Y H U J K and O L P ;
  are the notes from C3 (60), Z and X change the octave, C and V the
  velocity. The keys are sent to the same bus as a hardware port and are
  taken out of the frame's input, so nothing else (X to delete, say) acts on
  them; a text field, a dialog or the window losing focus lets go of held
  notes. It is app state, off at start, and not saved.
- **Auditioning.** The piano roll's keyboard holds keys down on the clip's
  own track input (`ClipFeeds::audition`, a bitmask in the node's shared
  state, so no queue and no allocation), and the track input turns changes
  into note events on its `midi` output, with note IDs in their own range.
  So the track's synth plays them, other tracks and `MIDI In` nodes don't,
  and they can't collide with a live keyboard. They sound whether or not the
  transport runs. A key still down when the roll closes is let go.
- **Channel filter.** A held note's note-off always gets through, so changing
  `channel` (which can be modulated) can't strand a note; all notes off ends
  the rest across blocks if the event buffer fills.

### MIDI clips

A MIDI clip (`ClipContent::Midi`) is a length and a list of notes, all in
ticks: a note has a start from the clip's start, a length, a key (0 to 127)
and a velocity (0 to 1). Unlike an audio clip's, its length follows the tempo,
because it is music and not a recording. A clip's notes can be in any order;
the commands that add and change clips check them (no empty notes, none
before the start, keys and velocities in range).

- **Playing them.** `ClipFeeds::update` turns each note into samples with the
  tempo table (the note's end is cut at the clip's end, and a note that starts
  past the end is dropped) and sends the sorted list to the track input along
  with its audio schedule, so one version number covers both and an offline
  render waits for both. The track input's `midi` output gets a note-on at the
  sample a note starts and a note-off where it ends. Note IDs in clips use the low 30 bits, so they can't collide with live MIDI (top bit) or the
  piano roll's keyboard. A note is known by its
  clip, start and key, so editing other notes while it sounds doesn't cut it;
  if the note itself is moved, resized or removed, the old one ends at once.
  At most 128 notes sound at once on a track.
- **Stopping and jumping.** When the transport stops, or a block doesn't
  continue from the one before (a seek or the loop's wrap), every sounding
  note gets a note-off at the start of that block, so nothing hangs. Playing
  from the middle of a note doesn't start it.
- **In the arrangement.** A MIDI clip is drawn as a block with its notes
  shown small. It moves and trims like an audio clip, with two differences:
  it has no fades, and trimming the left edge keeps the notes where they
  sound (the ones before the new start are cut off or shortened) instead of
  shifting them with the edge. The lane's right-click menu has *New MIDI
  clip*, a bar long at the beat nearest the click.
- **The piano roll** opens below the arrangement when a MIDI clip is
  double-clicked. Dragging on empty space draws a note, dragging a note moves
  it (in time and pitch, a whole selection together), dragging its right end
  resizes it, Delete removes, the arrow keys transpose and nudge, and a bar in
  the velocity lane sets velocity. Notes snap to a grid step (a beat down to
  1/16 of a beat), Alt turns snapping off. Each gesture is one undo step,
  worked out from the notes as they were when it began. Notes carry an `id`
  that is unique in their clip (the project assigns one to any note that
  repeats an earlier id when a clip enters it, which also fixes files from
  before ids), and the selection is a set of ids, so it follows its notes
  through undo and redo.

### Recording into the arrangement

A track is armed in the session (the R on its header), which is app state and
not saved with the project. Record needs an armed track and a saved project,
since takes are stored next to it, in a `<project name> recordings` folder as
`take-001.wav`, `take-002.wav` and so on, and a clip's source is a path
relative to the project file like any other. Record starts the stream and the
transport if they aren't running and starts `Playback::start_recording` at
the playhead. Ending the take (the button, stopping, pausing or seeking the transport, or
the stream closing, say for an output change) stops the recorder and adds one
audio clip of the whole take, starting where recording began, to every armed
track as a single `Batch`, so one undo removes the take. An armed track that
was deleted meanwhile gets no clip (if all were deleted, the file is kept and
said so), and a take with no audio adds nothing and
deletes its file. Input that was dropped because the disk stalled is
reported. The clip is placed at the playhead without compensating for the
audio device's latency, so a take can sit a little late; that is still to do.

**MIDI takes.** The same Record also captures MIDI. It subscribes to the
session's MIDI bus for notes (hardware ports and the computer keyboard alike)
and keeps each message with its arrival time. Every UI frame the take notes
the audio thread's newest anchor with what it collected, and maps each
message to the timeline through it, so the stream's startup delay and clock
drift don't matter. Times are moved earlier by the output latency the device
reported (`Playback::output_latency`), since the player was playing along to
what they heard, and the clip starts that much before the playhead was when
Record was pressed. Like an audio take, a MIDI clip goes on every armed track,
whether or not it takes MIDI, so arm only the tracks that should get one. The
take ends with one MIDI clip per armed track from the start of the take: a note still held ends where the take
does, a note-off with no note-on is ignored, and a take with no notes adds
no clip. The audio and MIDI clips of a take are one `Batch`. Audio is recorded
only when an input is chosen in the audio settings; without one a take is
MIDI alone. The Q menu in the transport sets a grid (off, 1/4, 1/8, 1/16,
1/32, 1/8 and 1/16 triplets) that the starts of the notes snap to when the
clip is made, keeping their lengths, shortening a note that now runs into the
same key's next one and dropping a second note on the same tick. What is
heard while playing is never snapped. The raw timing is not kept.

## Automation (M2)

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
- **Group boundaries.** A lane on a group's boundary node can drive its
  `gain` or `mute`: compiling keeps a stage for that node (the same stage a
  set control keeps), and the lane is wired into the stage's port. A lane on
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
  so dragging a Remap curve doesn't chop the sound.
- **The parameter widget.** For a parameter with a lane, the widget shows the
  lane's value at the playhead and is greyed like a wired parameter. The lane
  is edited as a lane.
- The lane evaluates every sample, with a binary search for the segment. The
  signal isn't flagged constant, since the node API has no way to flag an
  output.
- Lanes are drawn by the arrangement view under their track, and in the
  properties panel next to the parameter they target.

## Caching (M4)

Export, freeze and offline-only nodes all rely on one property:
**`Engine::process` doesn't care who calls it.** The offline renderer runs a
plan faster than real time on a worker thread and writes the output somewhere
other than a device.

**Node modes.** Each node type declares one of two modes:

- **Realtime:** processes one block at a time.
- **Offline:** needs its whole input before it can produce output (reverse,
  normalize, high-quality time-stretch, whole-file spectral processing).
  Offline nodes implement a render-over-range API, and their output is always
  a cached render.

**What counts as cacheable.** A node's output is cacheable if it's a pure
function of the timeline: everything upstream is deterministic and nothing
depends on live input or a device. A node type says it isn't with
`Layout::nondeterministic()` (the audio input, MIDI In). A node on a feedback
loop isn't cacheable either, and neither is anything that follows a node that
isn't. An offline node with a non-cacheable input gets a `NotCacheable`
diagnostic, and plays silence.

**Cache keys.** Keys are computed Merkle-style, by `noodle_engine::analyze`.
It compiles the project as the live engine does, with groups flattened and
automation lanes turned into nodes, and walks the schedule upstream first:

```
key(node) = hash(node id, type_id, type_version, config, sample_rate, range,
                 tempo map, each input: constant | key(source output), shapes,
                 NodeType::cache_inputs(…))
key(output) = hash(key(node), output index)
```

- The node's ID is in the key because its random seed comes from it, so two
  identical noise nodes render differently.
- The lane that drives a parameter is a node in the compiled graph, so its
  points are in the key of everything it reaches.
- The timeline lives outside the graph, so a node type contributes it through
  `cache_inputs`. A track input hashes each clip it plays: its position, its
  gain and fades, and for audio the *content hash* of the file
  (`noodle_io::FileHasher`, remembered while size and modification time hold),
  so a file replaced under the same name gives a new key.
- Every key also holds the crate version and `DSP_GENERATION`
  (`noodle-engine/src/cache.rs`), so a build whose sound changed never replays
  renders from the shared per-user cache. Bump the constant with any
  regenerated golden render.
- The render length is in every key, so lengthening the project (a clip
  dragged further right) renders every frozen target again, even those it
  doesn't feed. The length reaches the last clip, automation point or tempo
  change, plus a 4 s tail, and at least 30 s. Past it a frozen node is
  silent, so a tail longer than that, or a drone that should play on, is cut.
- Clip files are hashed whole, but never on the UI thread. The UI keys the
  project with `Freezer::analyze_known`, which only looks at a file's size and
  modification time (`FileHasher::peek`) and lists the files it has no hash
  for. `Freezing` hashes those on a thread of its own (`spawn_hash_files`);
  until it finishes, no render starts, offline nodes are silent, frozen nodes
  play live, and their badges say *waiting*. The session compiles again when
  the hashes are in. The renders themselves (`freeze`, `export`) hash on their
  own threads with the blocking `analyze`. A file that can't be read is
  remembered as such until its size or time change, so it isn't retried every
  compile.
- **Watching clip files.** Once a second the session looks at the size and
  modification time of every audio file a clip plays (`filewatch.rs`; a
  `stat` each, no platform watcher). A changed file schedules the clips
  again, which opens new streams (a clip's source includes the file's
  modification time, so streams on the old file are dropped), and compiles
  again, which keys frozen and offline nodes afresh and renders them. Waveforms
  watch their files separately (`timeline::sources`). A file rewritten in
  place with its modification time put back is not seen, the same trade-off
  the hasher makes.
- An offline node can take a modulated parameter: the render taps the wire
  like any other input, and a wire into an offsetting parameter is turned into
  the parameter's value by `apply_offset`, exactly as the real-time plan does
  (`value = from_travel(to_travel(base) + signal)`). The node then reads that
  parameter as a signal over the whole range, like any input (`io.inputs[P]
  .lane(0, 0)` is the value at each frame), so a ratio or a target level can
  move along the range. It can't take events yet; that is reported as not
  cacheable.
- Renders are stored on disk by key. An edit produces a new key, so stale
  data can never be served, and undoing an edit brings the old render back
  straight away. Bump a node type's `version` whenever its DSP changes.

**Targets.** What gets rendered is a *target*: an offline node, or a node or
group the project marks frozen (`Project::frozen`, set with
`Command::SetFrozen`, so it undoes and is saved). A target is a set of outputs
in the flattened graph; for a group they are the nodes whose outputs feed the
group's output ports. `Freezer::plan` says whether each has its render in the
store (`Ready`), lacks it (`Missing`), or can't be rendered (`Blocked`).

**Playing from the cache.** The compiler takes `Replacements`: for an output,
a source node to play in its place. `replace.rs` rewires the readers of that
output to the source and drops every node that only fed it, so a frozen
subgraph costs nothing, track inputs and lanes included. `CachedPlayer`
(`noodle-nodes`) is the source. Live it streams chunks from the store through
a worker thread, in the manner of clip streams: the audio side takes full
chunks and returns spent ones through two lock-free queues, a jump asks the
worker to restart, and a loop wraps without a gap because the worker follows
the transport's loop range. In an offline render it reads the file itself.
Nothing in `process` allocates (`tests/realtime.rs` checks, with seeks and a
wrapping loop).

Until a render exists a frozen target keeps playing live, and an offline node
plays silence (a `CachedPlayer::silent`). An edit that changes a target's key
takes its render off at once (the app compiles again straight away), so stale
audio is never heard during a drag; only starting the new render waits for the
editing to pause. When a render finishes during playback the player is swapped
in with the playhead somewhere past its start. `Setup::position` carries the
transport's position as of the compile, and the player starts its worker at
the chunk holding it and reads the next few chunks before the plan is sent
(instantiation is off the audio thread), so the first block after the swap
plays instead of waiting for the worker.

**Rendering the cache.** `freeze` / `spawn_freeze` (`noodle-nodes`) renders
every `Missing` target, upstream first, on a background thread. A target is
rendered by `render_taps` (`noodle-engine`): the graph is cut down to what
feeds the target's outputs, a *tap* sink is wired to each output, and the
ordinary engine runs, handing each block to the tap, which writes the cache.
Renders earlier in the chain stand in as blocking players, so two frozen or
offline nodes in a row cost each only its own work. An offline node is
rendered by tapping its inputs over the whole range, running the node
(`render_offline_node`, which builds it as the compiler would, with the same
seed) and storing its outputs. A render that couldn't read a clip file is
never stored. The app keeps renders in step with the project
(`noodle-app/src/freezing.rs`): it looks at the cache on each compile, starts
one background render for what is missing, cancels it if the project changes
meanwhile, and shows a progress bar on the node.

**Batch 1, the pieces underneath.**

- `render_project_streaming` (`noodle-engine`) renders in chunks into a sink,
  with progress and cancel, so memory stays flat. `render_project` is still
  the in-memory version; both give the same samples.
- `Job` (`noodle-engine`) runs work on a thread with `fraction()`,
  `cancel()` and a non-blocking `poll()`. Dropping it cancels. Nothing in
  it touches the audio thread.
- `spawn_render` and `spawn_render_to_cache` (`noodle-nodes`,
  `background.rs`) are the offline renderer as a background service. The
  render has its own engine and a blocking track-input registry, so it
  never disturbs live playback and renders the same samples every time.
  A render with a missing or unreadable clip is *incomplete* and is never
  cached, since the cache would keep serving it after the file came back.
- `CacheKey` and `KeyBuilder` (`noodle-core`) are the Merkle hashing
  (blake3). Fields are type-tagged and length-prefixed. Deciding *what*
  goes into a node's key is `analyze`'s job, above.
- `CacheStore` (`noodle-io`, `cache.rs`) keeps one file per key: a 32-byte
  header and raw `f32`, so `read_frames(start, ..)` is a seek. That is the
  random access Reverse needs. Entries are written to a `.partial` file and
  renamed on commit, a truncated or corrupt entry is deleted and counts as
  a miss, and `evict_to` drops least-recently-used entries.

**Offline nodes.** `Reverse`, `Normalize` and `Time Stretch` (`noodle.offline.*`)
get the whole range as one signal per input and write the whole range of each
output, so every output is as long as the range.

- *Normalize* measures the peak (or the RMS) over every lane at once, so
  stereo balance is kept, and scales to `level` (dB; a wire makes the gain
  per sample). Silence passes through.
- *Time Stretch* is waveform-similarity overlap-add (WSOLA): grains of `grain`
  seconds are taken from the input at the speed `ratio` asks (2 is half speed,
  a wire moves it along the range), each shifted within a quarter window to
  line up with the one before, summed with a Hann window and divided by the
  window sum. All channels of a voice share the grain positions, so the image
  holds. The output is as long as the range: slowed down, the input's end
  falls off it; sped up, it ends in silence.

**Export** (`noodle-nodes/src/export.rs`, `spawn_export`) writes the project
to WAV (32-bit float, 24 or 16) or FLAC (24 or 16; `noodle_io::ExportWriter`,
which writes a `.partial` file and renames it, so a failed or cancelled export
leaves nothing). It hears what playback hears: the missing renders of offline
and frozen nodes are made first (the same `freeze`, into the same cache, with
the same length that playback keys them by, so they are the same renders),
then `render_project_streaming_replacing` renders the project from the start
with those renders standing in, blocking players for exact output. A range
(`start..end` in frames) only chooses which part of that render is written;
the render always runs from the start, so delays and envelopes are in the
state they would be in on the way there. Offline renders cover the playback
length (the timeline and a tail, at least 30 s); a range that ends later gets
renders of its own, as long as the range (a different key), so it never goes
silent partway through.
Offline nodes that couldn't be rendered are silent in the file and listed in
the report. Progress is the renders of nodes first (half the bar), then the
file. The app's File > Export Audio… dialog (`export_dialog.rs`) asks for a
format and the whole project (to the last clip, plus 4 s of tail) or a range in
seconds, then for a file, and runs it on the session (`session/export.rs`) with
a progress bar and cancel. Exports are stereo, at the stream's sample rate.
Integer formats cut off samples past full scale. FLAC blocks are chosen so the
last one is never shorter than FLAC's 16-frame minimum, and its header says
every block but the last is the stream's block size, as some decoders
(Symphonia) insist. FLAC frames go to disk as they are encoded, after a
placeholder header that `finish` overwrites with the totals, so memory doesn't
grow with the length. Without a modification time (some filesystems), a file's
size alone says whether its hash is still good.

Plugins are treated as deterministic by default. Each plugin node can opt
out, for plugins that use randomness.

## Plugins (M5)

A plugin is a node. Its parameters become input ports, and its audio and note
ports become graph ports. Plugin state is saved in the Project.

- **CLAP** is hosted through `clack` (`noodle-plugins`).
- **VST3** is hosted through the Rust `vst3` bindings (raw COM, loaded with
  `libloading`): see "VST3 hosting" below.
- **AU** may come later. Most macOS plugins also ship as VST3 or CLAP.
- Plugin editors open as separate native windows: embedded in a window the host makes (macOS, Windows), or the plugin's own floating window (Linux, or when it can't embed). Both run on the plugin thread.
- Scanning runs in a child process, so a plugin that crashes during scanning
  can't take down the app.
- **Out-of-process hosting (M6)** runs plugins in sandbox processes that
  exchange audio through shared memory, so a crashing plugin can't take down
  the session.
- **Latency:** plugins report their latency (CLAP's latency extension, VST3's
  `getLatencySamples`, both read once after activation, on the plugin thread) and the engine
  compensates for it; see [Performance (M6)](#performance-m6).

### CLAP hosting

**Scanning.** `ScanCommand` runs a scanner in a child process for each
`.clap` file (`noodle-plugin-scan <file>`, or `noodle` / `noodle-app` started
with the hidden `--noodle-scan-plugin` argument, which their `main` hands to
`run_scan_child_if_requested`). The child loads the file, instantiates each
plugin to read its audio ports, note ports and parameters, and prints one JSON
line (`Descriptor`s). A crash, a hang (30 s timeout, then it is killed) or a
file that isn't a plugin comes back as an `error` on that file's `PluginFile`,
not as a failure of the scan. `ScanCache` keeps the results in the user's
cache folder, keyed by each file's size and modification time, so a file is
scanned again only when it changes (a failed one too). The standard folders
are `~/.clap` and `/usr/lib/clap` on Linux, `/Library/Audio/Plug-Ins/CLAP`
(system and user) on macOS and `%COMMONPROGRAMFILES%\CLAP` and
`%LOCALAPPDATA%\Programs\Common\CLAP` on Windows, plus `CLAP_PATH`.

**Node types.** A `Catalog` builds one `NodeType` per scanned plugin, with ID
`clap.<plugin id>` and category "Plugins". Ports come from the scan alone, so
a project's graph compiles without loading any plugin code. The layout
depends on one setting, `params`, which lists the parameters that get ports
(below); it never depends on the plugin's code:

| Port | Key |
|---|---|
| audio inputs, one per CLAP audio input port | `in`, `in2`, ... |
| a parameter input per exposed parameter (see below) | `p<param id>` |
| audio outputs | `out`, `out2`, ... |
| note input ports (all are read) | `notes`, `notes2`, ... |
| note output ports (all are written) | `notes_out`, ... |

**Exposed parameters.** A synth can have hundreds of parameters, which would
make its node unusably tall, so only the ones listed in the node's `params`
setting get ports (comma-separated parameter ids; `none` for no ports). That
is config, since it changes the ports. A node without the setting shows every
parameter, so projects saved before it existed keep their ports; the plugin
browser adds a plugin with more than 32 parameters with `none`. The
properties panel has a searchable picker, grouped by the plugin's modules. A
parameter with a wire or automation lane into it can't be hidden (they would
point at nothing). Changing the choice while the editor is open keeps the
editor's changes in the same undo step, and a parameter that is hidden forgets
its stored value. A hidden parameter is never sent to the plugin, so it keeps what
the plugin's saved state gave it. The editor shows a node's note inputs before
its audio and parameter inputs (the layout keeps them in their own list).

A parameter is `Offset`-modulated, with the plugin's range and default; a
stepped CLAP parameter is a stepped one (the node rounds the value before
sending). One instance has one voice: a polyphonic wire into a plugin is a
compile error, so put a Voice Mix before it. A mono wire into a stereo port
feeds both channels; any other channel mismatch is an error.

**macOS and Linux keep plugins loaded.** A plugin's Objective-C objects (an
editor's views) can still be queued for release on the main thread when its
instance is dropped, and releasing them after the plugin is unloaded or
deinitialised crashes. On Linux, plugins' GUI toolkits leave global state that
unloading and process exit both free, which aborted the real-plugin tests at
exit. So on both, each plugin file, CLAP or VST3, stays loaded for the life of
the app: the entry's `deinit` and the bundle's exit never run, and a plugin
file replaced on disk takes effect after a restart.

**Instances and the plugin thread.** CLAP says a plugin instance belongs to
its main thread, and JUCE-based plugins expect that to be one thread for the
whole process. So one shared plugin thread (`thread.rs`, started on first
use) owns every live instance. `instantiate` sends it a load command (30 s
limit) and gets back the audio processor, which the node holds; on drop the
node sends the processor back and the thread deactivates and destroys the
instance. The thread also runs what CLAP gives the main thread: plugin timers
(`timer-support`), `request_callback`, and editor creation and destruction.
By default that thread is one of its own, but an editor wants the application's
main thread on macOS (Cocoa) and a thread that pumps messages on Windows. So an
application whose UI thread should be the plugin thread calls
`adopt_current_thread(wake)` once at startup and `pump()` every frame:
requests from the UI thread then run on the spot, and requests from other
threads (a freeze render loading the plugin, say) wait in a queue for `pump`,
which `wake` asks the UI to run. `pump` also returns when to be called next so
plugin timers run (nothing is asked of an app with no plugin loaded). The app
does this on macOS and Windows only; Linux plugins keep their own thread, since
an X11 editor needs no event loop of ours. The cost is that a slow plugin
load or editor call stalls the UI thread there, as it already did when the
compile was waiting on it. One plugin that hangs while loading blocks the
others until M6's out-of-process hosting.

**Processing** (`node.rs`). Each block is copied into preallocated plugin
buffers (in chunks of at most `max_frames`), parameters whose value changed
are sent as timed `ParamValue` events (at frame 0, and every `PARAM_STEP` = 32
frames while a wire drives them, so audio-rate modulation is stepped), notes
are converted to CLAP note events (a note-off is given the key and channel its
note-on had, since Noodle's note-off carries only the id), and the plugin's
output is copied out. A plugin's note output becomes the node's event output.
The transport (tempo, position, signature, playing) is passed on. Nothing on
this path allocates (`tests/realtime.rs` checks it), so events beyond 4096 per
block are dropped. A plugin that fails in `process` gives silence for that
block. A parameter is sent once its port value differs from the plugin's
reported default: the first block leaves an untouched port alone, so state the
plugin restored isn't overwritten (Surge XT reports a default of 0 for every
parameter, and sending those silenced it).

**State.** The node's `state` config setting holds the plugin's saved state as
base64 text, so it is written to the project file with the rest of the graph.
It is loaded into the plugin before activation. Bad base64, or state the
plugin refuses, is a node error rather than a crash. CLAP state is wrapped on
saving in `NCS1`, a length and a checksum, and checked before it reaches the
plugin: some plugins crash the host on state that is cut short, and plugins run
in our process. (State saved before the wrapper is passed on unchecked.)
`capture_state` loads a
plugin (optionally with earlier state) and returns what it saves.

**Editors and capture** (`editor.rs`). Running plugin nodes register by node
ID. `open_editor(node)` opens the plugin's editor (CLAP `gui`). If the
application has set a window factory (`set_window_factory`; on macOS and
Windows `adopt_current_thread` installs one that makes an `NSWindow` or a
top-level `HWND`, `src/window/`) and the plugin supports the platform's API
non-floating, the editor is *embedded*: the host makes a hidden window of the
plugin's `get_size`, gives the plugin its handle with `set_parent`, and shows
both. The host follows the window: a size the plugin asks for
(`request_resize`) is applied from the plugin thread's housekeeping, a size the
user drags to goes through the plugin's `adjust_size` and `set_size`, and the
close button closes the editor (polled: `EditorWindow::is_closed`). With no
factory, or if embedding fails, the plugin's own floating window is used (the
platform's API). Edits in the window go to
the running plugin, so they are heard at once. `capture(node)` reads the live
plugin's saved state and every parameter's value; `Captured::commands` turns
that into one `Command::Batch`: `SetConfig state` plus a `SetParam` per
parameter, so the ports agree with the plugin (the ports win on the first
block). The app opens editors from the properties panel (`Edit::PluginEditor`)
and captures when the window closes (`Session::poll_plugin_editors`), as one
undo step. That is a config change, so the node recompiles and its plugin is
reloaded from the stored state. State changed while an editor is open is
stored only when it closes. The app follows an editor by its instance (`EditorKey`), not the node ID, so a freeze render of the same node can't take its place; if the node is rebuilt (rewired, config changed) while the editor is open, the editor's instance is gone and its unsaved edits with it. Editor commands wait up to 10 s on the plugin thread, and the app calls them synchronously, so a busy plugin thread freezes the UI that long.

### VST3 hosting

A VST3 plugin is the same kind of node as a CLAP one (`PluginNodeType`, the
same `Catalog`, scan cache, browser and caching), with ID `vst3.<class id>`
(32 hex digits; `Descriptor::format` says which standard it is, and scan
results saved before VST3 load as CLAP). The code is in
`noodle-plugins/src/vst3/`.

- **Files.** A `.vst3` is a bundle on every system (a folder holding the
  binary at `Contents/x86_64-linux/<name>.so`, `Contents/MacOS/<name>` or
  `Contents/x86_64-win/<name>.vst3`) or, on Windows, a plain DLL. The scan
  returns it as one file and doesn't search inside. Folders: `~/.vst3`,
  `/usr/lib/vst3`, `/usr/local/lib/vst3` on Linux;
  `/Library/Audio/Plug-Ins/VST3` (system and user) on macOS;
  `%COMMONPROGRAMFILES%\VST3` and `%LOCALAPPDATA%\Programs\Common\VST3` on
  Windows; plus `VST3_PATH`.
- **Loading** (`module.rs`). The binary is opened, its entry point is run
  (`ModuleEntry` with the `dlopen` handle on Linux, `bundleEntry` with the
  `CFBundle` on macOS, `InitDll` on Windows) and `GetPluginFactory` gives the
  factory. A module is shared by everything loaded from it (the entry runs
  once while any plugin is alive, `ModuleExit` after the last), and each plugin
  holds an `Arc<Module>` so the code outlives it. The classes that count are
  those in the category "Audio Module Class".
- **Plugin** (`plugin.rs`). The component, its `IAudioProcessor` and a
  controller: the component itself if it is also an `IEditController`,
  otherwise the class it names, initialised on its own. Both get an
  `IHostApplication` (the host's name, and `IMessage`/`IAttributeList` for
  plugins that make them) and the controller an `IComponentHandler`; a separate
  component and controller are wired through `IConnectionPoint`, so their
  messages reach each other (on the sender's thread; disconnected before
  teardown). Set-up follows the
  standard's order: bus arrangements (the plugin's own), every bus activated,
  `setupProcessing`, `setActive`, and `setProcessing` on the first block, on the
  audio thread. Tear-down is the reverse, with the component released last.
  The component is made, set up and dropped on the shared plugin thread (via
  `thread::run_job`, so a macOS plugin that wants the main thread gets it where
  the app has adopted its UI thread), processed on the audio thread.
- **Ports.** As for CLAP. Every audio bus is a port (`in`, `in2`, ...; the
  plugin's default channel count) and every event input bus is a note input
  (`notes`, `notes2`, ...). Parameters come from the controller: hidden,
  read-only and program-change ones are left out. A continuous parameter's port
  holds its normalized value, 0 to 1 (VST3 has no host-side plain range
  without asking the controller per value); a stepped one holds 0 to its
  step count and is rounded.
- **Processing** (`node.rs`). Per chunk of at most `max_frames`: the inputs
  are copied into preallocated buses, a parameter whose value changed becomes a
  point in its `IParamValueQueue` (at the chunk's start, then every
  `PARAM_STEP` frames while a wire drives it), notes become `Event`s
  (`kNoteOnEvent`, `kNoteOffEvent` with the key the note-on had, note
  expressions, polyphonic pressure) merged in time order across the note inputs,
  and a note output bus is read back. The queues and event lists are COM
  objects of a fixed size made when the node is built (`classes.rs`), so
  nothing allocates in a block (`tests/realtime.rs` checks it; points past a
  queue's room are dropped). A port at its default is not sent, so state the
  plugin was restored from isn't overwritten by an untouched port (the CLAP
  node does the same: a plugin can report a default that isn't its live value). Raw MIDI controllers (modulation wheel,
  volume, pan, expression, sustain, channel pressure, pitch bend) go through
  the controller's `IMidiMapping` to parameters, which need no port; a
  controller with no mapping is sent as a legacy MIDI event.
- **State.** `Plugin::save_state` writes `NVS2`, the component's state length
  as a little-endian `u32`, a checksum of everything after it, the component's
  state, then the controller's (only
  for a separate controller), through memory `IBStream`s. A bad checksum is
  refused before the plugin sees the bytes (`NVS1`, saved without one, is still
  read, unchecked). Loading hands the
  component its bytes, then the controller `setComponentState` with the same
  bytes and its own `setState`. It lives in the `state` setting like CLAP's.
- **Editors** (`gui.rs`). VST3 has no floating windows, so an editor is
  always an `IPlugView` embedded in a window the app makes (the window
  factory of `window/`; macOS and Windows have one built in, Linux has none
  yet, so there opening an editor says so). The `Gui` for each plugin lives
  in a table on the plugin thread under a key (a VST3 key has the top bit
  set, which is how `editor.rs` tells it from a CLAP instance's); open, close
  and capture are jobs on that thread, and the node holds only the key. The
  view gets an `IPlugFrame` whose `resizeView` only sets a flag, which the
  thread's housekeeping carries out (resize the window, then `onSize`), as
  it does the user dragging the window's edge (`checkSizeConstraint`, then
  `onSize`) and a closed window (a resize is thus carried out up to 50 ms after
  the plugin asks, not inside `resizeView`). Capturing a running VST3 first
  tells the controller what the processor was given through ports and MIDI
  (`setParamNormalized`, from a mirror of the last values sent, so a stale
  controller value never overwrites a port), then reads the component's and
  controller's state and each listed parameter's value (a stepped one as its
  step number) from the plugin thread while the audio thread
  runs, as the standard's UI-thread calls allow.
- **Edits.** A controller reports what the user does in the editor through
  `IComponentHandler::performEdit`; the host must pass it to the processor, or
  a plugin with a separate controller would change its display and not its
  sound. The handler stores the value in an `Edits` table (one atomic per
  parameter that isn't read-only, hidden ones included, made when the plugin
  is) and the audio thread reads it at the start of each chunk and queues a
  parameter change, without a lock or an allocation. The port's last-sent
  value is left alone, so an untouched port doesn't undo the edit; capturing
  the editor then stores the edit in the port. Not mirrored the other way:
  a wire modulating a parameter doesn't move the editor's display.
- **Transport.** `barPositionMusic` is the start of the current bar in
  quarter notes from the signature (6/8 is three quarters long), assuming the
  bars from the project's start are all the current signature's length.
- **Not yet.** A Linux window for editors, units and program lists, 64-bit
  audio, `IProcessContextRequirements`, `IRunLoop`, and note output other than
  note on and off. Latency is read once, when the plugin is created; a change
  the plugin announces later is not followed.

**Plugin browser** (`noodle-app/src/plugin_browser.rs`, View > Plugins). Lists
the catalog with a search over name, vendor, ID, format and kind; Add puts the node on
the canvas; Rescan runs the scan in the background (Shift scans every file
again) and the files that failed are listed with why.

**Caching.** Plugins are deterministic by default, so they can be frozen and
exported from the cache. The key covers the plugin's ID, its file's size and
modification time (not a content hash: hashing a plugin on every edit would be
slow) and the node's config (so its state). A plugin that uses randomness sets
the `nondeterministic` setting, and is never cached.

**Rendering.** Freeze, export and background renders build their own
registries, so `Catalog::extender()` is passed as
`RenderRequest::extend_registry`, and a render sees the same plugins as live
playback. The app scans in the background at startup (the cache is read first,
so plugins found last time are available at once) and compiles again when the
scan reports.

**Config exceptions.** The `state` and `nondeterministic` settings change
neither ports nor shapes, which the config-versus-parameter rule says makes
them parameters. They stay config because neither is a number a wire could
sensibly drive: state is an opaque blob and `nondeterministic` is a flag.

**Threads.** `instantiate` loads a plugin on its own thread (waiting up to 30
s, then failing the node), and `start_processing` runs on the first block, on
the audio thread. 

**Limits.** The transport's bar fields assume quarter-note beats and a
meter that has not changed. A plugin that crashes while playing takes the process with it
(until out-of-process hosting in M6); editors are separate windows (not drawn inside the app's window), Linux has no
embedding and no `posix-fd`, and the macOS and Windows editor windows are
checked only by CI compiling them and by one hand test on the Mac; CLAP extensions beyond audio
ports, note ports, parameters and state (e.g. tail, voice info, remote
controls) are not used.

## Performance (M6)

### Delay compensation

A node reports how late its outputs sound with `Node::latency` (samples; a
plugin's reported latency, or a lookahead's). It is read once, when the plan is
built, right after the node is created, and clamped to `MAX_LATENCY` (two
seconds at 96 kHz). A node that carries over from the previous plan keeps the
latency recorded in `PlanInfo`.

`plan::compensate` runs after the plan's nodes are made. Walking the schedule,
it gives every signal buffer an *arrival*: the slowest of its writer's inputs
plus the writer's own latency. Where paths meet, the early ones get a
`DelayLine` so everything lines up with the slowest:

- a node with several signal inputs lines them up with each other;
- every *sink* (an Output node that plays, or a tap) lines up with the
  slowest sink, so two Output nodes on different devices stay in sync;
- the lines read their source at the consumer's step and write a private region
  appended to the plan's pool, so the compiler's buffer reuse is not touched,
  and cost nothing when no node reports latency;
- a feedback loop's own input is never compensated (the loop already carries
  its delay), and event ports are not compensated at all (a plugin's latency is
  in its audio, and notes arrive on time);
- a plan swap hands each line's contents to its successor (keyed by node,
  input and delay), and a transport jump clears them with the nodes.

A plan swap is seamless when its delay lines match by node, input and delay,
whatever their slots, so unrelated edits don't fade the output. The mix as a
whole is then `output_latency` samples late (`Controller::output_latency`).
Live playback just sounds that much late, as in any DAW, and recording
subtracts it along with the device's output latency (`Session::stop_recording`, MIDI takes); offline renders (`render`, `render_project_streaming*`,
`render_taps`, so exports and freezes) render that many extra frames and drop
them from the front, so what they write lines up with the timeline.
`noodle-nodes/tests/pdc.rs` covers the alignment and the trim,
`realtime.rs` that it never allocates through plan swaps and seeks, and
`noodle-plugins/tests/latency.rs` that CLAP and VST3 plugins report theirs
(the test plugins read `NOODLE_TEST_PLUGIN_LATENCY`).

### CPU meters

When a `Telemetry` hub is set, each node's step is timed
(`Instant::now` around its `process`, no allocation) and reported through
`Telemetry::open_cpu`/`read_cpu`/`read_all_cpu` as a share of the block's
time (`CpuReading`: a smoothed `load` and a slowly falling `peak`; 1.0 is the
whole block). Without a hub nothing is timed, so offline renders pay nothing.
The editor's View > CPU meters shows each node's load in its header, coloured
amber from 15 % and red from 50 % (or a peak over 90 %), and the graph's total
in the corner. A group has no step of its own (it is flattened away), so its
header shows the sum of the nodes inside it at any depth. A split node (feedback loop) reports its output half; the input
half's time is not counted. Time spent in delay lines is not attributed to a
node.

## UI (`noodle-app`)

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
    mixer's `inputs` or adds a boundary node inside the group, named `in1`,
    `in2`, … or `out1`, … Config beyond the last wired mixer input is
    hidden, not removed, and a group port is never removed on its own,
    because the boundary node may be used inside. Every edit made on the
    user's behalf goes through `spare::wire`, which replaces whatever fed the
    input, never removes a node, and leaves wires leaving an output alone.
  - **Names.** F2 on a selected group, group input or group output renames it,
    and double-clicking a port row of a group node renames that port (a
    double-click elsewhere on a group enters it). A group's name is its
    `name` config, which a track shares, and the track input inside shows its
    group's name as its title. A port's name is its key, so `rename_group_port`
    moves the wires on it in the same step, and refuses an empty name or one
    another port on that side has. An empty group name puts the default back.
  - **Idle outputs.** A track input's `audio` output is greyed while no clip
    plays on the track, and `midi` is greyed until there are MIDI clips.
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
  - **Ticks on the x axis**, so the view doesn't stretch when the tempo
    changes. Zoom is points per quarter note.
  - **Moving and trimming** are `SetClip` commands, one undo step per drag.
    Each frame's edit is worked out from the clips as they were when the
    drag began, so snapping can't accumulate error. Moves and the grabbed
    edge snap to beats; Alt turns that off. Trimming turns the dragged tick
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
    target the selected clip's track, or the first, at the playhead; the
    right-click menu on a lane targets the lane and tick clicked), creates one clip per file, laid end
    to end from the drop position (snapped to beats; Alt turns that off).
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
    resets), written as `SetParam` commands, so a slider drag is one undo
    step. A track input node outside any group has no controls. Solo counts
    if either boundary node has it, so turning it off clears every one, and
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

### Parameter widgets

`widgets::ParamField` draws any `ParamInfo`, in the properties panel and
(with `compact`) on node bodies, so both behave the same:

- **Sliders** for continuous and unlabelled stepped parameters, in the style
  of Blender's number fields. The fill follows the taper, so a log
  frequency field puts 200 Hz a third of the way along 20 Hz to 20 kHz.
  Drag to change (Shift for fine control), click to type, Backspace while
  hovering or the right-click menu to reset. With keyboard focus, the
  arrow keys nudge the value and Enter starts typing; AccessKit's increment
  and decrement actions work too.
- **Drop-downs** for stepped parameters with labels.
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

### Node graphs

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

### Mixer

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

### Track order

`Project.track_order` is a list of group IDs, set by `Command::SetTrackOrder`
(one undo step; it has no engine effect). The arrangement and the mixer's
strip names read it through `Project::sort_tracks` (Add Track always
puts a new track last):
groups it names come first in that order, the rest follow by ID, and IDs
that no longer exist are ignored, so deleting then undoing a track keeps its
place. Dragging a track's header on the arrangement writes it, keeping only the
groups the arrangement shows. Saving drops IDs of groups that no longer exist,
so a reused ID can't inherit a deleted track's place.

### Outputs view

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

### Scope view

View > Scope, or double-clicking a Scope or Output node, opens a pane with a
drop-down over the Scope and Output nodes and a larger drawing of the chosen
one. It keeps no
state of its own: it reads the capture the editor already reads from the
telemetry hub (`EditorState::scope_view`).

## Testing

- **Golden renders:** render test graphs offline and compare against stored
  WAV files within a tolerance.
- **Compiler tests:** cycle detection, shape inference and buffer reuse, plus
  property tests that generate random graphs and check that the plans are
  valid.
- **Real-time safety:** the audio-thread allocation check runs in all
  debug builds and tests.
- **Plan swapping:** tests swap plans mid-render and check that the output
  is continuous, with no click at the swap: identical for an edit off the
  audible path, and no jump steeper than the signal's own for one on it.
- **App tests** drive the egui app with `egui_kittest`, sending real pointer
  and key events. `noodle-app/src/acceptance.rs` follows a milestone's
  "done when" end to end: it builds a patch in the node editor, plays it
  on ALSA's `null` device and tweaks it, saves, and reopens it unchanged.
- **CI** runs fmt, clippy and the tests on macOS, Windows and Linux.

## Open questions

- The node API is drafted in `crates/noodle-engine/src/node.rs` and `lane.rs`,
  with example nodes in `noodle-nodes`. Its planned follow-ups are in the
  roadmap:
  - **M3:** silence skipping, and skipping finished voices. A parameter
    that offsets along its travel (`OffsetInput` in `plan.rs`) converts a
    lane that holds one value (a gate, a velocity, a sustaining envelope) once
    rather than per sample, and flags the result constant when every lane
    agrees; a moving lane still costs a conversion per sample.
  - **M4:** streaming offline renders (done for whole-project renders; the
    offline *node* API still hands nodes the whole range, in memory, so a very
    long project can't go through an offline node).
- The project file format. RON or JSON for readable diffs, with audio stored
  alongside.
