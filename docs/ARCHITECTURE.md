# Noodle architecture

Noodle is a node-based DAW. You build sounds and mixes by wiring nodes
together in an editor modelled on Blender's compositor, and you arrange them in
time on a conventional timeline. This document describes how the system is put
together and the rules each part has to follow. What is left to build is in
[ROADMAP.md](ROADMAP.md); what is already built is in [features/](features/README.md).
The detail for each part lives in [architecture/](architecture/); the index is
below, so read only the file you need.

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
  noodle-stems    (M10) Stem separation: the separator trait, the model download,
                 Split to stems. Used by the app only.
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
- make a system call: file I/O, logging, thread spawning, sleeping (waking a
  parked worker thread is the one exception; see Parallel processing). The
  sanctioned way to tell a background thread there is work is a `Doorbell`
  (`noodle-engine/src/doorbell.rs`): `ring()` is two atomic operations and an
  `unpark` only if the thread is asleep. Background workers wait on one rather
  than polling on a timer
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

## Where the detail is

Each file below is one part of the design and the rules that part follows.

| File | Covers |
|---|---|
| [data-model.md](architecture/data-model.md) | `noodle-core`: graph, groups, tracks, clips, commands and undo, files |
| [signals.md](architecture/signals.md) | Wire kinds (audio, events, spectral), voices, shapes, the node API |
| [synths-and-library.md](architecture/synths-and-library.md) | Synths as groups, the library and presets, instrument/dynamics/effect nodes, Script |
| [feedback-and-silence.md](architecture/feedback-and-silence.md) | Feedback loops, silence skipping |
| [transport-and-devices.md](architecture/transport-and-devices.md) | Engine and transport, several devices, surround, live monitoring |
| [midi-and-recording.md](architecture/midi-and-recording.md) | MIDI input (1.0 and 2.0), MIDI clips, recording into the arrangement |
| [time-warping.md](architecture/time-warping.md) | Clip warps, Repitch, Grain, Beats |
| [stems.md](architecture/stems.md) | Stem separation and Split to stems |
| [automation.md](architecture/automation.md) | Lanes and Automation nodes |
| [caching.md](architecture/caching.md) | Cache keys, freeze, offline nodes, export |
| [plugins.md](architecture/plugins.md) | CLAP, AU and VST3 hosting, sandboxing |
| [performance.md](architecture/performance.md) | Delay compensation, CPU meters, parallel processing, profiling |
| [ui.md](architecture/ui.md) | `noodle-app`: editor, widgets, mixer, views |
| [testing.md](architecture/testing.md) | Test strategy |

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
