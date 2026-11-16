# Drum classifier accuracy: what the literature says

> **Scope correction (2026-11-16, from the user).** The goal is *correctly binning pure samples*:
> decide when a detection is **one hit that finishes ringing before the end of the sample
> window**, so it makes a clean kit sample. We are not splitting hits apart. A double hit must
> **never** land in a single-hit bin; double hits are kept as their own known category, so they
> can be separated later and used when MIDI asks for a double hit. Section 0 reframes the
> findings for that. Sections 1 to 6 are the broader accuracy survey and stay as written;
> where they talk about overall accuracy or detector recall, read them through section 0.

## 0. The problem as the user defined it

Two decisions per detection, with different costs:

1. **Purity:** `clean single` vs `double/multi hit` vs `truncated` (still ringing at the window
   end) vs `mixed` (another instrument under it). Only `clean single` may enter a kit bin.
2. **Type:** kick, snare, hat and so on, which only matters for detections that pass step 1.

The costs are asymmetric. A clean hit wrongly rejected is just a lost sample; a double hit
accepted into a single-hit bin is the failure the user ruled out. So the headline metric is the
**false-accept rate**: of all doubles, truncated and mixed detections, how many end up in a
clean bin. Overall accuracy and the roughly 60% detector recall are secondary: a missed hit
costs nothing here. Existing labels help: `docs/features/drum-classifier.md` already tags hits
as `mixed` or `truncated` for its clean column, so purity labels exist on the synthetic loops
but are only used for scoring, not as a training target. (Inference from that doc; I did not
read the data code.)

What the literature I read says about this specific problem (little; nothing targets it):

- **Second-onset detection is the core signal.** Transcription systems (Vogl 2018, E-GMD, both
  read) find onsets with a learned activation function at 10 ms resolution and peak-pick with a
  low threshold (0.1 to 0.2 in Vogl) because activations are spiky. E-GMD says the 10 ms frames
  were "important for predicting events that repeated rapidly, such as drum rolls". A purity
  gate is that detector pointed at the inside of one candidate window: any second onset after
  the first means "not clean". Use a deliberately *sensitive* threshold, because a false second
  onset only rejects a clean hit, which is cheap.
- **Tail completeness is a separate cue.** Open hats, crashes and rides ring long, so
  "finished ringing before the window end" is an envelope question: energy in the last part of
  the window relative to the peak. Inference: add the decay-tail energy ratio and the
  time-to-fall-below-threshold as features; a short log-mel patch (section 2) carries it too.
  I found no paper measuring this directly.
- **Confusion with human labelling is expected.** Vogl notes class boundaries and hat variants
  are ill-defined even for humans, so type errors will not reach zero. Purity is more objective
  and should be learnable to a high precision.

Recommended approach (inference, untested):

1. **Add purity classes as a training target.** Classes: `clean`, `double` (including flams and
   rolls), `truncated`, `mixed`, `spurious`. Today `spurious` and low-level hits are dropped from
   training, so the model never sees the doubles and junk it will be asked to reject.
2. **Synthesise doubles on purpose.** From the `drums-cc0` one-shots, place two hits (same or
   different instrument, offsets from a few ms, flam range, to most of the window), plus hits
   that ring past the window end. Keep the split rule by file name so a hit and its room version
   never straddle train and test.
3. **Gate with a precision target, not argmax.** Set the clean threshold on validation so the
   false-accept rate is at most the chosen figure (for example under 1%), and report it with a
   confidence interval; with 73 one-shots in the test split, pool more data first.
4. **Route doubles to their own bin rather than discarding them.** Label them `double` (and
   optionally which types) so the drum rack can list them for later separation and for MIDI
   double hits.
5. **Features:** envelope slices and spectral-flux peak count over the window, then the
   log-mel patch CNN if needed (section 2). Pitch/time augmentation helps only modestly and
   can hurt hats (Jacques & Röbel); do not use time-stretching on the purity task, because
   stretching changes ring-out length and double-hit spacing, which are the labels.
