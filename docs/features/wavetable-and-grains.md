# Wavetable and Grains

Issues #536 and #537.

## Wavetable oscillator

`noodle.osc.wavetable` (`noodle-nodes/src/wavetable.rs`), category Oscillators,
a `LaneKernel` like the other oscillators (silence skipping on `frequency`).

- **Ports.** `table` (a sample input, also the `table` config text, so a wire
  from a Sample node overrides it as on the Sampler), `frequency` (audio rate),
  `position` (0 to 1, offset modulation; neighbouring cycles crossfade) and
  `phase` (added to the running phase). `cycle` (256 to 4096, default 2048) is
  config: it decides how the file is cut. A ragged end is dropped; a file
  shorter than one cycle is one cycle, stretched.
- **Aliasing.** The table is built when the node is built, off the audio
  thread: each cycle is transformed once (`realfft`) and written back at one
  band-limited level per octave (level `l` keeps `cycle / 2 >> l` harmonics).
  The audio thread picks the level from the frequency, blends two levels over
  the bottom half of each octave and reads with linear interpolation. Nothing
  allocates there. Levels are stored at full length; the half-length saving the
  issue mentions was not needed (a 64-cycle table of 2048 is 5.6 MB).
- **Sharing.** Two nodes with the same table reference and `cycle` share one
  built `Arc<Table>` (a map of weak references in the node type).
- **Tables that ship** are generated, not recorded:
  `cargo run -p noodle-nodes --example wavetables` writes
  `assets/samples/wt-*.wav` (basic shapes, pulse width, harmonic sweep, formant
  sweep, digital; 16 cycles of 2048), registered as system samples (CC0, ours).
- **Wavetable Synth** template (8 voices, two oscillators with a detune, an SVF,
  an amplitude envelope, an LFO and a second envelope added to `position` by a
  Math node) with presets Glass Sweep, Vowel Pad and Digital Pluck.

Tests: a sine table matches `noodle.osc.sine`; a saw table at 5 kHz has under
-40 dB between its harmonics (proven to fail with the level forced to 0);
`position` halfway is the mean of two cycles; short and ragged files; sharing;
`realtime.rs` with every parameter modulated. `examples/wavetable.ron` has a
golden render.

## Grains

`noodle.instrument.grains` (`noodle-nodes/src/grains.rs`), category
Instruments: a granular sample player, one cloud per voice, driven by signals.

- **Ports.** `sample` (sample input), `gate`, `position`, `spray`, `size`,
  `density`, `jitter`, `pitch`, `frequency` with `root` (so a cloud follows
  Voices), `pitch_spray`, `reverse`, `spread`, `shape`, `seed`; outputs `out`
  (stereo) and `playhead`. Config `grains` (4 to 64, default 24).
- **Rules.** Each cloud has a fixed array of 64 grains (`grains` limits how many
  play; a due grain is skipped when all are busy, never stolen). A cloud's
  generator is seeded from the node seed, `seed` and the voice, restarted by
  `reset`, and advanced five numbers per due grain, so renders repeat. Output
  is scaled by `1 / sqrt(max(1, density x size))`. A lane with `gate` silent
  and no grain playing is idle.
- **Templates.** Grain Pad (Voices into Grains following pitch, filter,
  envelope, LFO on position; the cloud keeps starting grains until the envelope has finished its release; presets Steel Haze, Banjo Cloud, Frozen Bell) and
  Grain Freeze (no notes, for drones; presets Steel Drone, Banjo Mist, Frozen
  Glass), using shipped CC0 samples. Grains also has node presets Freeze,
  Texture and Scatter.

Tests: one grain equals the windowed slice; density 10 without jitter starts a
grain every 100 ms; a seed repeats and another differs; voices differ (proven to
fail with the voice number removed from the seed); a low gate lets grains
finish and starts none; level within 4 dB from density 5 to 100; a full cloud
skips without clicking; reset equals new; `realtime.rs`.
`examples/grains.ron` has a golden render. Both examples keep their audio in
`examples/samples/`.

`noodle-eval/tests/instruments.rs` plays the Wavetable Synth and Grain Pad like
the other instruments and checks the Grain Freeze drones.

## Display and drops (second PR)

- **Properties panel.** `sample_nodes.rs`: the Wavetable shows the stack of
  cycles faintly and the cycle at `position` brightly (the unmodulated slider
  value; a wired position shows where the slider rests); Grains shows the
  sample with the position as a line, `spray` as a band and the grains playing
  as ticks. The ticks come from telemetry: Grains reports each playing grain's
  place in the sample through `Telemetry::open_positions` (64 slots a voice),
  as the Sampler reports its voices.
- **Drops.** A clip, a file or a Sounds entry dropped on a Wavetable, a Grains
  node, or a group holding exactly one Sampler, Grains or Wavetable
  (`editor::pad_sampler`, `sample_drop::{choose, onto_sampler}`) sets its
  sample: a clip's part of the file is a window of the reference; Grains also
  gets `root` from the sample's pitch. The panel's "Load sample" and "Browse
  sounds" work the same way.
- **Not done.** Guessing `cycle` from the file and a `wavetable` tag for the
  Sounds panel (it does not list system samples) are still open (ROADMAP). The
  S17 route is unchanged: sending a rendered clip into Grains does not make the
  slicing route shorter, so the drop is covered by an app test instead.
