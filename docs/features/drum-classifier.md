# Drum hit classifier (the consumer side)

Issue #499, ROADMAP "Drum rack from clip, follow-ups". Noodle labels each drum bin with a
small network over the hit detector's features, as an ONNX file, and falls back to the rule
labeller (`guess` in `noodle-io/src/drum_bins.rs`) when there is no model. Binning is unchanged:
the classifier labels each bin's medoid.

**The model, its training data, the dataset generator, the training script, the research and the
model card live in a separate private repository, `Jezza672/drum-identifier`, not here.** Noodle
only consumes a published model. It depends on none of that code, and the model is never trained
in this repository. The repository depends on Noodle's crates (`noodle-io`'s detector and
`feature_vector`, `noodle-stems`' `OnnxLabeller`) so that the features a model is trained on are
exactly the ones the app computes.

## What Noodle does

- **Features.** `noodle_io::analyse_hits` cuts each hit and describes it with `HitFeatures`:
  24 log-mel bands, centroid, flatness, three-way energy split, decay, pitch, attack bursts, and a
  `Shape` of the envelope and the edges (six envelope slices, attack time, level and drop at the
  end of the cut, lead-in level, crest, clipped share, bursts over the whole cut, cut length).
  `noodle_io::feature_vector` turns a hit into the model's input, in a fixed order
  (`DRUM_FEATURES` = 50 values). Changing the features or their order bumps `DETECTOR_VERSION`
  and is a new model input contract.
- **Model contract.** `noodle_stems::drum::OnnxLabeller` opens an ONNX file with one input
  `features` `[n, DRUM_FEATURES]` and a first output of class probabilities `[n, 9]` for
  `DRUM_CLASSES` (kick, snare, clap, closed hat, open hat, crash, ride, tom, other; the app has no
  ride, so it folds into crash). An optional output named `purity` `[n, 6]` gives the probability
  of each of `PURITY` (clean, double, truncated, malformed, mixed, spurious). Opening checks the
  shapes. Normalisation is inside the graph, so the app feeds raw features.
- **Labels.** Under `LEARNED_CONFIDENT` (0.5), or when the top class is `other`, the answer is
  `Unknown`. A tom is low or high by its pitch (`TOM_SPLIT_KEY`).
- **The purity gate.** Only one hit that finishes ringing before the end of its cut may enter a bin of
  single hits. A hit is accepted when `clean` is the likeliest class with at least `CLEAN_GATE`
  probability **and** the detector flags neither `mixed` nor `truncated` (`pure_enough`).
  `Binning::relabel` takes hits the labeller rejects out of their bins into `other`, and drops a
  bin left with no accepted hit, so no double or damaged hit is offered from a bin of single hits.
  A labeller with no purity output (the rules, an older model) holds nothing back.
- **App.** `Session::drum_labeller` uses `models/drums/classifier/classifier.onnx` in the app's
  data folder when it is there and opens, else the rules. The model runs once per hit
  (`HitLabeller::assess`).

## Owed

- **Download of the published model** from Hugging Face (a manifest entry with address and
  checksum, a Models-tab row): there is no published model yet, and nothing is published without
  the user's say. Until then the file goes into the folder by hand.
- **Listen on the Mac:** open the drum rack dialog with a model installed and compare the labels
  and the bins to the rules on real loops.
- **A patch-CNN input** (a log-mel patch beside the features) would need the detector to produce a
  patch; not started.
