# Timeline clip editing

**Status:** done. Closes #190 to #195 and #205.

1. **Resizing a warped clip** no longer scales its waveform while the new render
   is worked out (#190): the render remembers the ticks it was made for and is
   drawn there, clipped to the clip.
2. **Clip edges trail the pointer** (#191): an edit only shows in the next frame,
   and nothing asked for one once the pointer stopped moving. The arrangement now
   requests a frame whenever it makes an edit.
3. **Drag to select** (#192): a box over empty lane space selects the clips it
   touches; Shift adds to the selection.
4. **Ctrl after the drag starts** copies (#193): the clips go back where the drag
   began, copies are added and the drag carries on with them, in the one undo step.
5. **Copy, cut, paste and duplicate** (#194, #195): Cmd+C/X/V/D, the Edit menu, the
   clips' right-click menu and a Paste item on the lane menu.

6. **Notes and automation points join the clipboard** (#195): Cmd+C/X/V/D in the
   piano roll copy notes with their spacing and paste them at the playhead
   (or the clip's start when the playhead is elsewhere), selected; the selected
   automation point copies, cuts and pastes at the playhead onto its lane. The
   Edit menu acts on the view last clicked in (arrangement, piano roll, node
   graph); node copy and duplicate are now on it too. Each type has its own MIME
   type on the one clipboard module, rather than one tagged enum, so a paste
   reads only what it can use.
7. **Audio input meters** (#205): every wired audio input shows its level (see
   "Audio input meters" in `architecture/ui.md`).

The tests deliberately broken to check they fail: the repaint request, the
mid-drag copy, the stand-in's position, the box and the paste spacing.
