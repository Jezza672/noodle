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
- **Group nodes** contain a subgraph and expose ports through it. A **track**
  is a group node with a clip source feeding its subgraph. Buses and sends are
  just wires.
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
 UI thread                         audio thread                  GC thread
 ─────────                         ────────────                  ─────────
 Command ─▶ Project ─▶ compile ─▶ [plan queue] ─▶ swap plans ─▶ [return queue] ─▶ drop old plan
                          │                         ▲
 param edit ──────────────┼──▶ [param queue] ───────┘ (applied at block start, smoothed)
                          │
 meters/scopes ◀──────────┴──── [telemetry queues] ◀── written by nodes
```

Compiling turns a Project into a **RenderPlan** in these steps:

1. **Flatten** group nodes into one graph, keeping a map back to the original
   node IDs for error messages and telemetry.
2. **Check for cycles.** A cycle is only allowed if it passes through an
   explicit delay node (minimum one block). Any other cycle is a compile error,
   shown on the offending wire.
3. **Infer shapes.** Propagate `(voices, channels)` through the graph using the
   broadcasting rules. A mismatch, such as 8 voices meeting 4, is a compile
   error shown on the wire.
4. **Sort topologically** into an execution order.
5. **Allocate buffers**, reusing them based on liveness, the way registers
   are allocated. Every buffer is allocated here at its maximum size, before the
   plan reaches the audio thread.
6. **Analyse cacheability** (see Caching).
7. **Instantiate nodes.** Nodes that are new in this plan get fresh DSP
   instances, built off the audio thread. For nodes that already exist, the
   plan records a migration entry: *take the instance from old slot i and put
   it in new slot j*.

On the audio thread, swapping plans means moving the surviving instances out
of the old plan into the new one. That's O(nodes) pointer moves with no
allocation, and it keeps filter state, oscillator phase and plugin
instances intact. The old plan, and any instances that were removed, go back
through the return queue to be dropped on the GC thread.

**Parameter changes don't recompile.** They go through the param queue, are
applied at the start of a block, and are smoothed by the node. The Project
records the new value for saving and undo.

**Data going back to the UI** (meter levels, scope buffers, playhead
position, cache-render progress) goes through SPSC ring buffers or atomics,
which the UI reads every frame.

Blocks have a fixed maximum size. Hosts may ask for any block size up to
that, and events inside a block are sample-accurate.

## Threads

- **Audio:** driven by the device callback in `noodle-io`, or by the offline
  renderer. Runs the current plan.
- **UI:** owns the Project and the undo stack, and compiles plans. Compiling
  can move to a worker if it gets slow for large graphs.
- **Workers:** disk streaming, cache renders, plugin scanning, dropping old
  plans.
- **Parallel execution (M6):** the plan's dependency graph is scheduled across
  a pool of real-time worker threads. The plan format records dependencies
  from the start so this can be added without redesigning it.

## Time and transport (M2)

The transport gives every block a timeline position in samples, plus the
musical position (bars, beats, tempo) from the tempo map. Clip player nodes
and tempo-synced nodes read it. A graph with no transport, such as a live
patch, runs on free-running time.

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

**Cache keys.** Keys are computed Merkle-style:

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

- **Node editor:** pan and zoom, Shift+A to search for and add a node,
  box select, drag to connect, Ctrl+right-drag to cut wires, reroute
  points, frames, and Tab to enter and leave a group.
- **Timeline:** tracks, clips, automation lanes.
- **Mixer:** a view over the track groups.
- **Properties panel:** the selected node's parameters.

The UI only changes the Project by issuing commands, and only reads engine state
through the telemetry API.

## Testing

- **Golden renders:** render test graphs offline and compare against stored
  WAV files within a tolerance.
- **Compiler tests:** cycle detection, shape inference and buffer reuse, plus
  property tests that generate random graphs and check that the plans are
  valid.
- **Real-time safety:** the audio-thread allocation check runs in all
  debug builds and tests.
- **Plan swapping:** a test swaps plans mid-render and checks that the output
  is continuous, with no click at the swap.
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
- The automation model: automation lanes as clip-like sources feeding
  parameter ports, or as a separate mechanism.
- Representing time: sample positions plus a tempo map, or musical ticks as
  the main unit.
- The node editor: build on `egui-snarl`, or write a custom canvas for
  Blender-specific interactions.
