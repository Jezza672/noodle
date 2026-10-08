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
  noodle-nodes    Built-in nodes (DSP), registered with the engine's registry.
  noodle-io       Device I/O (cpal, midir), file decoding, disk streaming.
                 Calls into the engine from device callbacks.
  noodle-app      The egui application.
  noodle-plugins  (M5) CLAP and VST3 hosting, exposed to the graph as nodes.
```

Dependencies only point downwards: `app → io/nodes → engine → core`. The engine
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
    its default) or it is muted, and drops the rest. So a group nobody has
    touched costs nothing and renders bit-for-bit like the flat patch, and a
    touched one costs a stage that is exact at unity. The stage stays once
    set because adding or removing a node in the audible path makes the
    engine fade the whole output out and in (5 ms each way): the first touch
    of a control on a group costs that one fade, and after that gain, mute and
    solo are parameter changes. Mute is a smoothed 0 to 1 parameter, so it
    ramps rather than clicks. A control reset should write the default, not
    remove the parameter, or the stage goes and the fade comes back. Boundary
    nodes have no parameter ports in the editor yet, so wiring into them
    waits for that.
  - **`create_track`** (`noodle_core::group`) makes a track as one undo step:
    the group, a `noodle.track.input` node (outputs `audio` and `midi`), a
    group output `out` with `audio` wired to it, and a group input `in`. The
    output starts with gain 0 dB and mute 0 already set, so the track's stage
    exists from creation and the first fader or mute touch is a parameter
    change, not a graph change with a fade. The arrangement view calls it.
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
    clips out of `midi`. (The `midi` output is an events signal, which arrives
    with M3; until then it exists and stays empty.)
  - **Creating a track** creates the group, its track input node and the
    group's output node in one step, with the input's `audio` output wired
    into the group's output by default. Undo removes all of it.
  - **Group input and output nodes carry the track's controls** as
    parameters: gain, mute, solo and the like. The track's gain, mute and
    solo buttons in the arrangement view and the mixer show and set those
    parameters. Gain and mute are ordinary runtime parameters, so they can be
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
  The mixer shows each track group's output gain, pan and send nodes, and the
  timeline shows the clips that feed each track.

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
- **Spectral:** may be added later, for FFT-frame processing.

Most nodes treat each (voice, channel) **lane** independently, so their
authors write a per-lane kernel with per-lane state, and the framework loops
over the lanes and handles broadcasting. Nodes that work across lanes
(panning, Voice Mix, voice allocation) implement the full `Node` trait and
see whole signals.

Every input, including an unconnected parameter, arrives as a signal buffer.
An unconnected parameter that isn't mid-change is flagged as **constant** for
the block, so nodes can move work such as computing filter coefficients out of
the per-sample loop.

## Compilation and the render plan (`noodle-engine`)

```
 UI thread (Controller)                 audio thread (Processor)
 ──────────────────────                 ────────────────────────
 Command ─▶ Project ─▶ compile ─▶ build ─▶ [plan queue] ─▶ install: take over
                                                            instances, run
 maintain(): free old plans ◀──────────── [return queue] ◀── replaced plan

 set_param ─▶ shared atomic cell ─────────────────────────▶ read each block,
                                                            smoothed

 meters/scopes ◀── telemetry (M1) ◀──────────────────────── written by nodes
