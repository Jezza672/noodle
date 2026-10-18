# M0: Real-time core, headless

Delivered. This is the record of what was planned and built; open work is in [ROADMAP.md](../ROADMAP.md), the design in [ARCHITECTURE.md](../ARCHITECTURE.md).

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
