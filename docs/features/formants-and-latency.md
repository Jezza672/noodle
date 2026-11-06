# Formants and latency

Issues #424 (S05) and #429 (S19), from the new-user runs.

## Formant-preserving pitch shift (#424)

A pitch shift made by stretching and reading back (see [clip-keys.md](clip-keys.md)) moves the formants with the pitch, so a vocal carried up sounds like a smaller singer. Two pieces fix it, both on the same function.

- **`Formant Shift`** (spectral node, `noodle.spectral.formant`). Takes a spectrum and moves its outline by `shift` semitones (-12 to 12) without moving the harmonics. `smooth` (50 to 2000 Hz, default 400) is how wide the outline is smoothed: wider than the gap between harmonics. Presets: Smaller, Larger. Between Analyze and Synthesize it is also a creative effect. The work is `FormantWarp` in `noodle-engine/src/spectral.rs`: magnitude smoothed over a window, then each bin scaled by `E(f / ratio) / E(f)` with the phases kept. A bin is lifted or cut by at most 24 dB, so a narrow feature moved a long way falls short.
- **Keep formants** on a transposed clip (the Key row of the clip panel, `Transpose::keep_formants`). The stretch worker's `Pitcher` runs the shifted audio through `clip/formant.rs` (Analyze, `FormantWarp` at `1 / r` for the pitch ratio `r` at that frame, Synthesize) so the pitch stays shifted and the formants go back. It follows the key changes along the clip, because the ratio is read from the `PitchPath` for each frame. The cache key holds the flag. Best on voices and solo instruments; large shifts of thin or noisy material can sound strained.

There is no spectral pitch shifter node yet (Spectral Shift moves by hertz, not by ratio), so a pitch-shifting group of primitives is not possible; it stays on the roadmap with the other frame-based nodes.

Tests: `FormantWarp` in `spectral.rs`, `Formant Shift` in `noodle-nodes/src/spectral.rs`, `clip/formant.rs` (the stage puts a shifted voice's formant back), `tests/transposed_clips.rs` (a voice carried up four semitones matches the singer at that pitch with the flag, and is brighter without it; with the stage's ratio forced to 1 the test fails), the spectral allocation test in `realtime.rs`. The S05 route ticks the box.

## Latency display (#429)

`Controller::latency_report` (`noodle-engine/src/plan.rs`: `LatencyReport`, `NodeLatency`, `HeldInput`) gives, per node, its own reported latency, how late its output sounds and which inputs delay compensation held back. The session keeps the report from each compile (`Session::plan_latency`); `noodle-app/src/latency.rs` turns it into words. A node that adds latency shows `+N ms` in its header, one that lines up paths shows `holds N ms`, and the properties panel has a Latency section. A group sums what is inside it. Nothing shows when no node reports latency.

Tests: `tests/pdc.rs` (the report names the latent node and the held input; an empty report with no latency), `session.rs` (a Synthesize path joined with a dry path: the Synthesize reports its latency and the mixer's second input is held), `latency.rs`.
