# Spectral effects (the M11 follow-ups)

Batch B of the M11 follow-ups: a frame size per spectral wire and five effects built on it. Each effect is a library group of primitives with its controls exposed as group inputs, so nothing here is a monolithic node.

## Frame size per wire (ADR 0007, revised)

`Analyze` has a `size` config (256 to 8192, a power of two, default 2048; the hop is a quarter of it). Every spectral node on the wire adopts it: `NodeType::spectral_size` (given the spectral inputs' sizes) is the default way a node says what size its outputs have, and an Analyze overrides it from its config. The compiler (`infer_spectral_sizes`) propagates sizes along spectral wires and reports a diagnostic when a node's spectral inputs disagree (a Cross of a 1024 and a 4096 analysis, say). `Setup::spectral_size` tells the node its size; buffers are pooled per size; `NodeKey` includes the size so a carried-over instance is rebuilt when an upstream Analyze changes size (a bug the allocation test found: a carried node kept its old size and indexed out of bounds). `Synthesize` latency follows the size (`synth_latency`).

## The nodes

| Node | Type | What it does |
|---|---|---|
| Spectral Pitch | `noodle.spectral.pitch` | Phase vocoder pitch shift by `semitones`, with identity phase locking (peaks, valley-bounded regions, true frequency from the phase difference). |
| Spectral Stretch | `noodle.spectral.stretch` | Time stretch on frames: reads a bounded ring of the last `length` seconds at `rate`, wrapping at `length`. On a live wire it cannot lengthen the total duration, so it is a looping or slowed window over the recent past. |
| Spectral Compress | `noodle.spectral.compress` | Per log band: a dB envelope, threshold, ratio and soft knee, attack and release in seconds. An upward mode lifts quiet bands, capped at +30 dB. |
| Spectral Denoise | `noodle.spectral.denoise` | Subtracts a noise power profile (a running mean). The profile comes from a `noise` spectral input (frames that are silent are ignored) or from `learn`. The `noise` wire exists because `learn` state is lost on seek or reset, which the playback cache needs to be a pure function of the start time. |
| Convolver | `noodle.effect.convolver` | Exact uniform partitioned convolution (256-sample partitions, a frequency-domain delay line), latency 256 reported through `Node::latency`. The impulse response is a sample input; the compiler writes it into `config["ir"]` from the `SampleStore`, up to 10 s. |

The groups in the library (generated from `noodle-eval/src/song/spectral_effects.rs`): Pitch Shifter, Time Stretch, Spectral Compressor, Denoiser, Convolution Reverb. Three impulse responses ship inside the binary as system samples (`ir-room`, `ir-hall`, `ir-plate`; made by `assets/samples/make-impulse-responses.py`, noted in `SOURCES.md`). Node presets were added for the primitives.

## Tests

- `noodle-nodes/tests/spectral_effects.rs`: every frame size gives the dry signal back, a size that is not a power of two is rounded, nodes adopt the wire's size and mismatches are reported, pitch shift of a tone and of a chord, stretch at 1 and slower, compress (loud and quiet tones, upward mode), denoise with a noise wire and with nothing learned.
- `noodle-eval/tests/spectral_templates.rs`: each group template does what its name says (octave up, down by the control, stretch keeps pitch, compressor cuts a loud tone, denoiser removes shown noise, reverb rings after a click).
- The convolver has unit tests against direct convolution in `convolver.rs`.
- `noodle-nodes/tests/realtime.rs`: the counting-allocator test runs the new nodes and switches frame size between plans.
- Goldens: `spectral-pitch`, `spectral-stretch`, `spectral-compress`, `spectral-denoise`, `convolution-reverb` (examples of the same names).

## Still open

Pitch Shifter and Time Stretch are vocoder quality, not a stretcher engine; a transient-preserving mode and formant preservation (the Formant Shift node can be chained after) are not built.
