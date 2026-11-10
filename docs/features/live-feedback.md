# Live visual feedback

The graph shows more of what the sound is doing, and each piece can be switched off for a user who wants raw performance. The design is in [../architecture/ui.md](../architecture/ui.md) under "Live feedback".

- **Event wires** light up while notes flow and fade after.
- **Audio wires** glow with the level of the signal in them, and turn red when it clips.
- **Level bars** on every wired audio input (as before) and now on every audio output.
- **Silent nodes** are faded: a node that made no sound in its last block, or that nothing listens to.
- **Modulation wires** pulse when their signal moves.
- **Voice lights**: a dot per voice on polyphonic nodes, lit for the voices that sound.
- **Beat pulse** on clocks, arpeggiators, pattern players, the metronome, swing and launch quantise while playing. A node type opts in with `NodeType::pulses_on_beat`.
- **CPU heat tint**, off by default.
- **Display Settings** (View menu) holds a switch for each, and *Performance mode* turns all of them off at once. The choices are saved in the preferences.

The engine side is `Telemetry::gather()` (four flags the audio thread reads once per node per block), output meters, event counts and per-node state. With a switch off the audio thread skips the measurement.

Left out: wires through a group's boundary don't glow (a group has no engine node of its own).

Tests: `telemetry` and `runtime/tests.rs` (output levels, event counts, node state, each stopping when switched off), `realtime.rs` (the reports don't allocate), `editor/body.rs` and `editor/draw.rs` (glow, fade, clipping, state, heat, beat), `display.rs`, `prefs.rs`, and the Display Settings dialog test in `app.rs`.
