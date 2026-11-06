# Pattern Player (#354, S14, S15, S09)

A sampler for MIDI clips: `noodle.events.pattern` plays a pattern on its
`notes` output, with the modes, rate and region a Sampler has for audio.

- **Patterns are ordinary MIDI clips the player owns.** The clip's `node` is
  the player and its `start` is ignored; the clips in ID order are the player's
  slots, picked by `slot`. The piano roll, `SetClip`, undo and take folders all
  work on them. `Project::timeline_clips` leaves them out, and the timeline,
  its extent and its cut all use it. `ClipFeeds::update` turns them into a
  `PatternSet` (notes in ticks) for the node; it never touches the timeline
  schedule.
- **Hand-off to the audio thread** (`noodle-nodes/src/pattern.rs`,
  `PatternShared`): a version counter read with an atomic load, the new set
  taken with a `try_lock` only when the version moved, replaced sets parked on
  the UI side so the last reference is never dropped on the audio thread. A
  changed pattern, slot or region ends the sounding notes first.
- **Modes.** Loop (`sync` Transport reads the position from the transport, so
  it stays on the bar through tempo changes and seeks and is silent while
  stopped; Free runs at the tempo from when it starts), One-shot, Gate (the
  note-off ends it) and Step (a rising edge on `advance`, or a `trigger` note,
  plays the next window of one `step`; `reset` goes back to the first).
  `play` (Always, Held, Toggle) says what switches a Loop on. This is a
  parameter, not "wired or not" as in the design, because a node cannot see
  whether an event input is wired.
- **Ports.** Inputs `trigger` (events), `advance`, `reset`; outputs `notes`,
  `gate` (1 while any pattern note sounds) and `out` (the velocity of the
  latest note). `out` has the LFO's output key, so Replace node keeps an LFO's
  targets (the S14 late change). Parameters: `mode`, `play`, `sync`, `rate`,
  `step` (Clock's divisions), `start`, `length` (steps), `slot`, `transpose`,
  `root`, `keytrack`, `velocity`.
- **Note IDs** are a per-node salt, a repeat count and the note's index, inside
  the bits clip notes use, so a retriggered copy never collides with the one
  ending and two players merged into one Voices do not collide.
- **Editing.** The properties panel (`noodle-app/src/pattern_panel.rs`) lists
  the slots (Edit opens the piano roll, Play picks the slot, Remove, `+ New
  slot`) and draws the step grid for the slot that plays: a row for each key,
  a cell for each step of the region (each cell is a button named "Step N on
  key K"; the grid squeezes to fit the panel and numbers its steps), a click for a note half a step long, a vertical drag for velocity,
  `+ Row` for a key not yet used, and `To triplets`, which moves every note
  from the straight grid to the triplet one and sets `step` to match in one
  batch. A timeline MIDI clip's menu has `Copy to pattern`.
- **Library.** Templates `Step Sequencer` (a Clock into `advance`, exposed
  `rate`, `length`, `transpose`), `Step Gate` (a looping player into a VCA,
  exposed `step`, `depth`) and `Pattern Pads` (a pad note into `trigger`,
  looping on the bar while held or toggled, exposed `slot`, `transpose`), each
  with three or more group presets, and five node presets.
- **Routes.** S14 gains the late change "replace the LFO with a step
  sequencer" (Replace with..., then `+ New slot`) and is `full`. S15 builds the
  gate from a player and a pattern drawn in the grid, with the triplet change
  as the button; its remaining gap is the tempo change. S09 has a second route
  that draws the bar in the grid in front of the Script node.

Not built: a swing option on the player (the Swing node of #330 goes on its
`notes` wire, so swing is defined once), a per-voice player (`gate` and `out`
are mono until event wires carry a voice dimension), and a per-note length in
the grid (use the piano roll).
