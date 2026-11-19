# Drum separation analyser (#501)

Version 3 of Create drum rack from clip (`docs/design/drum-rack-from-clip.md`,
Decisions 6 to 10): an optional analyser that runs the dialog's chosen drum model
over the clip and finds the hits again in each part, so a hat over a kick is two
hits, each cut from its own part.

**Hits are found per part.** Detecting on the whole mix and then checking each hit
against the parts left most hits of a real song mixed (the detector flagged about
80% of the hits of eight real drum stems). `separate_and_find_hits` instead runs
`analyse_hits` on every part. Detections in different parts within 25 ms are one
event: the parts within `KEEP_WITHIN_DB` (20 dB) of the loudest at its first 30 ms
keep their detection, the rest is the separator's leakage and is dropped. The hits
come back in onset order and *replace* the analysis's hits (`DrumAnalysis::generation`
counts the replacements, so the dialog bins them again). Each hit plays the part it
was found in (`HitSeparation::home`), as if taken; "Use the original hit" goes back
to the mix. On real songs, with drumsep, about half the hits then have one part at
least 10 dB above the others; the rest are drums struck together or leakage.

- **`noodle-stems/src/drum/analyse.rs`:** `separate_hits` (hits given) and
  `separate_and_find_hits` (hits found per part) resample the clip to the
  model's rate, runs a `PartSeparator`, brings the parts back to the clip's rate and
  length, and for each hit compares the parts' level over its first 80 ms. A part is
  *active* when it is within `ACTIVE_WITHIN_DB` (10 dB) of the loudest part and above
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
- **Not done:** the listening check on real kits (on `ROADMAP.md`); the 10 dB margin is
  tuned on measurements of drumsep's leakage in real songs, not by ear.
