# Deferred items, first batch

Small items that had been left on the roadmap, built together.

- **Model controls for Split to stems.** The Models tab of Settings has a
  *Split to stems* section under the drum models: an enable checkbox, Download
  (ONNX Runtime first when the machine has none, then the weights, with a
  progress bar and Cancel), Remove, size on disk, licence and credit, and where
  the files are kept. `noodle_stems::ModelSpec::all()` lists the models and
  `ModelSpec::info()` describes them; `ModelStore::remove` and `size_on_disk`
  are the store's side. Only HT-Demucs exists today. A model is on unless the
  user switched it off (`Prefs::stem_models_off`, per machine, not exported). The
  clip panel has a **Stem model** drop-down beside Stem quality
  (`Edit::SetStemsModel`, session state like the quality), listing the models
  that are switched on; splitting with one that is off says where to turn it on.
- **Beat view gain envelope.** The warped (beat) view of the clip editor draws
  the gain line and its handles (`clip_editor::beat_gain`, over the same
  `gain_line` as the plain view): a clip's points are ticks from its start, which
  are beats on the axis, and a Sampler's are millionths of the sample. Drag a
  handle to move it, double-click it to remove it, **Cmd or Ctrl and
  double-click** the waveform to add a point (a plain double-click is still a
  marker). `testkit::double_click_with` holds modifiers.
- **One `TimeAxis`** (`time_axis.rs`): the line from a unit of time (seconds,
  quarters, beats) to the screen, and `zoom_about`, zooming about the pointer.
  The arrangement, piano roll and clip editor (plain and beat view) all use it
  for x, the inverse and Ctrl-scroll or pinch zoom, instead of four copies of
  the arithmetic. What stays with each view is how its own tick or beat becomes
  the unit (a tempo map, 960 ticks to the quarter, a warp).
- **The engine toggle is remembered.** The toolbar's Audio toggle is saved
  (`Prefs::engine_on`, per machine), and the app starts the engine at launch only
  if it was on.
- **A saved device that comes back.** While the engine runs on a fallback (the
  system default or the virtual output), `Session::watch_device` lists the
  devices on a background job every five seconds and, once the saved output is
  there, reopens the engine on it, keeping the transport. It never runs while
  recording, and it does nothing when the virtual output was the choice.

Tests: `time_axis.rs`; `the_beat_view_draws_the_gain_line_and_takes_points_by_cmd_double_click`;
`a_switched_off_model_is_not_offered_or_used_until_it_is_on_again`;
`a_stem_model_is_on_by_default_and_can_be_downloaded_and_removed`;
`the_engine_toggle_is_remembered_and_stays_on_this_machine`; the device watch tests in
`session.rs`.