```

**Compiling** (`compile`) turns the project graph into a **schedule**:

1. **Resolve nodes** to their types and layouts, and **resolve wires** to port
   indices.
2. **Drop loops.** A wire that closes a loop is ignored, with a diagnostic on
   it. Loops through a Delay node come in M3.
3. **Sort topologically** into an execution order, breaking ties by node ID.
4. **Infer shapes.** Propagate `(voices, channels)` through the graph using the
   broadcasting rules. A mismatch, such as 8 voices meeting 4, is reported on
   the wire that broke it.
5. **Allocate buffers** by liveness, the way registers are allocated. A buffer
   is reused once its last reader has run, and a node's outputs never share a
   buffer with its inputs.

Before step 1, **group nodes are flattened** (`flatten.rs`): groups and their
boundary nodes are dropped and each wire through them is joined end to end, so
a group costs nothing at run time. Nodes keep their IDs, so diagnostics still
point at the right node. Cacheability is analysed in M4.

**Problems don't stop compilation.** A node that can't run is left out, and
anything wired to it behaves as if unconnected. A wire that can't work is
ignored. Each problem becomes a diagnostic on the node or wire where it
happened, for the UI to show.

**Building a plan** (`plan::build`) happens off the audio thread:

- **Memory:** it allocates everything the plan will need: the buffer pool,
  event buffers, and scratch space for each node's views.
- **Nodes:** a node whose type, config and shapes are unchanged since the
  last plan is marked to **carry over**: *take the instance from old slot i
  and put it in new slot j*. Every other node is instantiated fresh. A node
  that fails to instantiate is kept as silence, with a diagnostic.

**On the audio thread**, installing a plan moves the carried-over instances
out of the old plan into the new one. That's O(nodes) pointer moves with no
allocation, and it keeps filter state, oscillator phase and plugin instances
intact. The old plan, and any instances that were removed, go back through
the return queue. The controller frees them in `maintain()`, which the UI
calls every frame. The processor installs a plan only when the return queue
has room, so nothing is ever freed on the audio thread.

**Swaps that change what's audible fade.** Carrying instances over keeps an
edit elsewhere in the graph seamless, but removing, rewiring or rebuilding a
node on the output's path would still switch the sound in one sample and
click. So `plan::build` records the wiring of every node the Output nodes
depend on, and marks the plan *seamless* if that wiring is unchanged and all
of those nodes carry over. A seamless plan goes in at once. For any other,
the processor fades the output out over 5 ms with the old plan, installs
every queued plan at silence, and fades back in over 5 ms.
- The cost is a 10 ms dip on structural edits to the audible path. Parameter
  changes, and edits elsewhere in the graph, don't dip.
- A true crossfade would avoid the dip, but carried-over instances can't run
  in both plans at once, so it would need the old plan run without them.
- The first plan goes in without a fade, so offline renders are unchanged.

**Parameter changes don't recompile.**
- Every unconnected input has a shared atomic cell. `set_param` writes the
  cell, and the audio thread reads it at the start of each block and smooths
  the change, so nodes just see a ramp.
- The project is the source of truth: every `update` writes the project's
  values back into the cells, so undoing a parameter change reaches the
  audio.
- A ramp that's in progress carries on smoothly across a plan swap.

**Output nodes** are mixed into the device buffer as they're reached in the
schedule. Mixing at the end of the block would be too late, since their input
buffers may already have been reused. A mono signal goes to every channel,
and voices are summed.

**Input nodes** work the other way round: the executor writes the block's
device input into an Input node's output when the node's step comes. Its
channel count is config (default 2): channel n is device channel n, a mono
device feeds every channel, and channels the device lacks are silent.
`Processor::process_with_input` takes the interleaved input; plain
`process`, as offline rendering uses, leaves Input nodes silent. Input
nodes are nondeterministic, so they're never cached.

**On a device**, `noodle-io`'s `DeviceWriter` runs the processor inside the
cpal callback, through a preallocated block buffer, and converts to the
device's sample format. It clamps to between -1 and 1 and turns non-finite
samples into silence, to protect ears and speakers. Offline renders aren't
clamped, so files keep exactly what the graph produced.

**Choosing a device.** `play` takes an `AudioConfig`: a host (audio API),
an output device, a sample rate and a buffer size, each defaulting to the
system's choice. Hosts and devices are stored by cpal's stable IDs, so a
saved choice survives restarts and renamed devices. A device ID names its
own host, so the host setting only picks where default devices come from.
`hosts()` and `devices()` list what the picker offers, and the rate and
buffer size are checked against what the device supports before a stream
opens. Changing any of these means a new engine, since `Settings` are
fixed for an engine's lifetime. In the app, the audio settings dialog
(`noodle-app/src/devices.rs`) edits a copy of the session's `AudioConfig`
and hands it back on Apply; if audio is playing, it restarts on the new
devices. It opens from File → Audio Settings… (Cmd/Ctrl+,). It lists
devices when it opens, when the host changes and on Refresh, never per
frame, because probing devices is slow.
It offers only sample rates both the output and the chosen input support,
and when the devices change it drops a rate or buffer size they don't
support, so Apply can't hand back settings that fail to open.
Applied settings are saved to `prefs.ron` in the user's config directory
(`noodle-app/src/prefs.rs`) and loaded at startup, so they survive
restarts. Missing fields take their defaults, so the file stays readable as
settings are added. A broken file is reported and moved aside to
`prefs.ron.bad` rather than stopping the app or being overwritten. If the
saved output can't play (unplugged, or a rate it no longer takes), the app
plays on the system's default output instead and says so, keeping the saved
choice for when the device is back.

**Device input** is off unless `AudioConfig::input` picks a device, since
opening a microphone can prompt for permission. It runs at the output's
sample rate, since there's no resampling yet. An input device that can't (or
can't be opened at all) doesn't stop playback: Input nodes stay silent and
`Playback::input_problem` says why. The input stream picks its own buffer
size and starts before the output, and the feed doesn't count a shortfall as
a glitch until input has first arrived. The streams also fail separately:
`Health::errors` says which stream each error came from, and a fatal error
on the input (the device unplugged, say) leaves Input nodes silent and the
status bar saying "No input", while only a fatal output error stops
playback. If the output is rerouted (headphones unplugged, say), cpal
reports `DeviceChanged` and some backends leave the stream silent, so the
app starts playback again on the new default and says so, unless it has
already done that three times in ten seconds, when it stops instead. cpal
runs input and output as separate streams, so input crosses between their
callbacks through an SPSC ring (`noodle-io/src/input.rs`): `Capture` fills
it, and the `DeviceWriter`'s `Feed` takes one engine block at a time. Unless
both are the same device, their clocks drift apart. A slow input runs dry,
and the gap is silence. A fast input builds a backlog, so the feed watches
the smallest backlog over each half second and drops whatever was beyond a
small margin. Both count as input glitches in `Health`, a gap once however
many engine blocks it spans.

**Recording** takes a second copy of the input. `RecordTap`, inside the
input callback (`Capture`), queues samples into its own ring while recording
is on, and a writer thread (`noodle-io/src/record.rs`) writes them to a
32-bit float WAV, so the callback never touches the disk and recording
doesn't depend on the output. `Playback::start_recording` and
`stop_recording` switch it with a flag: no stream is rebuilt and nothing is
allocated on the audio thread. The file's header is refreshed about once a
second, so a crash loses little, and a `Take` reports the frames written and
any input dropped because the disk stalled. The file is at the input's
channel count and the engine's rate, and starts at the moment of the call:
aligning it with the timeline, and with the input's latency, is the
transport's job.

**Data going back to the UI** goes through a `Telemetry` hub
(`noodle-engine/src/telemetry.rs`), which the UI reads every frame by node ID.

- **Opening a channel:** a reporting node type, such as Meter or Scope, holds
  a handle to the hub. When it instantiates a node, off the audio thread, it
  opens a channel under the node's ID (`Setup::node`), keeps the writing end
  in the instance, and the hub keeps the reading end. Carried-over instances
  keep their channels.
- **Following the playing instance:** instances are made when a plan is
  built, before it reaches the audio thread, and a plan can be dropped
  unsent, so a node can have several channels open. The hub reads the newest
  one that has been written to (falling back to the newest), which is
  always the instance that's playing.
- **Meters** are atomics per channel. The audio thread raises the peak with
  `fetch_max` on the float's bits (integer order is float order for
  non-negative floats) and stores the smoothed RMS. Views read through
  their own `MeterReader`. A read takes the peak, resetting it, and folds it
  into a held peak for every reader, so each view (the editor every frame, a
  mixer now and then) sees the highest peak since its own last read.
- **Scopes** are `rtrb` SPSC ring buffers of interleaved frames, holding about
  a second. When the ring is full, new frames are dropped whole, so channels
  stay aligned. The hub keeps the most recent second it has read in a
  fixed-size ring, and the UI copies it into a `ScopeView` it reuses.
- **Locking:** only the hub's map of channels has a lock, and only the UI and
  plan building take it, never while running caller code. The writing ends
  never lock or allocate, which `realtime.rs` checks.
- **Closing:** every use of the hub closes channels whose writing end has
  been dropped. That happens when the controller frees a deleted node's
  instance, so the hub needs no hook into graph edits.
- **One hub per engine:** two engines instantiating the same graph from one
  hub (live playback and an offline export, say) would both look like they
  were playing, so an export should use a registry with its own hub.
- **Drawing:** the node editor reads every meter and scope once a frame
  (`noodle-app/src/editor/body.rs`) and draws them in their nodes' bodies,
  repainting continuously while audio plays. Scopes trigger on a rising zero
  crossing so steady waveforms hold still.
- `noodle_nodes::register_all` creates the hub and returns it. Playhead
  position and cache-render progress will use the same hub.

Blocks have a fixed maximum size, and a longer device buffer is rendered as
several blocks. Events inside a block are sample-accurate.

## Threads

- **Audio:** driven by the device callback in `noodle-io`, or by the offline
  renderer. Runs the current plan.
- **UI:** owns the Project and the undo stack, compiles and builds plans, and
  frees old ones. Compiling can move to a worker if it gets slow for large
  graphs.
- **Workers:** disk streaming, cache renders, plugin scanning.
  - **Disk streaming** (`noodle-io/src/stream.rs`): one worker thread per
    playing clip decodes the file, resamples it to the engine's rate and fills
    fixed-size chunks. Full chunks reach the audio thread, and spent ones go
    back, through two lock-free queues, so the audio thread never allocates,
    locks or waits (`tests/stream_realtime.rs` enforces it). A stream counts
    its position in engine frames from the start of the clip, and maps it to
    the file with the clip's `offset` and `length` in file frames. A seek is a
    request the worker acts on; until it catches up, reads come back short
    and count an underrun, and the caller plays silence.
- **Parallel execution (M6):** the plan's dependency graph is scheduled across
  a pool of real-time worker threads. The plan format records dependencies
  from the start so this can be added without redesigning it.

## Time and transport (M2)

The transport gives every block a timeline position in samples, plus the
musical position (bars, beats, tempo) from the tempo map. Track input nodes
and tempo-synced nodes read it. A graph with no transport, such as a live
patch, runs on free-running time.

### Representing time

Settled for M2 (Phase 0):

- **The document counts in ticks.** A `Tick` is an integer, 960 to a quarter
  note. Clip starts, loop points and automation points are ticks, so a
  project edited in bars and beats stays put when the tempo changes.
- **The engine counts in samples.** The transport position is a sample
  count (`u64`) at the engine's rate, and `Processor::process` never sees a
  tick. Where a node wants the musical position it reads it from the block's
  timeline info, which carries the tick (as `f64`, with the fraction), the
  tempo and the time signature at the start of the block.
- **The tempo map converts between them.** It is a list of tempo changes
  (a tick and a BPM, with steps, not ramps) and time signature changes (a
  bar and a numerator over a denominator). It lives in the project, because
  it is edited and undone like anything else. The engine gets a compiled copy:
  a table of segments with the sample position each starts at, searched
  without allocating, and swapped like a plan when the map changes.
- **Audio doesn't stretch with the tempo.** An audio clip stores its start in
  ticks but its source offset and length in the file's own samples, so a
  tempo change moves it without changing how it sounds. (Warping to the
  tempo is a later feature and would be a property of the clip.)
- **Tempo is keyed to ticks, signatures to bars.** Editing an early time
  signature moves the bar lines but not the tempo changes after it, which
  stay at their place in the music. That is what a signature edit means: the
  bars change, the music underneath doesn't. Later signature changes follow
  their bar number, so they move in ticks along with the bar lines.
- **Nothing starts before tick 0.** Clip starts and lane points can't be
  negative, so there is no pre-roll in M2.
- **The engine never calls the tempo map per sample.** Its lookups walk the
  list of changes, which is fine for the UI. The engine uses the compiled
  table (a binary search per block) and steps through a segment by adding.
- **Rounding.** Tick to sample rounds to the nearest sample. Sample to tick
  is only needed for display and for nodes reading the musical position, so
  it is a float.

### The engine's transport

- `Controller::transport()` gives a `TransportControl`: play, stop, seek (to
  a tick), loop (between two ticks) and the playhead in samples. They are
  shared atomics, so a UI thread sets them and the audio thread reads them
  without a lock. A new transport is playing from sample 0, so a live patch
  runs on free-running time as before. Stopping holds the position and tells
  nodes `playing` is false, and the graph keeps rendering.
- `Controller::set_tempo_map` builds a `TempoTable` (the compiled map) and
  sends it over a queue, like a plan. The audio thread gets the tick from
  the sample position by binary search, and nodes read `tick`, `bpm` and
  `signature` from `Context::transport`.
- Seeks wait for the output to fade out (the same 5 ms fade a non-seamless
  plan uses), jump while it is silent, reset every node, and fade back in.
  A new tempo table goes in at once, keeps the playhead's tick, and resets
  nothing, so dragging the tempo doesn't chop the sound or wipe reverb tails.
  Only something that reads the sample position can notice the move, which
  means a clip schedule (below). Old tables are sent back to the controller
  to be freed.
- `stop()` doesn't fade: the graph keeps rendering. A track input playing a
  clip must end it or fade when `playing` goes false, or stopping mid-clip
  clicks.
- A block ends exactly at the loop end, and the playhead wraps there.

### Playing along the timeline

- **Blocks.** The transport splits a block at the loop end, so a block never
  crosses the wrap. It does not split at tempo changes: the block's timeline
  info carries the tick and tempo at its start, so a tempo change reaches
  tempo-synced nodes at the next block. That lag is bounded by `max_frames`
  (512 in the app, about 11 ms at 48 kHz), which is fine for LFOs and delays.
  Track inputs work in samples and aren't affected.
- **Editing the tempo map or a clip while playing.** The playhead keeps its
  tick, since that is what the user sees. Three things make that work:
  - **The new sample position is computed on the audio thread**, when the
    plan is installed, from the playhead's tick under the old map and the
    new map's table. The playhead moves while the UI thread builds the plan,
    so working it out on the UI thread would be racy.
  - **A change to what is heard at the playhead dips the track.** The track
    input does this itself, in its own 5 ms fade, so other tracks play on
    untouched and the engine needs no discontinuity flag. When a new schedule
    arrives, the node compares it with the old one over the coming block. If
    they sound the same (a clip edited far from the playhead, a tempo edit
    after it), it swaps at once. If not, it fades the old schedule out, swaps
    at silence, and fades the new one in. Steps of one drag arriving while it
    is down replace each other, so they don't each trigger a fresh dip. The
    node also fades in when the transport starts or jumps, and out when it
    stops.
  - **A schedule reaches a carried-over track input without rebuilding it.**
    Instances are carried over by their `NodeKey` (type, config, shapes), and
    the schedule is not part of it: rebuilding would throw away the decoder
    and streaming state mid-clip. `ClipFeeds::update` (in `noodle-nodes`)
    turns the project's clips and tempo map into a schedule per track input,
    in samples, and a hub thread per node instance passes it to the node
    through a lock-free queue, along with the audio streams the node will
    need. The node reads both at the start of a block and hands what it is
    done with back to the hub to be freed. The hub opens a stream for the
    clip at the playhead and the ones starting within the next second, so no
    file is opened on the audio thread. After a seek, the first few
    milliseconds of a clip may be silent while its stream positions itself.

### Clips

Clips are part of the project, not of the graph, like frames. A clip says
which track input node plays it (`node`), where it starts, and what it
contains. A clip's content is audio (which part of which file) or, from M3,
MIDI, and one track holds both kinds. M2 builds the audio side; MIDI clips
themselves arrive with M3, and the data model leaves room for them. The track itself is a group node, so
the arrangement view reads the track input node's clips.
Compiling turns the clips into the schedule the node follows, sorted by
start; the node only reads that. Clip commands are undoable like any other.

- **Overlaps.** A clip's length is in samples and its start in ticks, so
  slowing the tempo can make audio clips on one track overlap. A track plays
  one audio clip at a time: the one that started last (the higher ID on a
  tie), and the earlier clip is cut where the later one begins. This holds per
  kind: an audio clip and a MIDI clip on the same track play together. There is no automatic
  crossfade; a clip's own fades apply. The arrangement view doesn't stop you
  placing clips on top of each other: overlaps are legal, and the later start
  wins.
- **Nodes that go.** Removing a node removes the clips it plays (a track input node) and the lanes
  driving its inputs, in the same undo step. Node IDs don't change when a
  node moves in or out of a group, so those clips and lanes stay valid. A lane
  whose port no longer exists (a config change removed it) is not a load
  error but a diagnostic when compiling.

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
  the node as they are. The internal automation node therefore ramps the
  jump at each hold step over the same smoothing length the target would have
  used, so a stepped lane doesn't click.
- **The parameter widget.** For a parameter with a lane, the widget shows the
  lane's value at the playhead and is greyed like a wired parameter. The lane
  is edited as a lane.
- The lane evaluates per sample for linear segments and flags the signal
  constant for hold segments, so a stepped lane costs nothing.
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
depends on live input or a device. The compiler marks this per node. An
offline node with a non-cacheable input is a compile error.

**Cache keys.** Keys are computed Merkle-style. The hash includes the
timeline: the tempo map, the clips a track input plays, and the points of any lane
driving the node, so freezes go stale when they change. An audio source in the key is its
content hash, like any source file, so replacing a file under the same name
invalidates it.

```
key(node) = hash(type_id, type_version, params, sample_rate, range, key(inputs)…)
```

Source files are keyed by their content hash. Renders are stored on disk by
key. An edit produces a new key, so stale data can never be served, and
undoing an edit brings the old render back straight away. The node's
`type_version` gets bumped whenever its DSP changes, which invalidates old
caches.

**Freeze.** Freezing a node or group marks it frozen. A render starts in the
background and the node shows a progress bar, as in Blender. Until the render
finishes, the subgraph plays live, or plays silence if it contains an offline
node. Once it's ready, the next compiled plan replaces the subgraph with a
cached-audio player.

**Export** is the offline renderer run on the whole project over a chosen
range, writing to a file.

Plugins are treated as deterministic by default. Each plugin node can opt
out, for plugins that use randomness.

## Plugins (M5)

A plugin is a node. Its parameters become input ports, and its audio and note
ports become graph ports. Plugin state is saved in the Project.

- **CLAP** is hosted through `clack`.
- **VST3** is hosted through the Rust `vst3` bindings, or through a thin C++
  shim over Steinberg's SDK (MIT-licensed since 3.8) exposed through a C ABI.
- **AU** may come later. Most macOS plugins also ship as VST3 or CLAP.
- Plugin editors open as separate native windows.
- Scanning runs in a child process, so a plugin that crashes during scanning
  can't take down the app.
- **Out-of-process hosting (M6)** runs plugins in sandbox processes that
  exchange audio through shared memory, so a crashing plugin can't take down
  the session.
- **Latency:** plugins report their latency, and plugin delay compensation is
  added to the plan in M6.

## UI (`noodle-app`)

The UI is egui, rendered on the GPU, aiming for Blender's dense, keyboard-driven
style. It has these views:

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
  - A clip's file is read once for its sample rate and length, and
    remembered as missing if it can't be, so a broken path costs one read.
  - Automation lanes and the mixer are still to come.
- **Mixer:** a view over the track groups.
- **Properties panel:** the selected node's config settings, parameters
  and compile problems. A parameter with a wire into it is greyed out,
  since the wire replaces its value.

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
- **Config fields** (`ConfigField`) only commit when a drag or typing
  finishes, because changing config recompiles the node.

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
  - **M3:** the port derive macro, silence skipping, and skipping finished
    voices.
  - **M4:** streaming offline renders.
- The project file format. RON or JSON for readable diffs, with audio stored
  alongside.
