# Noodle as a plugin (#544), chunks 1 to 3

The design and the later chunks are in #544 and `docs/ROADMAP.md`; how it works is "Noodle as a plugin" in [../architecture/plugins.md](../architecture/plugins.md).

**Chunk 1, shell split and host backend.** `noodle-app` is a library with the desktop shell in `main.rs`. `noodle-io` has the host backend. The Input and Output nodes take the device `host`, and MIDI In and Out the port `host`. Tests: odd block sizes (1, 37, 128, 4096) against an offline render; `realtime.rs` covers the host path. Checked that the tests fail: with the chunking removed, five of the seven fail; with seek detection removed, the playhead test fails.

**Chunk 2, the headless CLAP.** `noodle-plugin`: Noodle (effect) and Noodle Instrument, stereo audio, notes in and out, state, latency, the two default projects. Tested by `noodle-plugins`' host loading the built library. Allocation-free process is covered where the code is, in `realtime.rs`; the CLAP glue only copies into scratch made at activation.

**Chunk 3, transport, tempo and parameters.** The host's play, stop, locate and tempo drive the engine (`Processor::set_host_tempo`; `TempoSource::Host`). The bank of 64 parameters and the Host Parameter node; `Session::expose_to_host` (no button yet, since the plugin has no window).

**Decisions made on the open questions:** one binary with two plugin entries (effect and instrument); 64 parameters; an instance holds a full project (the group-as-instance idea is not built).

**Owed:** the CLAP validator in CI, then the chunks 4 to 6 of the roadmap.