6. **Test against failure, as CLAUDE.md asks:** deliberately mix a double hit into the test set
   and confirm the false-accept metric catches it.

**Addendum (user, 09:14): a malformed/incomplete category.** Chopped or otherwise imperfect
samples must be identified as such, not only doubles. Add `malformed` to the purity classes (it
replaces catch-all `spurious` for real-but-bad hits; keep `spurious` for non-hits). Examples to
synthesise and label:

- **Chopped start:** attack cut off or missing the transient (window begins mid-hit).
- **Chopped end:** ring-out cut by the window or by the next hit (`truncated`, already present).
- **Clipped/distorted:** hard-clipped peaks, or a click from a cut at non-zero crossing.
- **Gated or faded:** abrupt gate, fade-in or fade-out from a loop edit.
- **Noisy or bled:** background music or another instrument under the hit (`mixed`).
- **Silence-padded or off-centre:** hit sits so far into the window the lead-in is empty.

How to make it learnable (inference, untested): the same synthetic generator can apply these
damages to clean one-shots, so ground truth is exact. The cues are mostly envelope and
waveform ones, which the averaged spectrum cannot see: the first few ms (attack present, no
leading click), the last few ms (does it end at near-zero or abruptly), peak-to-clip ratio, and
tail energy. A patch CNN over the waveform envelope plus the log-mel patch covers them. Keep the
gate as above: a sample is accepted only when `clean` wins with high confidence, so any doubt
falls into `malformed`, which is the safe error. Report false-accept rate per damage type, so
a weak spot such as a short fade-out shows up rather than averaging away.

Open question for the user: how long is the sample window, and does a hit that is clean but
quiet below the existing -45 dB cut count as clean or dropped?

Written 2026-11-16 from the user's Mac. Research only; no code changed.
Context: `docs/features/drum-classifier.md` (PR 575): 24-band averaged spectrum features,
small PyTorch MLP exported to ONNX, 73% one-shot and 39% loop-hit accuracy, validation accuracy
stuck near 62% for every network size, hit detector finds about 60% of placed hits.

This replaces an earlier cloud draft that could not read papers in full. Everything below marked
"read" was downloaded and read as text on this machine. Anything else is labelled.

## How to read this

- **Read (full text):** Vogl et al. 2018; Callender et al. 2020 (E-GMD); Jacques & Röbel 2019;
  PANNs (Kong et al.); AST; EfficientAT; BEATs (text extracted, searched for the facts used);
  SampleMatch; SpecAugment and mixup (text extracted, only the headline claims used).
- **Read only in part:** ADTOF (Zehren et al. 2021). The PDF text extraction gave me the abstract,
  introduction, related work and references, not sections 3 to 7. I did not read its results.
  Vogl 2018: I read the method, experiment design and discussion prose, not the numeric tables.
- **Abstract or search snippet only:** Schlüter & Böck 2014 (onset CNN); the hip-hop wav2vec2
  Hugging Face model and a few small GitHub drum-classifier projects (accuracy figures are
  self-reported, on different class sets and splits, and I did not verify them).
- **Not read at all:** Wu et al. 2018 review, Cartwright & Bello 2018 (SDDS), MT3, Böck's
  onset-detection papers, CLAP, MERT. Nothing below relies on them except where a paper I read
  describes them, and that is said each time.
- **Licences:** I read GitHub pages, not the checkpoint files. Where a checkpoint licence is not
  stated on what I read, I say "not stated". Confirm before shipping anything.

## 1. State of the art, and why it does not transfer directly

The literature is mostly **drum transcription in polyphonic music**: a network turns a
spectrogram into per-instrument onset activations, then peak-picks. One-shot classification
barely appears as a research benchmark. The only things I found were small self-reported
projects and SampleMatch.

