# Selector

PR #388. A deterministic N-way selector for audio and control signals, notes,
samples and spectra: one output routed from a chosen input by a parameter,
where Random Select picks at random on a trigger.

- **The node.** `noodle.util.selector`, "Selector", in Utilities. One `out`,
  the inputs `in1`…`inN`, and the stepped parameter `which` with the labels
  `In 1`…`In N`. `kind` is config: 0 for audio or control signals, 1 for
  notes, 2 for samples (or sets) and 3 for spectra, since it changes the ports.
- **Config and parameters.** How many inputs (`inputs`, 2 to 64, default 2) is
  config, because it changes the ports. `which` is a parameter, numbered from
  0 internally, so it can be automated or wired; the node rounds it and clamps
  it to an input, and a wired value chooses per sample. The choice is read from
  the first voice, as Random Select reads its trigger.
- **Signals.** `switch` is `Instant` or `Zero crossing`. Instant swaps at the
  sample the choice changes on; zero crossing waits for the old input's next
  crossing, per voice and channel, and gives up after 20 ms. An unconnected
  selected input is silent, so a half-patched selector still passes whichever
  input is selected.
- **Notes.** `switch` is `Finish held` or `Cut held`. Note-ons come from the
  selected input. On a switch, the old input's new note-ons are closed; held
  notes either keep their note-offs (`Finish held`) or are ended with a
  note-off at the switch (`Cut held`). Note-offs and expressions follow their
  own note-on, and other MIDI comes from the selected input.
- **Samples.** The output is a sample port. The `choice` signal carries the
  number of the input in force, plus `CHOICE_CUT` when `switch` is
  `Cut held`; the compiler wires it to the receiver's `sample_choice`, and the
  editor does not draw it.
- **Spectra.** The chosen input's frames pass, and a change takes effect at
  the first frame at or after it. Spectra have no `switch`.
- **Shapes.** A signal output broadcasts the inputs' shapes, so a mono input
  plays on every voice and channel another input sets. A sample selector's
  `choice` is mono; a spectral selector broadcasts the spectral inputs.
- **Memory and real-time.** `Memory::None` for samples and spectra,
  `Memory::Bounded(20)` for signals (a zero-crossing wait), and
  `Memory::Unbounded` for notes (a held note outlives the block); no
  allocation, with per-lane state only for zero-crossing waits.
- **Editor.** The node has no type until the first data wire, which sets
  `kind` in the same undo step; the inputs take any wire until then, grow with
  the wires like a mixer's, and the type-specific `switch` stays hidden until
  typed. A wire of another kind is refused once one is set. Properties shows
  "Chooses between: …" for the kind.
- **Use.** A send is one wire: a Gain's input is the pre-fader tap and its
  output the post-fader tap, so a Selector on those two taps makes pre/post
  switching a `which` change rather than moving a wire. S19 does not depend on
  it; its route still wires the taps out to a second Mix (#332).

Tests: `selector.rs` (selection, rounding and clamping, a wired `which`,
zero-crossing waits and giving up, broadcasting, unconnected inputs, the
default layout, each kind's shapes, finish and cut, the sample chooser and cut
flag, spectral framing, and bad input counts); `realtime.rs`
(`selector_never_allocates_through_switches_wiring_and_resizes`,
`selector_zero_crossing_never_allocates_through_switches_and_resizes` and
`selector_note_sample_and_spectral_kinds_never_allocate`); the editor tests
(`a_picker_*`, `the_first_wire_*`, `a_signal_first_*`, `another_type_*`,
`a_spectral_wire_*`, `a_sample_wire_*` and `the_inputs_grow_*`, which run for
both pickers); and `reset_matches_fresh.rs`, which runs every registered type.
