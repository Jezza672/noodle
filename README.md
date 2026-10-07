# Noodle

A node-based DAW. Build sounds and mixes by wiring nodes together in an editor
modelled on Blender's compositor, and arrange them on a timeline. It has
real-time audio input and output, cached pre-processing (freeze, offline-only
nodes, export), and will host CLAP and VST3 plugins.

The name comes from Blender's word for the wires between nodes, and from
*noodling*, musician slang for idly messing around on an instrument.

**Status:** early planning and scaffolding. Nothing runs yet. See the
[roadmap](docs/ROADMAP.md).

## Building

You need a stable Rust toolchain (via [rustup](https://rustup.rs)).

```sh
cargo run -p noodle-app
```

## Documentation

- [Architecture](docs/ARCHITECTURE.md): how the engine, graph and UI fit together.
- [Roadmap](docs/ROADMAP.md): milestones and what each one proves.

## Licence

GPL-3.0-or-later. See [LICENSE](LICENSE).
