# Testing

- **Golden renders:** render test graphs offline and compare against stored
  WAV files within a tolerance.
- **Compiler tests:** cycle detection, shape inference and buffer reuse, plus
  property tests that generate random graphs and check that the plans are
  valid.
- **Group boundaries are invisible.** `noodle-nodes/tests/group_boundaries.rs`
  sends every wire of four example projects (audio, modulation, events,
  voices, spectra) through 1 and 3 nested groups, with the boundary controls
  untouched and set, and folds random selections into random nested groups.
  Each must render sample for sample like the flat project, with no
  diagnostics, and survive a save, a load and an undo. `flatten.rs` has the
  same property on random wiring, with no audio. `noodle-core/tests/random_edits.rs`
  applies random edits (add, wire, move into a group, remove, group), checking
  that undo and redo reach every state exactly and a save loads back equal.
- **Real-time safety:** the audio-thread allocation check runs in all
  debug builds and tests. `tests/parallel.rs` does the same for a plan on
  worker threads, counting allocations on every thread, and checks it sounds
  like one thread through plan swaps.
- **Plan swapping:** tests swap plans mid-render and check that the output
  is continuous, with no click at the swap: identical for an edit off the
  audible path, and no jump steeper than the signal's own for one on it.
- **App tests** drive the egui app with `egui_kittest`, sending real pointer
  and key events. `noodle-app/src/acceptance.rs` follows a milestone's
  "done when" end to end: it builds a patch in the node editor, plays it
  on ALSA's `null` device and tweaks it, saves, and reopens it unchanged.
- **CI** runs fmt, clippy and the tests on macOS, Windows and Linux.
