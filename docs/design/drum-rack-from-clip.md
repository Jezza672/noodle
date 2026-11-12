# Create drum rack from clip (design, proposed)

Status: proposed, nothing built. Asked for by Jeremy; open questions are at the
end. This builds on the Drum Rack of #457 (named `Drum Pad` groups, ADR 0009),
the onset detector and Sample Set work of #489 (#356), the Selector (#388) and
the stems work (M10, ADR 0006).

## What the user does

1. Right-click an audio clip, **Create drum rack from clip…**.
2. A dialog opens with a progress bar (analysing), and a Cancel.
3. When it finishes, the dialog shows **bins** (groups of similar hits) on one
   side and a **pad grid** on the other. Each bin has a best-guess label
   (Kick, Snare, Closed hat…) and is already dropped on the pad it names.
4. The user drags bins onto pads, to fix guesses, to fill pads the guess left
   empty, or to put one bin on several pads. A bin with several members has a
   **picker** to choose which hit that pad plays.
5. **Create** makes one new track holding the rack, in one undo step. Cancel
   changes nothing.

The grid starts as the eight pads of the Drum Rack template (kick, snare,
closed hat, open hat, clap, low tom, high tom, crash). **Show all keys** widens
it to the 128 MIDI keys, each labelled with its General MIDI drum name where it
has one (35 to 81), and the note name where not.

## Pipeline (all offline, none of it on the audio thread)

One module, `noodle-io/src/drum_bins.rs`, pure functions over `&[f32]`, so it is
testable without a session, like `onsets.rs` and `pitch.rs`.

1. **Decode** the clip's window (the source file's audio, not the warped
   playback: the samples should be the original recording), mono for analysis,
   the channels kept for cutting.
2. **Find hits.** `detect_onsets_with` at a higher sensitivity than the warp's,
   merged with `detect_bass_onsets_with` and `detect_treble_onsets_with`, so a
   hat on top of a kick is its own onset. Hits within `PIN_MERGE` (5 ms) are one.
3. **Cut each hit** from 2 ms before the onset to the earliest of: the next
   hit, the point the envelope falls 50 dB, and a cap (1.5 s). A hit cut short
   by the next one is **truncated**; truncation is recorded, since a ringing
   crash cut at the next hat is a poor sample.
4. **Describe each hit** by a fixed-length vector from its first ~80 ms,
   level-normalised, so a soft and a hard snare are near each other: a log-mel
   spectrum summary (about 24 bands), spectral centroid and flatness, the
   energy split below 150 Hz, 150 Hz to 5 kHz and above 5 kHz, the decay time
   (to −30 dB), and a pitched-ness figure from the existing fundamental
   detector (toms are pitched, snares are not). Level is kept beside it, not in it.
5. **Bin.** Greedy leader clustering in order of hit, then one pass of
   reassigning each hit to its nearest bin's medoid: a hit joins the closest
   bin if within the threshold, else starts a bin. The threshold is the one
   knob, shown in the dialog as a **Merge more / Split more** slider. Features
   are kept, so moving it re-bins in milliseconds with no re-analysis. Bins with
   one member and a very quiet or truncated hit are shown collapsed as
   "Other".
6. **Rank the members of a bin** so the first is the best default: not
   truncated, isolated (no other onset within its window, which is the cheap
   test for "not mixed with another drum"), closest to the medoid, and loud
   enough to be well above the noise. The picker lists them in this order.
7. **Guess the label** per bin with rules over the medoid's features: low-heavy
   and short is a kick; broadband, a body near 150 to 250 Hz and noisy, a snare;
   bright and short, a closed hat; bright and long (decay beyond ~250 ms), an
   open hat; very long and bright, a crash or ride; pitched with a clear
   fundamental, a tom (low or high from the fundamental); noisy mid-band with a
   multi-peaked attack, a clap. Each guess has a confidence; under it the bin is
   labelled "Unknown" and left unassigned.

