# Onset sensitivity, slicing, transient quantise and audio to MIDI

Built for #356 (Slice), #347 (quantise transients across a link) and #352 (audio
to MIDI). The three share one onset detector with one sensitivity scale.

- **Sensitivity.** `noodle_io::detect_onsets_with(mono, rate, sensitivity)`
  (also `detect_bass_onsets_with`, `detect_treble_onsets_with`), sensitivity 0 to
  1; `rise_db` maps it to the jump a block must make (14 dB at 0, the old 5 dB at
  0.5 = `DEFAULT_SENSITIVITY`, under 2 dB at 1). `detect_onsets` is the default,
  so the warp engine and the golden renders are unchanged.
- **Slice node** (`noodle.instrument.slice`, `crates/noodle-nodes/src/slice.rs`):
  a sample in, a sample set out, slice `i` on key `first_key + i`. Config:
  `sensitivity` (0 to 100), `first_key`, `edits` (`+0.25,-0.5`: cuts added or
  removed by hand as fractions of the sample, surviving a change of
  sensitivity). It is a sample wire node that derives its output from its input:
  `NodeType::sample_through` / `sample_derived` (the compiler resolves the input
  first, so chains work). Cuts come from `SampleStore::onsets`, cached by sample
  window and sensitivity. A hit within 100 ms of the start is where the first
  slice starts.
- **Slice to Sampler** (clip menu): a new track with Sample, Slice and a Sampler
  (keytrack off) fed by the track's MIDI. One undo step.
- **Pitch tracking** (`noodle-io/src/pitch.rs`): `PitchTracker` (YIN on audio
  reduced to about 8 kHz, a frame every 10 ms, no allocation), `NoteSegmenter`
  (steady pitch becomes a note, a rise in level or a move of a semitone restarts
  it), and `transcribe` (both over a recording, cut also at `detect_onsets_with`).
- **Pitch Track** (`noodle.analysis.pitch_track`: `pitch`, `confidence`, `level`
  out) and **To Notes** (`noodle.event.to_notes`: those three in, notes out), for
  live use. Both have a `sensitivity` parameter.
- **Convert to MIDI** (clip menu, Fewer / Normal / More notes): destructive.
  Notes go on a new track that plays them with the Mono Synth; with a MIDI clip
  selected too, it replaces that clip's notes instead (one undo step).
- **Pin transients** (clip menu and the clip panel, with a sensitivity):
  `ClipWarp::pin_transients` pins a marker on the hits of every clip in a link
  where they play now; hits within `PIN_MERGE` (5 ms) are one, the earliest
  standing for them.
- **Quantise regions** (`WarpQuantise::regions`, edited in the clip panel):
  stretches of the clip with their own grid and amount (triplets for a fill,
  amount 0 to leave a bar alone). The groove is not applied inside one.
- **Slices warp mode** (`WarpMode::Slices`, `Stretcher::slices`): cut at the
  warp's markers, each slice at natural speed moved to where the map puts it, a
  gap left to ring out then silent, overlaps crossfaded. Every clip sharing the
  warp is cut at the same places, so linked mics stay in phase.

Not built: the **To Sample** offline node (track, Reverse, To Sample, Slice,
Sampler staying live), which needs a render, then recompile cycle; and drawing
quantise regions and cut lines on the editors (regions are edited as rows in the
clip panel, cut edits through the Slice node's `edits` config).
