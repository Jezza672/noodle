# Drum separation analyser (#501)

Version 3 of Create drum rack from clip (`docs/design/drum-rack-from-clip.md`,
Decisions 6 to 10): an optional analyser that runs the dialog's chosen drum model
over the clip and finds hits where two drums sound together.

- **`noodle-stems/src/drum/analyse.rs`:** `separate_hits` resamples the clip to the
  model's rate, runs a `PartSeparator`, brings the parts back to the clip's rate and
  length, and for each hit compares the parts' level over its first 80 ms. A part is
  *active* when it is within `ACTIVE_WITHIN_DB` (20 dB) of the loudest part and above
  -55 dB; a hit with two or more active parts is mixed. Only each hit's window of each
  part is kept (`HitSeparation::audio`), not whole parts, so a result is small for a
  clip of any length. Parts are `DrumPart`s, so the analyser does not care which model
  ran (a model's own part mapping is in `mapping.rs`).
- **Session** (`session/drum_rack.rs`): `Session::separate_drum_hits(model_id)` runs
  on a cancellable `Job` with progress (`Edit::SeparateDrumHits`,
  `CancelDrumSeparation`). It downloads ONNX Runtime first when the machine has none,
  as Split to stems does. The result is cached by clip window, file hash, detector
  version and model id and version, so asking again is instant. It is held in
  `DrumAnalysis::separation`.
- **Taking a part:** a mixed hit is never replaced by a separated one by default
  (separator artefacts). `Edit::TakeSeparatedPart(hit, Some(part))` puts the part's
  audio for that hit into the project's samples and the rack plays it;
  `None` goes back to the hit's own audio.
- **Dialog:** next to the model drop-down, "Find overlapping drums" (with a progress
  bar and Cancel while it runs). Hits it finds mixed rank last in their group, are
  labelled with their parts ("mixed: kick + hi-hat"), and have a "Use part…" menu.
  The slider re-bins, which takes the separation in again.
- **Tests:** the analyser against canned parts (bleed under the margin is not mixed, a
  silent hit has no part, resampling, cancelling); the session against a fake
  band-splitting separator (mixed hit found, cache reuse, cancel, a model that will not
  open, taking and releasing a part into the rack); the dialog's ranking and label. The
  S09 route checks the dialog says where to get a model when none is downloaded: a
  route acts only through the pointer, so running a model is covered by session tests.
- **Not done:** the listening check on real kits (on `ROADMAP.md`); the 20 dB margin is
  untuned on real material.
