# Looper node (#541), node only

`noodle.util.looper` (Effects, `crates/noodle-nodes/src/looper.rs`): record the input, loop it on the bar, overdub, undo the last layer, clear. Every control is a parameter (`record`, `undo` and `clear` are rising edges, `play` mutes while the loop keeps running, `feedback`, `level`, `length` Auto/1/2/4/8 bars), so a Button, a MIDI Control or a Launch Quantise drives it through visible wires. Config `max_seconds` (default 30) sizes the two preallocated buffers.

- **State machine:** Empty, Recording, Playing, Overdubbing; `record` cycles them. Overdub starts and stops only at the loop's wrap, so every layer is a whole pass and undo is exact (an undo during an overdub takes effect at the wrap).
- **Sync:** with `length` Auto the stop gesture is held to the next bar line (from the transport's tick, tempo and signature at the start of recording); a fixed length ends by itself. The loop's length in samples is fixed then, and it runs on the node's own sample counter, so it plays with the transport stopped.
- **Seams:** the head of the loop is crossfaded (about 5 ms) with the live input that follows the end; overdub writes ramp in and out over the same time.
- **Undo:** `base` and `layer` buffers; a new pass folds the old `layer[p]` into `base[p]` (times `feedback`) before overwriting it, so no bulk copy runs on the audio thread. One level.
- **Shape:** one voice out, channels follow the input. Nondeterministic, so never cached or frozen. The recording is session state, not saved; `reset` (a seek) keeps it, so the node is skipped in `reset_matches_fresh.rs`. Changing `max_seconds` or the channel count loses the loop.
- **Tests:** unit tests with `Harness` at 1 kHz (bar length, sine repeat, mid-bar stop, fixed length, overdub, feedback fold, undo, clear, reset, seam, mute); `looper_never_allocates_in_any_state` in `realtime.rs`. Each was shown to fail by breaking the code (no crossfade, no fold, no-op undo, reset clearing the loop, an allocation in `process`).

Not done: the state and playhead display, the `system:template:looper` group (Buttons, Launch Quantise, Mono Note) and the S27 route, which stay in ROADMAP.md.
