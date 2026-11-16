# Drum hit classifier

Issue #499, ROADMAP "Drum rack from clip, follow-ups". Replaces the rule labeller
(`guess` in `noodle-io/src/drum_bins.rs`) with a small network over the same
feature vector, as an ONNX file so it can be published and swapped. Binning is
unchanged: the classifier labels each bin's medoid.

## What is built

- **Data** (`noodle-eval drum-dataset build`, `crates/noodle-eval/src/drum_data.rs`).
  The `drums-cc0` pack's one-shots, alone, and 1,500 synthetic loops (seed 1): kit pieces
  on a grid with timing jitter and level changes, overlap, bleed in stem-like loops, room,
  EQ and noise, cut by the real `analyse_hits`, each detection labelled from the nearest
  placed hit (only the nearest detection per placed hit; the rest are `spurious` and
  dropped). Samples are split 70/15/15 by a hash of the file name *without mic and folder
  words*, so a close-mic and a room version of one hit share a split; a loop uses only its
  split's samples. `hits.csv` holds features only; `manifest.json` lists every sample with its
  licence and credit; `CREDITS.md` carries the CC BY 4.0 credit (MuldjordKit, Lars Muldjord).
  Eval songs are never used.
- **Taxonomy.** kick, snare (rimshots, snare-extra), clap, closed hat, open hat (half-open too),
  crash (china, splash), ride, tom, other (cross-stick, snap, body, percussion, stacks:
  should come out `Unknown`). The app has no ride, so ride folds into `Crash`. A tom is low or
  high by its pitch at key 52 (`TOM_SPLIT_KEY`).
- **Training** (`tools/drum-classifier/train.py`, PyTorch): an MLP of 256 and 256 units over 36
  raw inputs (`noodle_io::feature_vector`, normalised inside the graph) with softmax outputs
  `probs` [n, 9] and `confidence` [n]. Hits quieter than -45 dB and loop hits labelled from a
  placed hit more than 35 dB down are neither trained on nor scored. The script checks the
  ONNX output against PyTorch.
- **App.** `noodle_io::HitLabeller` (`Rules` by default); `noodle_stems::drum::OnnxLabeller`
  runs the model; `Session::drum_labeller` uses the file `models/drums/classifier/classifier.onnx`
  in the app's data folder when it is there and opens, else the rules. Bins are relabelled
  after binning (`Binning::relabel`). Under 0.5 probability, or when the top class is `other`,
  the answer is `Unknown`.

## Results

Test split, scored in Rust through the same ONNX path the app uses
(`noodle-eval drum-dataset baseline --data D [--model classifier.onnx]`). The test split holds
samples the model never saw, in loops it never saw. Per-hit scores on these loops are harsh
(overlap and bleed on purpose); a bin is labelled from its medoid, which is a cleaner hit.

| | rules | learned |
|---|---|---|
| One-shots, alone: right / confidently wrong | 26% / 47% | 73% / 21% |
| Loops, all hits | 26% / 42% | 39% / 18% |
| Loops, clean hits only | 37% / 29% | 46% / 23% |

Weak spots: closed hats mixed under kicks and snares, rides against hats, and toms in loops.
These come from the 24-band feature, not the network: a wider or bigger network did not help
(validation accuracy sat near 62% for every size tried).

## Purity (bin only pure samples)

The goal of binning is pure samples: only **one hit that finishes ringing before the end of its
cut** may enter a clean bin. The classifier has a second output, `purity` `[n, 6]`:
`clean`, `double` (flams, rolls, a hit doubled), `truncated` (still ringing at the end of the
cut, or cut by the next hit), `malformed` (chopped start, clipped, faded in), `mixed` (another
drum under it) and `spurious` (not a hit). Rationale and the survey behind it are in
`research/drum-classifier-accuracy.md`.

- **Labels** come from the generator (`purity_labels` in `noodle-eval/src/drum_data.rs`), from
  where hits were placed: the same drum, or a deliberate double, on top of a ringing hit makes
  both `double`; another drum's tail within 25 dB under a hit makes it `mixed`; another hit
  starting before it has fallen 30 dB makes it `truncated`.
