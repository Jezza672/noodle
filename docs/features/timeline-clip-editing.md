# Timeline clip editing

**Status:** done. Closes #190 to #194 and #195.

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

The tests deliberately broken to check they fail: the repaint request, the
mid-drag copy, the stand-in's position, the box and the paste spacing.