- **Vogl, Widmer, Knees 2018** ([arXiv 1806.06676](https://arxiv.org/abs/1806.06676), read).
  CNN and CRNN, 3, 8 and 18 classes. Input: log-magnitude spectrogram, 2048 window at 44.1 kHz,
  10 ms hop, 12 triangular bins per octave over 20 Hz to 20 kHz (84 bins), plus the positive
  first-order difference over time stacked on top (168 values per frame). Peak picking with
  thresholds of 0.1 to 0.2 because the activations are spiky. Findings I read:
  - CRNN beat CNN consistently as classes grew.
  - A large synthetic dataset (4,197 tracks, about 259 h) generalised "surprisingly well" to real
    data; some under-represented classes (toms, ride, crash variants) improved.
  - Balanced synthetic sets scored worse overall than unbalanced ones (harder), and helped rare
    classes while hurting common ones.
  - Confusions they list: bass drum vs low tom, closed vs pedal hi-hat, high tom vs bass drum,
    clap vs snare, low/mid/high toms, ride vs ride bell. They argue the class boundaries
    themselves are ill-defined, so some of these errors are human-level.
- **ADTOF** ([arXiv 2111.11737](https://arxiv.org/abs/2111.11737), intro and related work read
  only). Says synthetic-only training does not beat small real datasets, and that results
  improve only when real data is involved (mixed, or pre-train then fine-tune). Says
  pitch-shift and time-stretch augmentation helped but with limited gain (citing Vogl). Their
  own data is 114 h of crowd-sourced charts with 5 classes.
- **Callender, Hawthorne, Engel 2020, E-GMD** ([arXiv 2004.00188](https://arxiv.org/abs/2004.00188),
  read). 444 h, 43 kits, CC BY 4.0, recorded at 44.1 kHz. They raised the sample rate from 16 to
  44.1 kHz and say it "improved the model's ability to process events with high-frequency
  content like cymbal crashes". They also argue accuracy and F-measure are poor proxies for how
  good a transcription sounds downstream.

Takeaway: the field's best numbers come from **large, kit-diverse data and time-frequency
inputs**, not from clever classifier heads on a compressed feature. Noodle's training set is
one CC BY pack plus synthetic loops, so kit diversity is the closest analogue to what limits it.

## 2. Input features

What I read: every system above feeds the network a **time-frequency patch** (tens to hundreds
of frames at 10 ms, 84 to 250 mel or log-frequency bins), not a time-averaged spectrum.
Vogl adds the positive spectral difference, which emphasises onsets and decays. E-GMD uses
250 mel bins at 44.1 kHz with 10 ms frames.

Inference (mine, not from a paper): Noodle's 24-band averaged spectrum discards the temporal
envelope. Attack shape and decay length are what separate closed from open hat, ride from hat,
and kick from low tom, which matches the weak spots in the report. It also matches the observation
that bigger MLPs plateaued at about 62%: the information is not in the input. Candidate inputs,
cheapest first:

1. Keep the 24-band vector and **append envelope features**: per-band energy in a few time
   slices after the onset (for example 0 to 10, 10 to 30, 30 to 100, 100 to 300 ms), plus a
   spectral-flux/positive-difference summary.
2. A **short log-mel patch** around the hit, for example 64 mel bins by 20 to 40 frames at
   5 to 10 ms, with the positive difference as a second channel, into a small 2-D CNN. Still a
   few hundred thousand parameters and plain ONNX ops.
3. Keep sample rate at 44.1 kHz or the app's rate; E-GMD's note on cymbals argues against
   dropping to 16 kHz.

## 3. Pretrained encoder front ends

All of these were trained on AudioSet (10 s clips, 527 or 521 event classes), so they are
generalist and untested on one-shots in anything I read. They are an option for a frozen
embedding plus a small head, or fine-tuning.

| Model | Params | Rate / input | AudioSet mAP | Code licence | Weights licence |
|---|---|---|---|---|---|
| PANNs CNN14 ([paper](https://arxiv.org/abs/1912.10211), read) | 80.8 M | 32 kHz, 64 mel, win 1024, hop 320 (a 16 kHz variant also exists) | 0.431 | MIT (repo page) | Not stated; hosted on Zenodo |
| PANNs MobileNetV2 | about 4.1 M | same | 0.383 | MIT | Not stated |
| EfficientAT mn10 ([paper](https://arxiv.org/abs/2211.04772), read) | 4.88 M | 32 kHz, 128 mel, 10 ms hop | 0.470 in the paper table (47.1 in the repo README) | MIT (repo page) | Not stated; the Releases section was empty when I looked |
| EfficientAT mn04 | 0.983 M | same | 43.2 (README) | MIT | Not stated |
| AST ([paper](https://arxiv.org/abs/2104.01778), read) | 87 M | 128 mel patches, ImageNet-initialised ViT | 0.459 single, 0.485 ensemble | not checked | not checked |
| BEATs ([paper](https://arxiv.org/abs/2212.09058), read) | 90 M | README examples use "16khz" input | 48.6 single (paper abstract area) | repo root is MIT, but the BEATs README only points to "the LICENSE file" and `beats/LICENSE` returned 404 | Unresolved; do not assume |
| YAMNet (README read) | 3.7 M | 16 kHz, 96x64 patches (0.96 s), 1024-d embedding | n/a | not stated in what I read | not stated |
| OpenL3 (README read) | n/a | not stated on the page | n/a | MIT | **CC BY 4.0** (stated) |

Notes:

- PANNs paper compares three transfer strategies: train from scratch, frozen embedding plus
  classifier, and full fine-tune. Useful template for an experiment.
- Size: only MobileNet-class models (EfficientAT mn04/mn10, PANNs MobileNetV2, YAMNet) are
  plausibly appropriate for an in-app ONNX file. The 80 to 90 M models are not, and I did not
  find anything about running them on one-shot drum material.
- Input mismatch: 10 s clips at 32 kHz. A hit is 50 to 500 ms, so it would be zero-padded;
  how well AudioSet embeddings separate hat variants is **unknown** from anything I read.
- Licence status is the blocker for publishing on Hugging Face. OpenL3 is the only one where the
  weights licence is stated (CC BY 4.0, attribution needed). For the others, ask the authors or
  check the actual release assets before relying on them.
- Recommendation: treat a frozen mn04 or mn10 embedding plus a small head as a **cheap upper-bound
  probe** (a day of work, same dataset, same test split). If it does not clearly beat a patch-CNN
  on the existing features, drop the idea and avoid the licence work.

## 4. Augmentation and training

- **Shuffled mixup** (E-GMD, read). Their ablation, F-measure on E-GMD test / IDMT / ENST:
  shuffled mixup 83.40 / 85.72 / 76.89; plain mixup 69.11 / 47.44 / 62.27; none 63.07 / 52.74 /
  67.35. Plain mixup helped in-domain but mostly not cross-dataset; shuffling chunks made the
  train/eval curves stop diverging. Noodle already builds synthetic loops, so mixing two loops
  at the audio level before analysis is the same idea. Cost: the labels still come from placed
  hits, which stay valid under audio mixing.
- **Smaller network generalises better across kits** (E-GMD, read): cutting conv filters from
  32/32/64 to 16/16/32 and the dense layer from 512 to 256 stopped overfitting to the training
  kits. Consistent with Noodle's finding that capacity did not help.
- **Augmentation is instrument-specific** (Jacques & Röbel,
  [arXiv 1903.01416](https://arxiv.org/abs/1903.01416), read). Every strategy helped at least
  somewhere; combining all of them was usually worse than the best single one. Transposition
  without time compensation gave the best results for bass drum, while transposition up to
  300 cents hurt hi-hat (higher frequency range, bigger offset). Gaussian noise on the input
  spectrogram was the best strategy for hi-hat (std 0.2). They conclude extents must be tuned
  per instrument. Apply: small pitch shifts for kick/snare/toms only, noise and gain for hats.
- **Vogl and ADTOF** (above): pitch/time augmentation helps but modestly; real data in the
  mix matters more than more synthetic data. Eval songs are not training data, so "real data"
  has to come from other licensed kits and recordings.
- **SpecAugment** ([arXiv 1904.08779](https://arxiv.org/abs/1904.08779), abstract and method
  read) is validated on speech recognition only. Time and frequency masking is cheap, but I
  found no drum evidence for it; try it as an ablation, not an assumption.
- **mixup** ([arXiv 1710.09412](https://arxiv.org/abs/1710.09412)) is generic (images, speech
  commands); the drum-specific evidence is the E-GMD ablation above.
- **Class balance**: Vogl found balancing helps rare classes and hurts common ones. For a
  rare-class problem like crash/ride/open hat here, class-weighted loss or oversampling is the
  lighter-touch version of what they did by resynthesis.
- **Metrics**: with only 73 one-shots in the test split, a 10-point change is within noise. Add
  confidence intervals or pool the split, and say which kit each test hit comes from.

## 5. The hit detector's roughly 60% recall

Notes from papers I read, plus inference:

- The transcription systems above do not run a separate detector then classify. They learn
  **per-class onset activations** from the spectrogram (CNN/CRNN) and peak-pick them (Vogl:
  window 2 frames, threshold 0.1 to 0.2 chosen on validation). Detection and class are one
  decision, so a quiet hat under a snare is found because the network learned that pattern.
- Schlüter & Böck 2014 (abstract only, not the body): CNN onset detection beat the then
  state of the art on about 100 min with 26k onsets, and they report separate detectors for
  percussive and harmonic onsets help. Supports a learned detector over a threshold-based one,
  but I did not read the numbers.
- Noodle-specific inference. The synthetic loops have overlap and bleed on purpose, and about
  4,820 of the 6,287 detections match no placed hit. Two cheap steps before replacing the
  detector:
  1. **Train a `spurious` class** instead of dropping those detections. Right now the model never
     sees the false detections it will be handed in the app, so it cannot learn to reject them.
  2. Measure recall against placed hits by level and by class so the 60% can be split into
     "masked under louder hits" and "below the detector threshold". The first is a physics limit
     of loops; the second is tunable.
- The bigger step is a small learned activation network on the patch input from section 2, used
  as the detector. That is a larger piece of work and should wait until the classifier probe
  results are in.

## 6. Recommended order (inference, my judgement)

1. **Features:** add envelope slices to the existing vector, then try the log-mel patch CNN.
   Expected to address the closed hat / ride / tom weakness. Cheapest and most likely to move
   the 62% ceiling.
2. **Add the `spurious` class** and train on detections the model will actually see.
3. **Augmentation:** loop-level mixup, per-class transposition, hat noise. Ablate each.
4. **More kits:** the limit is probably kit diversity (E-GMD uses 43 kits, MuldjordKit is one).
   Only add CC-licensed or user-owned kits, keep the manifest and credits.
5. **Pretrained probe:** frozen EfficientAT mn04/mn10 or YAMNet embedding. Only pursue if it wins,
   and only after the checkpoint licence is confirmed in writing.

Portability: patch-CNN and MLP heads export to standard ONNX ops; the MobileNet encoders do too
but add a 1 to 5 M parameter file and a licence obligation.

## What I could not do

- Read ADTOF beyond the introduction, or any numeric table in Vogl 2018.
- Verify any of the third-party one-shot classifier accuracy figures (87% to 94%). They come
  from search snippets of small projects, so they are not in the table above and should not be
  treated as benchmarks.
- Confirm weights licences for PANNs, EfficientAT, BEATs, YAMNet, AST.
- Measure anything. No experiments were run; the recommendations are untested.
