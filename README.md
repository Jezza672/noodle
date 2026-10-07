# Noodle

A node-based DAW. Build sounds and mixes by wiring nodes together in an editor
modelled on Blender's compositor, and arrange them on a timeline. It's being
built for real-time audio input and output, cached pre-processing (freeze,
offline-only nodes, export), and hosting CLAP and VST3 plugins.

The name comes from Blender's word for the wires between nodes, and from
*noodling*, musician slang for idly messing around on an instrument.

**Status:** the real-time core works headless. Graphs compile, run with
seamless live edits, play through your speakers, and render to WAV. There's
no UI yet. See the [roadmap](docs/ROADMAP.md).

## Trying it

You need a stable Rust toolchain (via [rustup](https://rustup.rs)).

```sh
cargo run -p noodle-cli -- play examples/vibrato.ron
cargo run -p noodle-cli -- render examples/vibrato.ron vibrato.wav
```

`play` runs until Ctrl-C, and picks up changes to the file while it plays.
On Linux, install the ALSA headers first (`libasound2-dev` on Debian and
Ubuntu).

Each project in [`examples/`](examples) is a readable RON file. Open one to
see how its nodes are wired.

## Documentation

- [Architecture](docs/ARCHITECTURE.md): how the engine, graph and UI fit together.
- [Roadmap](docs/ROADMAP.md): milestones and what each one proves.

## Licence

GPL-3.0-or-later. See [LICENSE](LICENSE).
