# Noodle

A node-based DAW. Build sounds and mixes by wiring nodes together in an editor
modelled on Blender's compositor, and arrange them on a timeline. It's being
built for real-time audio input and output, cached pre-processing (freeze,
offline-only nodes, export), and hosting CLAP and VST3 plugins.

The name comes from Blender's word for the wires between nodes, and from
*noodling*, musician slang for idly messing around on an instrument.

**Status:** every milestone through M11 is built in code (see [what was built](docs/features/README.md)); what is left is checks on real hardware and a few deferred items, in the [roadmap](docs/ROADMAP.md).

## Trying it

You need a stable Rust toolchain (via [rustup](https://rustup.rs)).

```sh
cargo run -p noodle-app -- examples/vibrato.ron
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
- [Roadmap](docs/ROADMAP.md): open work.
- [Delivered features](docs/features/README.md): what each milestone built.

## Licence

GPL-3.0-or-later. See [LICENSE](LICENSE).