The result is cached by clip window, file hash and detector version
(like `SampleStore::onsets`), so reopening the dialog is instant.

## Where the work runs, and the dialog's state

A `Job` like Split to stems: one progress bar in the status bar with Cancel, the
result held by the session (not the project) until the dialog is closed.
`Session::analyse_drum_clip(clip)` starts it; the dialog (`drum_rack_dialog.rs`,
next to `stems_dialog.rs` and `sample_set_dialog.rs`) holds the user's choices:
`Vec<Bin>` (members as `SampleRef` windows, label guess, confidence), the slider,
and `Vec<(key, bin, pick)>` for the assignments.

## What it creates (the design rule)

The rack is a group of primitives with exposed controls and no new synth node.

- **Command.** `Edit::CreateDrumRack(clip, Vec<PadAssignment>)`, run like
  `cuts_to_sampler` (`session/to_sample.rs`): build on a scratch copy of the
  project, then push one `Command::Batch`. Nothing mutates the graph directly,
  so undo is one step. New IDs come from `Project::new_node_id`.
- **Shape.** A new track (`add_track::with_input`) named `<clip> (drum rack)`
  whose group is the existing **Drum Rack template**, with one `Drum Pad` per
  used key. A pad for an unlisted key is the same `Drum Pad` group, named by
  its GM name, with its `key` input set. Pads the user left empty are not
  added. The swing and choke wires of the template stay (the hats still choke).
- **Samples in a pad.** Inside each pad, a **Sample Set** node feeds the
  Sampler: one entry per member of the bin, all on the pad's key as root (so the
  note is not repitched), each its own round-robin slot. Entries are
  `hash:start:end` windows of the one source file (the form `Convert to sampler`
  already uses), so **nothing is rendered or copied**. A bin dropped on two pads
  gives two Sample Sets with the same entries (the entries are hashes and
  numbers, so the duplication is cheap). Nothing links them afterwards; see the
  questions.
- **Which copy plays** is a Sampler parameter, so it can be changed, automated
  or wired: a new `rr_mode` choice **Fixed** and an `rr_pick` parameter
  (1 to N, the slot, clamped), exposed on the pad as a `take` input. The same
  Sampler's existing **Cycle** and **Random / No repeat** modes then give a
  humanised kit for free, from real hits of the same drum. This is a config-vs-
  parameter match: the choice changes no ports, so it is a parameter.
  The alternative is a **Selector** (kind samples, `which`) over N Sample nodes:
  no Sampler change, but a bin of 40 hats is 40 nodes in the pad, and the
  graph is what the user reads. Recommended: Fixed mode.
- **The MIDI.** Offered as a checkbox, on by default: a MIDI clip on the new
  track with a note for each hit, on its bin's key, at the hit's position, with
  velocity from its level relative to the bin's loudest; the source clip is
  muted (`AudioClip::muted`, as Split to stems does). The user then has the
  groove as a pattern on a rack of its own sounds. This reuses audio to MIDI's
  placement of notes on the tempo map; see questions.

Not built here, and why: no live link between the rack and the source clip (re-
analysis is making another rack, as picking cuts again is converting again for
Convert to sampler); no new node types.

## Why not the takes mechanism

Take folders (`takes-and-punch.md`) are windows into one file played over time by
a comp of spans: they choose which pass plays *where on the timeline*. Here the
choice is which hit a pad plays *at every note*, which is what a Sampler's
round-robin slots already are. What is worth borrowing is the interface: the
picker is one row of lanes (waveform per member, click to audition, tint on the
chosen), like the take lanes, so the idiom is the same to the user.

## The dialog's parts

- **Bin card:** label guess and confidence, member count, the medoid's waveform
  and a small spectrogram, **Play**, and the member picker (previous/next, or a
  grid of the members with truncated and mixed ones marked). **Exclude** removes
  a hit from the bin (a wrong one); **Split** and **Merge with…** move a bin
  against the slider.
