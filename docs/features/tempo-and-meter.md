# Tempo and meter

Delivered. This is the record of what was planned and built; open work is in [ROADMAP.md](../ROADMAP.md), the design in [ARCHITECTURE.md](../ARCHITECTURE.md).

**Status:** done. Closes #209 and #210.

1. **Ruler menu (#209).** The ruler's right-click menu (which already had the
   time signature items and the loop band, #234) gains "Add tempo change at bar N",
   and every time signature change after the first is drawn as a marker on the
   ruler (`TempoMap::bar_start`). Meter changes sit in the existing tempo map on
   bar lines only; each edit is one `SetTempoMap` undo step.
2. **Beat Length node (#210).** `noodle.mod.beat_length`: a `bpm` parameter and a
   `division` choice, with `time` (seconds) and `rate` (hertz) outputs like Tempo,
   for any tempo rather than the project's.