- **Data** (dataset version 2): the one-shots; the same one-shots damaged (`chop_start`, `clip`,
  `fade_in`, `gate`: cut dead while ringing); standalone doubles (a flam to 0.6 s apart, same
  or another piece); and loops with 6% deliberate doubles. `hits.csv` has `purity` and
  `damage` columns.
- **Gate** (`noodle_io::clean_accepted`, `CLEAN_GATE`): a hit is accepted when `clean` is the
  likeliest class with at least `CLEAN_GATE` probability. A labeller with no purity output
  (the rules, or an older model) gates on the detector's `mixed`/`truncated` flags. The
  model's answer is ANDed with those flags. `Binning::relabel` takes rejected hits out of their bins
  into `other`, and drops a bin left with no accepted hit, so a double or damaged hit is never
  offered from a bin of single hits (only a labeller that judges purity does this; the rules do not).
- **Headline metric: false-accept rate**, the share of not-clean hits let into a clean bin,
  with a 95% Wilson interval, per kind of impurity and per damage type
  (`noodle-eval drum-dataset baseline --data D [--model M] [--gate G]`, "Purity gate"). The tests
  break the gate and the labelling on purpose and check the numbers move.
- **Result** (`drums-cc0`, 1,500 loops, 40 epochs, test split): the detector flags alone let 11.8%
  of not-clean hits through (56.5% of clean kept). The model at gate 0.77 (tuned on validation for
  at most 1% false accepts) lets 0.1% through (5 of 5,937) but keeps only 2.3% of clean hits;
  gate 0.5 gives 1.4% and 19% kept. So the gate is safe but too strict: the 36 features cannot
  separate a clean hit from the messy ones in loops. Envelope features and a patch CNN are next
  (PR 3); more kits and a held-out-by-kit split are PR 2. The test split has only 736 clean hits
  from a handful of kits, so read the intervals.

## Real-data tools

`tools/drum-real/` (README there) builds a hand-labelled set of real hits from openly licensed
audio (CC0, CC BY, CC BY-SA only; the manifest records source, licence and credit, and NC/ND rows
are refused). `drum_real_split` keeps the drums of a mix, `drum_real_detect` runs the app's
`analyse_hits`, and `label.py label` plays each hit for single-keystroke class and purity labels
(`clean double truncated malformed mixed spurious`). `label.py export` writes rows in the dataset
generator's `hits.csv` columns (a non-hit is label `spurious`, purity `spurious`, as the generator writes it; the `mixed` and `truncated` columns keep the detector's flags, never the human label). The data stays outside the repo. Nothing consumes the export yet.

## Owed

- **Publish the model** (and a dataset card, a model card): not done, and not to be without the
  user's say. `tools/drum-classifier/MODEL-CARD.md` has the licence and credit text. There is no
  manifest entry or Models-tab row yet because the model has no address and no checksum; add them
  when it is published (`DrumModelManifest` fits it badly; a small classifier entry is simpler).
  Until then the file goes into the folder by hand.
- **Listen on the Mac:** open the drum rack dialog with the model installed and compare the labels
  to the rules on real loops.
- **Detector recall** is a separate problem: on the synthetic loops it finds about 60% of placed
  hits, and about as many detections match nothing.

## Reports

### Rules

Run on the features of every detected hit.
A ride is right when the rules say `crash` (the app has no ride); `other` is right when they say `Unknown`.
Hits quieter than -45 dB are left out (the app drops them), as are loop hits labelled from a placed hit mixed more than 35 dB down, and `mixed` or `truncated` hits in the clean column.

#### One-shots, alone

73 hits: 26.0% right, 46.6% confidently wrong, 0 detections matched no placed hit.
Toms with a pitch: 1/4.