- **Pad grid:** drop target for bins, drop on an occupied pad replaces, a pad
  shows its bin's name and a mini waveform, double-click empties it. Auditioning
  a pad plays its chosen hit.
- Audition plays through a copy of the project fed to the engine, as take
  audition does, so the project is untouched.

## Hits that are more than one drum

Version 1 detects and avoids, without a model: a hit is **mixed** if another
onset is inside its window (a hat under a kick), or its features are far from
every bin's medoid in a way a sum of two medoids explains better (a test of the
spectrum against the sum of two medoids, nearest to a kick plus a hat). Mixed
hits are listed but never the default pick and are not included in Cycle or
Random unless the user adds them. Two ways to do better, in order of cost, are
under the research below.

## Research: open models

Network note: `huggingface.co` is not reachable from the cloud session, so the
model cards could not be read. The licence statements below come from search
results and a GitHub page, not the cards; **each must be read before adoption.**

**What we already have.** `noodle-stems` runs HT-Demucs (MIT, 4 stems: drums,
bass, vocals, other) through ONNX Runtime, loaded at runtime, with a download
that checks a SHA-256 and a dialog that names it. A drum-part model of the same
signature class would drop in behind that `Separator` idea. If the clip is a
full mix rather than a drum track, **Split to stems already gives the drums**:
the new feature takes the drums stem as input.

**Drum-part separation (kick/snare/toms/hat/cymbals from a drum track).**

