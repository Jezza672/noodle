# Tempo and meter

Delivered. This is the record of what was planned and built; open work is in [ROADMAP.md](../ROADMAP.md), the design in [ARCHITECTURE.md](../ARCHITECTURE.md).

**Status:** done. Closes #209 and #210.

1. **Ruler menu (#209).** Right-click the timeline ruler for "Add tempo change at
   bar N" and "Add time signature change at bar N", snapped to the nearest bar
   (`TempoMap::nearest_bar`, `bar_start`). Meter changes sit in the existing tempo
   map on bar lines only and show as markers on the ruler; right-click a marker to
   change its signature or remove it. Each is one `SetTempoMap` undo step.
2. **Beat Length node (#210).** `noodle.mod.beat_length`: a `bpm` parameter and a
   `division` choice, with `time` (seconds) and `rate` (hertz) outputs like Tempo,
   for any tempo rather than the project's.