| truth \ guess | kick | snare | clap | closed_hat | open_hat | crash | ride | tom | other | Unknown | n | recall |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| kick | 5 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 5 | 100% |
| snare | 0 | 3 | 1 | 0 | 0 | 0 | 0 | 0 | 0 | 7 | 11 | 27% |
| clap | 0 | 0 | 3 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 3 | 100% |
| closed_hat | 0 | 2 | 4 | 3 | 0 | 0 | 0 | 0 | 0 | 1 | 10 | 30% |
| open_hat | 0 | 2 | 0 | 3 | 0 | 0 | 0 | 0 | 0 | 3 | 8 | 0% |
| crash | 0 | 2 | 2 | 1 | 1 | 0 | 0 | 0 | 0 | 4 | 10 | 0% |
| ride | 0 | 0 | 1 | 0 | 1 | 0 | 0 | 0 | 0 | 3 | 5 | 0% |
| tom | 1 | 1 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 2 | 4 | 0% |
| other | 4 | 5 | 2 | 1 | 0 | 0 | 0 | 0 | 0 | 5 | 17 | 29% |

#### Loops, all hits

6287 hits: 26.1% right, 42.3% confidently wrong, 4820 detections matched no placed hit.
Toms with a pitch: 13/248.

| truth \ guess | kick | snare | clap | closed_hat | open_hat | crash | ride | tom | other | Unknown | n | recall |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| kick | 818 | 37 | 86 | 15 | 3 | 0 | 0 | 1 | 0 | 502 | 1462 | 56% |
| snare | 30 | 246 | 128 | 9 | 1 | 0 | 0 | 0 | 0 | 391 | 805 | 31% |
| clap | 2 | 21 | 67 | 8 | 0 | 0 | 0 | 0 | 0 | 40 | 138 | 49% |
| closed_hat | 307 | 714 | 610 | 464 | 24 | 0 | 0 | 2 | 0 | 678 | 2799 | 17% |
| open_hat | 7 | 38 | 13 | 32 | 1 | 0 | 0 | 0 | 0 | 55 | 146 | 1% |
| crash | 15 | 11 | 13 | 9 | 2 | 0 | 0 | 0 | 0 | 17 | 67 | 0% |
| ride | 47 | 71 | 90 | 32 | 8 | 0 | 0 | 0 | 0 | 190 | 438 | 0% |
| tom | 65 | 45 | 23 | 2 | 0 | 0 | 0 | 0 | 0 | 113 | 248 | 0% |
| other | 16 | 78 | 36 | 10 | 1 | 0 | 0 | 0 | 0 | 43 | 184 | 23% |

#### Loops, clean hits only

1054 hits: 36.8% right, 28.9% confidently wrong, 0 detections matched no placed hit.
Toms with a pitch: 0/10.

| truth \ guess | kick | snare | clap | closed_hat | open_hat | crash | ride | tom | other | Unknown | n | recall |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| kick | 277 | 4 | 18 | 4 | 0 | 0 | 0 | 0 | 0 | 124 | 427 | 65% |
| snare | 5 | 44 | 23 | 2 | 0 | 0 | 0 | 0 | 0 | 92 | 166 | 27% |
| clap | 0 | 1 | 8 | 0 | 0 | 0 | 0 | 0 | 0 | 3 | 12 | 67% |
| closed_hat | 99 | 50 | 76 | 53 | 0 | 0 | 0 | 0 | 0 | 130 | 408 | 13% |
| open_hat | 2 | 0 | 1 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 3 | 0% |
| crash | 2 | 0 | 0 | 0 | 1 | 0 | 0 | 0 | 0 | 3 | 6 | 0% |
| ride | 5 | 0 | 1 | 0 | 0 | 0 | 0 | 0 | 0 | 2 | 8 | 0% |
| tom | 2 | 1 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 7 | 10 | 0% |
| other | 1 | 6 | 0 | 1 | 0 | 0 | 0 | 0 | 0 | 6 | 14 | 43% |


### Learned

Run on the features of every detected hit.
A ride is right when the rules say `crash` (the app has no ride); `other` is right when they say `Unknown`.
Hits quieter than -45 dB are left out (the app drops them), as are loop hits labelled from a placed hit mixed more than 35 dB down, and `mixed` or `truncated` hits in the clean column.

#### One-shots, alone

73 hits: 72.6% right, 20.5% confidently wrong, 0 detections matched no placed hit.
Toms with a pitch: 1/4.