| Model | Parts | Licence as found | Notes |
|---|---|---|---|
| [drumsep, inagoy](https://github.com/inagoy/drumsep) (Hybrid Demucs fine-tune, from a 2022 thesis) | kick, snare, cymbals, toms (no hat on its own) | Repository MIT; the page states no licence for the weights, or the training data | the original; an [ONNX conversion](https://huggingface.co/splitzo/drumsep/blob/main/README.md) is labelled MIT by its uploader |
| [LarsNet / LARS](https://github.com/EdoardoMor/LARS) (trained on StemGMD) | kick, snare, toms, hi-hat, cymbals | Code MIT; **weights CC BY-NC 4.0** | best fit by parts; non-commercial weights do not suit a GPLv3 project that may ship |
| [MDX23C DrumSep](https://pypi.org/project/mdxnet-infer/) community checkpoints | 6: kick, snare, toms, hi-hat, ride, crash | Code MIT; checkpoints' terms unsettled | most parts; provenance unclear |
| [Timbrica](https://huggingface.co/Faridzar/timbrica-drum-separator) ONNX fp16 | kick, snare, toms (hat and cymbals as the remainder) | uploader's own notes only | claims synthetic training data and LarsNet's architecture; new, unknown quality |

None has a clearly permissive licence on weights and data together. Training
data that is openly licensed exists: [E-GMD](https://research.google/pubs/improving-perceptual-quality-of-drum-transcription-with-the-expanded-groove-midi-dataset/)
(Creative Commons, with a pretrained transcription model) and StemGMD
(isolated drum stems, on Zenodo, [licence not confirmed](https://arxiv.org/html/2312.09663v1)),
so training our own LarsNet-style model on permissive data is possible but is a
project of its own.

**One-shot classification (what drum is this sample).**
[DLDC](https://github.com/alecames/DLDC) (GPLv3, kick/snare/hat/tom/cymbal),
a [wav2vec2 classifier](https://huggingface.co/yojul/wav2vec2-base-one-shot-hip-hop-drums-clf)
(Apache-2.0, seven classes with open hat, clap and 808, but no tom, and a 95 M
parameter model for a job a small one does), and several hobby CNNs of unknown
licence. The training sets are sample-pack collections whose redistribution
rights are not stated.

**Automatic drum transcription** (ADTOF, E-GMD models) finds *when* a drum
plays, not clean audio, so it does not replace the onset pipeline. It could
raise the quality of the hit list later; it does not help bin or label hits.

### Recommendation

1. **Version 1: no model.** Onsets, features, leader clustering, rule labels.
   It runs in seconds, needs no download, has no licence risk, and the user's
   drag corrects the guesses, which is the point of showing bins at all.
   Where it fails (labels on unusual kits), the dialog is still usable.
2. **Version 2: a small classifier of our own** on the same feature vector (a
   small MLP, tens of kilobytes, compiled in; no `ort`), trained by a script in
   `noodle-eval` on data we may use (E-GMD-derived stems cut at hits, plus
   hits synthesised from the app's own nodes), with the licence of every piece
   of data recorded. Replaces the rules for labels only; binning stays as it is.
3. **Version 3: separation as an optional analyser**, for the "more than one
   drum at once" ask. Run a drum-part separator (a download after asking, like
   the Demucs weights, behind a trait generalised from `Separator` to N named
   parts) over the clip, then per hit compare the energy in each part: a hit
   with two active parts is mixed. A mixed hit is **not** replaced by a
   separated one by default: separator output has artefacts a clean original
   does not. The user can choose to take the part's audio for a mixed hit
   when no clean one exists. Pick the model then, after checking licences,
   against actual drum tracks; today the cleanest choice for quality is
   LarsNet, whose weights bar commercial use, so it can only be an opt-in
   download of the user's own choosing, never shipped.
4. **A model-free step before version 3:** use the bins' medoids as fixed
   templates (partially-fixed NMF / template matching) to explain a mixed hit as
   the sum of known hits. It also answers "is this a kick plus a hat?" with
   what the user already has, and needs no download.

## Tests

- **Binning** (`drum_bins.rs`): clips made from the app's own nodes, with a
  known number of drums and hits (kick, snare, hat in a pattern, some with
  level variation and noise): the right bin count, each hit in the right bin,
  at the default threshold and after the slider; a clip with a hat over every
  kick; a silent clip; one hit; a very long clip stopped by the cancel.
  Prove they can fail: loosen the threshold, break the feature scaling, then
  restore.
- **Labels:** each of the synthetic kit's drums gets its label; an odd sound is
  "Unknown".
- **The command:** `session` test, one undo step, the pads, the keys, the Sample
  Set entries and the MIDI notes, in the style of
  `picked_cuts_become_a_sampler_group_of_primitives_…`.
- **Sampler Fixed mode:** unit test in `sampler.rs`; added to
  `noodle-nodes/tests/realtime.rs`, since it is read on the audio thread (a table
  lookup, nothing allocated).
- **Story route:** extend S09 (the drum pattern story) or add a route that turns
  a drum clip into a rack, through the `testkit` helpers; update the route in
  the same PR (`docs/architecture/usability.md`).
- Listening check on the Mac (a Remote Control session) for the rack's sound.

## Build order (each a PR)

1. `drum_bins.rs` with hits, features, bins, rank and labels, plus the tests.
2. Sampler **Fixed** mode and `take` on the pad; golden renders are unaffected
   (no example uses it), so no `DSP_GENERATION` bump.
3. `Edit::CreateDrumRack`, the command and the session test.
4. The dialog, the clip menu entry and the route.

## Open questions

1. Should Create also make the MIDI clip and mute the source? (Recommended yes,
   as a checkbox.)
2. Is a rack per bin-per-pad (duplicated Sample Sets) acceptable, or should a
   bin be one shared node that several pads read, edited once?
3. Velocity: should soft and hard hits of one drum become velocity layers of a
   pad (the Sample Set already has layers) instead of one bin?
4. Input: drum-only tracks first, with full mixes going through Split to stems
   first, or should the dialog offer to do that itself?
5. Licences: is an optional download of non-commercial weights, never shipped,
   acceptable for version 3, or only permissively licensed weights?
