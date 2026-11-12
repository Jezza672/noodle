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
- **Convert to sample node and Convert to sampler (#356)**
  (`session/to_sample.rs`; the gestures are in `clip_editor/`):
  - *Region of a clip.* Shift+drag on the waveform (warped or not) picks a
    region, drawn as a band, in seconds of the file. Right-click inside it,
    Convert to sample node: `Edit::RegionToSample` adds a Sample node at the top
    level, below everything, on the file's own audio with the selection as its
    start and end (the `hash:start:end` form), so nothing is rendered.
  - *Region of the arrangement.* With a range selected (Start selection here /
    End selection here), right-click empty lane space or the ruler's Markers and
    selection menu, Convert to sample node: `Edit::BounceToSample` renders the
    mix over the range (`ClipRequest::group` is `None`: what the Output nodes
    play, through `render_project_streaming_replacing`, with the effects' tail up
    to ten seconds) to a file in the renders folder and, when the render is
    done, adds a Sample node on it, one undo step. The project has to be saved.
  - *Transients of a clip.* In the warped editor click a hit's handle to pick it
    (Shift or Ctrl adds or takes out; a click elsewhere lets go). Right-click,
    Convert to sampler: `Edit::CutsToSampler` makes a track whose group
    `Sliced Sampler` is a Sample Set into a Sampler (keytrack off, one-shot),
    fed by the track's MIDI. The group's inputs are `notes`, `tune`,
    `bend range`, `release` and `gain`. Each stretch from one picked hit to the
    next is an entry of the set, on keys from C2 (36), up to 92. One picked hit
    is cut at the next detected hit, or the end of the lit part. One undo step.
    Unlike Slice to Sampler, the cuts are the ones picked, so changing them is
    picking again and converting again.
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
- **Quantise transients (#430)** (clip menu, a submenu, and a section of the clip
  panel): `Edit::QuantiseTransients` pins the hits that have no marker yet, turns
  warp quantise on at a grid and a strength (0 to 100 %), and sets how the slices
  join, as one undo step (`Session::quantise_transients`). The join is a choice
  of warp mode: **Crossfade slices** is Slices (each slice at its natural speed
  where its hit now is, overlaps crossfaded, a gap after a slice that moved
  early left silent once the slice has run on), **Fill gaps by stretching** is
  Beats (the audio between two hits stretched or squeezed to fill the space), or
  leave the mode. Nothing in the sound code is new; the strength stays editable
  as the panel's Amount. The markers are drawn on the clip in the arrangement
  (`timeline/pins.rs`: a line and a flag where each pinned moment plays now, so
  with quantise on they sit at the moved places).
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