| truth \ guess | kick | snare | clap | closed_hat | open_hat | crash | ride | tom | other | Unknown | n | recall |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| kick | 3 | 0 | 0 | 0 | 0 | 0 | 0 | 2 | 0 | 0 | 5 | 60% |
| snare | 0 | 10 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 1 | 11 | 91% |
| clap | 0 | 0 | 2 | 0 | 0 | 1 | 0 | 0 | 0 | 0 | 3 | 67% |
| closed_hat | 1 | 0 | 0 | 6 | 0 | 0 | 0 | 0 | 0 | 3 | 10 | 60% |
| open_hat | 0 | 1 | 0 | 1 | 6 | 0 | 0 | 0 | 0 | 0 | 8 | 75% |
| crash | 0 | 0 | 0 | 0 | 0 | 10 | 0 | 0 | 0 | 0 | 10 | 100% |
| ride | 0 | 0 | 1 | 0 | 0 | 0 | 4 | 0 | 0 | 0 | 5 | 80% |
| tom | 1 | 0 | 0 | 0 | 0 | 0 | 0 | 2 | 0 | 1 | 4 | 50% |
| other | 2 | 1 | 2 | 1 | 0 | 1 | 0 | 0 | 0 | 10 | 17 | 59% |

#### Loops, all hits

6287 hits: 39.0% right, 18.1% confidently wrong, 4820 detections matched no placed hit.
Toms with a pitch: 13/248.

| truth \ guess | kick | snare | clap | closed_hat | open_hat | crash | ride | tom | other | Unknown | n | recall |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| kick | 471 | 4 | 3 | 44 | 7 | 42 | 0 | 117 | 0 | 774 | 1462 | 32% |
| snare | 25 | 304 | 4 | 78 | 2 | 8 | 0 | 7 | 0 | 377 | 805 | 38% |
| clap | 1 | 5 | 43 | 24 | 0 | 3 | 0 | 1 | 0 | 61 | 138 | 31% |
| closed_hat | 295 | 114 | 33 | 1218 | 42 | 8 | 0 | 29 | 0 | 1060 | 2799 | 44% |
| open_hat | 1 | 7 | 1 | 28 | 81 | 0 | 0 | 2 | 0 | 26 | 146 | 55% |
| crash | 0 | 0 | 0 | 3 | 3 | 34 | 0 | 1 | 0 | 26 | 67 | 51% |
| ride | 12 | 27 | 24 | 23 | 6 | 0 | 94 | 12 | 0 | 240 | 438 | 21% |
| tom | 29 | 2 | 4 | 4 | 0 | 0 | 0 | 76 | 0 | 133 | 248 | 31% |
| other | 7 | 8 | 6 | 20 | 2 | 2 | 0 | 5 | 0 | 134 | 184 | 73% |

#### Loops, clean hits only

1054 hits: 45.6% right, 23.3% confidently wrong, 0 detections matched no placed hit.
Toms with a pitch: 0/10.

| truth \ guess | kick | snare | clap | closed_hat | open_hat | crash | ride | tom | other | Unknown | n | recall |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| kick | 236 | 1 | 1 | 10 | 1 | 0 | 0 | 24 | 0 | 154 | 427 | 55% |
| snare | 4 | 109 | 0 | 12 | 0 | 0 | 0 | 0 | 0 | 41 | 166 | 66% |
| clap | 0 | 0 | 6 | 1 | 0 | 0 | 0 | 0 | 0 | 5 | 12 | 50% |
| closed_hat | 116 | 52 | 3 | 113 | 0 | 1 | 0 | 2 | 0 | 121 | 408 | 28% |
| open_hat | 1 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 2 | 3 | 0% |
| crash | 0 | 0 | 0 | 1 | 0 | 4 | 0 | 0 | 0 | 1 | 6 | 67% |
| ride | 3 | 2 | 1 | 0 | 0 | 0 | 0 | 1 | 0 | 1 | 8 | 0% |
| tom | 5 | 1 | 0 | 0 | 0 | 0 | 0 | 2 | 0 | 2 | 10 | 20% |
| other | 0 | 2 | 0 | 1 | 0 | 0 | 0 | 0 | 0 | 11 | 14 | 79% |

