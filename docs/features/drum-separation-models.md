# Drum separation models (#518)

The runtimes behind the drum rack's model choice (`docs/design/drum-rack-from-clip.md`,
Decisions 6 to 10). The settings tab, the first-use popup and the dialog's
drop-down are separate issues; this is the library side, in `noodle-stems/src/drum/`.

- **Manifests** (`manifest.rs`): one per model, in the UI order LarsNet, Timbrica,
  drumsep: parts and their order, the parts it lacks, licence and non-commercial
  flag, credit, source, sample rate, chunking (segment, STFT size, hop), layout and
  files (address, SHA-256, size). A file with no address or checksum cannot be
  downloaded (`StoreError::NotPublished`); **none of the three has them yet** (see below).
- **`PartSeparator`** (`mod.rs`): N named `DrumPart`s (kick, snare, toms, hi-hat,
  cymbals, and `Rest` for Timbrica's hat and cymbals together), cancellable, at the
  model's rate. The four-stem `Separator` of Split to stems is left as it is.
- **Runtimes.** The STFT front end is in Rust (`stft.rs`), outside the graph; the
  network is behind a trait, so chunking, blending, remainder and cancelling are
  tested with no ONNX Runtime.
  - `MaskSeparator` (`mask.rs`): LarsNet and Timbrica. One network per part takes the
    mix's magnitude spectrogram; chunks of the network's frame count overlap by half,
    are blended with triangular weights, and the gain is applied to the mix's complex
    spectrogram (the mix's phase). `Rest` is the mix minus the other parts.
  - `HybridSeparator` (`hybrid.rs`): drumsep, a Hybrid Demucs. Waveform and
    complex-as-channels spectrogram in, spectrogram and time branches out, summed;
    Demucs's padding, overlapping segments, zero-mean unit-variance scaling.
  - `onnx.rs` (feature `onnx`): `open_separator(manifest, store, runtime)` checks each
    network's shapes against the manifest and says what it found if they differ.
- **Mapping** (`mapping.rs`): `part_for(DrumKind, parts)` and `pad_sources` put a
  model's parts onto the rack's pads (hats and crash fall back to `Rest`; claps have no
  part), so the analyser (#501) is model-independent.
- **Store** (`store.rs`): `DrumModelStore` keeps files by SHA-256 in the shared
  download cache, with consent, progress, cancel, size on disk and remove.

## Not verified against real weights

The cloud session cannot reach Hugging Face, so the tensor layouts (bins, frames,
output kind, input order) are written from the papers and cards and are **unchecked
against real files**. Opening refuses a mismatch with the shapes it found. Before
release: put the real files in a folder and run `tests/drum_runtime.rs` (header
explains), fix `Chunking`/`Layout` in the manifest if the shapes differ, fill in each
file's `url`, `sha256` and `size`, and listen on real kits.
LarsNet needs our own ONNX conversion hosted as a separate credited CC BY-NC 4.0
download (never in the repo or binary).
