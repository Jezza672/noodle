# Linked clips across tracks

Planned in the roadmap (settled in #281), built in #288 as one PR.

**What landed.**

- **Clip groups.** A link is a `ClipGroup` in the project (`noodle-core/src/clip.rs`,
  `Project::clip_groups`, saved as `clip_groups`): a set of member clips under a
  `ClipGroupId`. `Command::SetClipGroup` sets, replaces or removes one, so a link and
  an unlink are undo steps like any edit. A member that is not in the project
  (deleted, and back with an undo) is ignored, and a group with fewer than two live
  members is neither a link nor saved. `Project::group_of` / `linked_with` /
  `with_linked` read links; `link_command` and `unlink_command` build the edits
  (Link merges the groups of everything selected into one).
- **Shared edits.** Selecting a linked clip selects its whole link (click, box, shift-click
  toggle). Move, trim, slip, cut (`S`), delete and cut/copy then act on every member:
  the timeline works each member out against its own file and the tempo map, so
  the edit is one `Command::Batch` and one undo step. A cut leaves the left halves
  linked and links the right halves to each other. Gain, fades and mute stay per clip.
  A move keeps every member on its own track.
- **Link and Unlink** are in a clip's right-click menu. Recording several armed tracks in
  one take links the new clips (`Session::stop_recording`).
- **One warp.** `Project::section_edit` (and so `warp_edit`) treats a clip and its link
  as one: the edit goes to the warp they share, in place; if a clip outside the link
  uses that warp, the whole link forks to a new one together. A section edit (sliding
  the audio under the beat lines) moves every member's offset and length by the same
  frames. Linking gives every audio member the warp of the first member that has
  one. Unlinked clips that still share a warp behave as before: editing one forks it.
- **Warp editor.** For a linked clip the transient row has a lane per member, each in
  its track's colour, so candidates a few milliseconds apart (kick mic and overheads)
  stay apart. Double-clicking any lane's handle pins that clip's transient on the
  shared warp, and moving a marker moves it for every member.
- **Split to stems links** the source, the stems and the residual (joining the source's
  own link if it has one), on the source's warp. They used to get a copy each.

**Tests.** `noodle-core` project tests (link, unlink, merge, undo, save and load,
shared warp edit without a fork, fork for the whole link, section slide, deleted members);
`timeline/tests.rs` (selection, drag, trim, cut, delete, the menu);
`clip_editor/tests.rs` (a lane per member); `session/stems.rs` (one link, one warp);
`session.rs` (a take on two tracks is linked); `noodle-nodes/tests/linked_clips.rs`
(four mics linked, slid a beat and warped from markers still sum to one sample-aligned
ramp).

**Not done.** Copy, paste and duplicate make unlinked copies. Quantise and loop edits
reach the link through the shared warp; a MIDI clip's own loop is per clip.
