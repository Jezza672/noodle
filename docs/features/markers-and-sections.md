# Markers and sections (#329)

Restructuring a song by section (story S07), as Jeremy chose option C: markers and a range selection, not a section ruler.

## What landed

- **Markers.** `Project::markers()`: a name and a tick (`Marker`), kept in order, saved as `markers` when there are any, changed only by `Command::SetMarkers`. Added from the ruler's menu ("Add marker at bar N"), dragged to move, renamed by typing in their menu (Enter), deleted from it.
- **A section** is the span from one marker to the next (the last one runs to the end of the clips and automation). A marker's menu has "Select to <next marker>"; any span can also be picked with "Start selection here" and "End selection here" on the ruler's menu. The selection (`TimelineState::range`) is across all tracks, shown as a tint and a band on the ruler, and is never held together with a clip or point selection.
- **Copy, cut, paste.** With a range selected, Cmd+C copies **clips and automation lanes only, plus the tempo** (the tempo map shows as an automation lane). The copy is its own clipboard type (`application/x.noodle.section`), and copies are not linked to what they came from. Paste goes at the playhead: clips are added, automation points and tempo changes inside the pasted span are replaced, and what follows the span keeps its values. Cut copies, then empties the range without closing the gap.
- **Insert time and Delete time** (Edit menu, on the selected range): insert opens a gap as long as the range at its start, delete takes the range out and closes the gap. Clips across the edge are split; later clips, lane points, tempo changes and markers move; a lane holds its value across an inserted gap; the tempo that followed the deleted range carries on from where it began. Signature changes move only when the shift is a whole number of bars. Each is one undo step.
- The code is `crates/noodle-app/src/timeline/sections.rs` (`copy`, `paste_section`, `clear`, `insert_time`, `delete_time`), with its tests.
- The S07 route (`stories/routes/s07_sections.rs`) marks a verse and a chorus, copies the verse after the chorus and, as its late change, lengthens the chorus by inserting four bars and pasting it twice. The story is `full`.

## Not done

- A marker has no colour, and no time-locked variant (#363's hit points) yet.
- Markers are not yet used by the playback cache's hot starts, or by album export (#360).
