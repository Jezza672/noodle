# Drum separation models (#518)

The runtimes behind the drum rack's model choice (`docs/design/drum-rack-from-clip.md`,
Decisions 6 to 10). This is the library side, in `noodle-stems/src/drum/`;
the app side (Models tab, first-use popup, dialog drop-down) is at the end.

- **Manifests** (`manifest.rs`): one per model, in the UI order LarsNet, Timbrica,
  drumsep: parts and their order, the parts it lacks, licence and non-commercial
  flag, credit, source, sample rate, chunking (segment, STFT size, hop), layout and
  files (address, SHA-256, size). A file with no address or checksum cannot be
  downloaded (`StoreError::NotPublished`); all three have them (see below).
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

## Real weights (#523)

Checked with `tests/drum_runtime.rs` against the real files (set `NOODLE_DRUM_MODEL`
and `NOODLE_DRUM_WEIGHTS`; its synthetic-kit test also checks the kick part
dominates a kick-and-hat mix). Not listened to on real kits yet.

- **Timbrica** (`Faridzar/timbrica-drum-separator`, pinned to a commit): input `mag`
  `[1, 2, 2048, 512]`, output `mask` of the same shape, a 0 to 1 gain, so the layout is
  `MaskOutput::Mask` (it was written as magnitude). fp16 inside with float32 at the
  edges. The snare is the "r2" retrain; the first snare file is superseded.
- **LarsNet:** the authors' `UNet` is the same network. Our ONNX conversion exports
  `produce_mask` (input `mag`, output `mask`, 2048 bins, 512 frames, the Nyquist bin
  dropped on the way in and zero on the way out), so it is `MaskOutput::Mask` too.
  Hosted as a separate credited CC BY-NC 4.0 download at
  `huggingface.co/jezza672/larsnet-onnx` (five 39,346,035-byte files), never in the
  repository or the binary. It matches PyTorch to about 1e-5.
- **drumsep:** `waveform [b, 2, 1764000]` and `magnitude [b, 4, 2048, 1723]` in,
  `freq_output [b, 4, 4, 2048, 1723]` and `time_output [b, 4, 2, 1764000]` out, stems in
  the manifest's order. splitzo's file does not load in ONNX Runtime (four `EyeLike`
  nodes on a boolean input have no CPU kernel), so we host a copy with those constants
  changed to float, same result, at `huggingface.co/jezza672/drumsep-onnx` (MIT).
  The model is fixed at 40 s segments.
- Downloads are pinned to commit hashes, and the Hugging Face CDN redirect needs
  `us.aws.cdn.hf.co` reachable.

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
