# Drum separation models (#518)

The runtimes behind the drum rack's model choice (`docs/design/drum-rack-from-clip.md`,
Decisions 6 to 10). This is the library side, in `noodle-stems/src/drum/`;
the app side (Models tab, first-use popup, dialog drop-down) is at the end.

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

## The app side (#520): Models tab, first-use popup, drop-down

`noodle-app/src/models.rs` (`Models`, held by the app).

- **Enabled** is a preference (`Prefs::drum_models`, id to bool). A model not in it
  has not been asked about. It is machine-local, so not exported or embedded.
- **Ready** means enabled and downloaded; the drum rack dialog's "Separation model"
  drop-down lists only those, in the app's order (LarsNet, Timbrica, drumsep). With
  none it says where to switch one on. The choice is held by the dialog; nothing
  consumes it until the analyser (version 3) is built.
- **Settings, Models tab:** per model an enable toggle, parts and what it lacks,
  size, licence (with the non-commercial warning), credit, provenance, a source
  link, and Download / Cancel (progress bar) / Remove, plus where the weights are
  stored. Downloads run as a `Job` over `DrumModelStore::fetch`.
- **First-use popup:** shown the first time Create drum rack is used, while some
  downloadable model is unasked. One toggle per model, non-commercial ones start
  off so a download of them is a choice. Download selected enables and fetches the
  ticked ones; Not now leaves all off; both answer them.
- **Not published:** a model without an address and checksum shows "Not available
  yet" with Download disabled, and the popup leaves it unasked until it can be
  downloaded (all three today, #523).
