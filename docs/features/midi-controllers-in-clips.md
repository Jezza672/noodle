# MIDI controllers and expressions in clips

Batches 1 and 2 (issue #286) and batches 3 and 4, the piano roll lanes (issue #287), of the roadmap item. Before this a MIDI clip held
notes and nothing else, so a recorded piano part came back without its pedal.
The design as it stands is in
[../architecture/midi-and-recording.md](../architecture/midi-and-recording.md)
("MIDI clips" and "MIDI takes").

## What landed

- **Model** (`noodle-core/src/clip.rs`). `MidiClip::controls`, a list of
  `ControlPoint`s for controllers, pitch bend, channel pressure and program
  changes, and `MidiNote::expressions` (pitch, poly pressure, timbre, volume,
  pan). Values are normalised, ramps are two points with a `ramp` flag. `MidiNote`
  is no longer `Copy`. Old files load with none.
- **Playback.** `ClipFeeds` schedules them beside the notes (`Notes` is now a
  struct of notes and controls), ramps become events every 5 ms, the track
  input chases each controller after a jump or a start and releases what a
  clip set where it ends. A clip's own loop repeats its points and puts each
  pass back. Expressions play only while their note sounds.
- **Editing.** Trim and split keep held controllers and expressions in step
  with the notes (`controls_from`, `expressions_from`); cache keys include
  everything.
- **Recording.** Takes keep controllers, bend, pressure, program changes and
  poly pressure (as an expression of the held note); quantising moves a note's
  expressions with it and leaves controllers where they were played.
- **Example.** `examples/midi-cc.ron`: pedal and mod wheel from a clip into a
  synth group through two `MIDI Control` nodes, with its golden render.

## Tests

- `noodle-core` `clip.rs`: round trip, validation, played controls (cut ramp,
  loop repeats), `controls_from`, `expressions_from`.
- `track_input_midi.rs`: playback and release, chase after a seek (and none
  after the release), ramps at 5 ms, a clip's own loop, expressions.
- `track_input_realtime.rs`: the new scheduling path never touches the
  allocator (mutation-checked: an allocation in `play_controls` fails it).
- `timeline/clips.rs`: split and trim. `midi_take.rs`: kept messages,
  poly pressure, quantise. `session.rs`:
  `a_take_keeps_the_pedal_and_the_mod_wheel_and_plays_them_back` records a
  pedal and wheel, then renders it through the example's synth and hears the
  pedal hold the note (and not without the controllers).

## Piano roll lanes (#287)

- **Controller lanes** (`noodle-app/src/piano_roll/lanes.rs`) under the
  velocity lane: one for each controller, bend or pressure the clip uses, plus
  any added with "+" in the toolbar (common controllers, or any CC number; a
  lane with no points has a × to remove it). Program changes have no lane.
- **Tools:** Draw sets values along the drag (7-bit steps for controllers,
  snapped to the grid, replacing the points it crosses), Line stores a ramp as
  two points, Select picks points by click or box to move (ticks and value),
  Delete, and copy, cut, paste and duplicate (`application/x.noodle.controls`).
  Delete and the clipboard mean points when the pointer is over the lanes or
  no notes are selected.
- **Expression lane**, shown by the toolbar's Expression box with a kind
  chooser: draws and lines on the selected notes only, stored from each note's
  start and cut to the note.
- Every gesture is one `SetClip` undo step.
- Tests: `piano_roll/tests.rs` (pedal draw, line, select/move/copy/delete,
  expression, arithmetic; the line test is mutation-checked).
