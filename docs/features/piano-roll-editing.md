# Piano roll editing gaps

A batch of fixes to the piano roll found in use.

- **Computer keyboard octave.** The keyboard called note 60 "C3" while the
  roll (and `widgets::format::note_name`) call it C4, so the key labelled C1
  played what the roll labels C2. The keyboard now uses the roll's naming:
  C4 is note 60, the octaves run -1 to 9, and it starts on C4.
- **Keys light under MIDI in.** The roll listens to the session's MIDI bus
  (`piano_roll/live.rs`) while open, so notes from a hardware port or the
  computer keyboard light the same keys as clicking them.
- **Vertical zoom.** Alt with the wheel, or Alt with a pinch or Ctrl-scroll,
  zooms the key rows; a plain pinch or Ctrl-scroll still zooms time, and a
  touch-screen pinch that is clearly across or along zooms only that way
  (egui's `zoom_delta_2d`).
- **Region selection.** Shift-drag on empty space sweeps a box that adds the
  notes it touches to the selection. Cmd/Ctrl+A selects every note.
- **Paste.** Notes paste at the playhead, or at the left edge of the view
  when the playhead is outside the clip, so a paste is never out of sight.
- **Playhead.** Pressing or dragging on the roll's ruler moves the playhead
  (snapped, Alt for free); the loop brace's handles keep priority.
- **Follow the selection.** An open roll switches to the clip newly selected
  on the timeline (MIDI clips only), not just the last one double-clicked.
- **Velocity bar.** The bar being dragged is drawn last, outlined, with its
  value beside it.
- **Copy by dragging.** Cmd/Ctrl-drag on a note (or the whole selection) drags
  copies and leaves the originals.
- **Group velocity.** Dragging the velocity bar of a selected note scales
  every selected note by the same ratio, so the group keeps its shape.
