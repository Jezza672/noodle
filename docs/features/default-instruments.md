# Default instruments

A round of ready-to-play instruments in the system library, so a new project
can make music without building a synth first.

- **Ten synth templates**, each a group of primitive nodes that takes `notes`
  and sounds (Voices or Mono Note, a nested `Voice` group, Voice Mix inside),
  with three group presets apiece: Lead Synth, Bass Synth, Electric Piano,
  Organ, String Ensemble, Brass Section, Bells, Chip Synth, Pluck Synth, Warm
  Pad. Levels are trimmed so a chord peaks around -9 dBFS.
- **Three sampled instruments**, also with three presets each: Steel Drum
  (jlearman, Unlicense), Banjo (ganjo, CC0) and the Body Percussion Kit (CC0)
  on General MIDI drum keys with a choke between the two snaps. The audio is
  compiled into `noodle-nodes` (`system_samples`, 2.3 MB), found by hash by
  the sample store, so nothing is installed; sources and licences are in
  `crates/noodle-nodes/assets/samples/SOURCES.md`.
- **Fixed on the way:** the Multisample Instrument template wired the
  low-pass's `out`, which the State Variable Filter does not have (its output
  is `low`), so it made no sound; it now uses `low`.
- **Tests:** `noodle-eval/tests/instruments.rs` plays every instrument from a
  MIDI clip on a track and checks it sounds at a sensible level, stops, follows
  the key and (for the kit) plays on its keys; `system_samples` tests check the
  index matches the files. The architecture is in
  [../architecture/synths-and-library.md](../architecture/synths-and-library.md).
