# First-use discoverability

The zero-docs new-user runs (PR #394) kept losing actions to features that
existed but could not be found. This batch fixes the first-use blockers. The
rule: a command-palette entry is never the whole fix. Each feature is
reachable where it is used, and the palette is an addition.

- **Add menu (#396).** Right-click and double-click on empty canvas open the
  same search as Shift+A. The empty-graph hint says so.
- **Track menu (#397).** Right-click on the track name, the RA/RM/M/S buttons
  and the header background all open one menu (Colour, Folder, Freeze track,
  Render to clip, Delete). There is no "..." button. The name's tooltip says
  the menu is there.
- **Clip actions (#398).** Slice to Sampler and Convert to MIDI join Pin
  transients and Split to stems in the clip panel, which also says the
  right-click menu has them. Palette entries (Split Selected Clip to Stems,
  Slice Selected Clip to Sampler, Pin Transients on Selected Clip, Convert
  Selected Clip to MIDI) act on the selected audio clip.
- **Tooltips (#401).** M, S, RA, the metronome and the automate "~" menu say
  what they do. The tempo source menu reads "Tempo map" rather than "Map".
- **Export (#406, #402).** "Nothing to export yet" says why (the project has
  no length) and how to fix it. With no mix versions, the export dialog shows
  a Versions row with an "Add mix versions" button; the palette has "Add Mix
  Version".
- **Sampler (#403).** A Sampler fed by a wire says where its sound comes from
  ("a Sample Set (N samples)") instead of "No sample loaded".
- **Problems (#404).** The status bar's "N problems" opens a list; picking
  one selects the node concerned.
- **Stale notice (#405).** A successful import clears the "Add a track before
  importing audio" message.
