# Swing, humanise, choke and the MIDI ports view

Issues #330, #350 and #346, from the story gap solutions (decisions by Jeremy, 2026-11-03).

- **Swing node and choke groups (#330).** A Swing primitive on the note wire and a `choke{n}` group on each Note Split output; a negative-velocity note-off makes a Sampler fade out quickly in any mode. The Drum Rack template exposes `Swing` and a `choke` input per slot, with the closed and open hats already in one group. The songs' racks are built without them (`Rack::controls`), so the songs and their golden renders are unchanged. S09's route no longer needs a Script node.
- **Humanise, both ways (#350).** Option A: the piano roll's Humanise button gives the selected notes stored random offsets, and the clip's Timing (ticks) and Velocity (percent) amounts scale them when it plays; the drawn notes stay put and a ghost shows where each plays. Option B: a Humanise node on the note wire (`timing`, `velocity`, `seed`, 50 ms latency). S08 has a route for each.
- **MIDI ports view (#346).** View > MIDI ports lists every MIDI In, MIDI Control and MIDI Out node with a drop-down of the ports; a choice is a `SetConfig` of `port`, one undo step. A port other nodes use is named but stays choosable; a saved port that is not connected stays chosen and is labelled. The list is made when the view opens and on Refresh. "Add MIDI Out" adds a node on the first free port. The properties panel shows the port and points to the view. The row's status says whether the port is in the list, not whether the node opened it.

Tests: Swing (`noodle-nodes/src/swing.rs`), Humanise (`humanise.rs`) and choke (`note_split.rs`) have unit tests; the S03, S08 and S09 routes cover the UI.
