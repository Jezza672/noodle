# Code cleanup audit

An audit found ten places where the code had painted itself into a corner,
repeated itself or disagreed with itself, and where an idle laptop paid for it
in wake-ups. The issues were worked in batches. This file records what landed
in the last one; the earlier batches are in the PRs the issues name.

## Batch: idle power, derived state and file watching (#237, #238, #239, #243, #244)

- **#238 Project revision.** `Project::revision()` changes with every applied
  command; `History::position()` replaces the deep-equality dirty check and the
  `saved` clone in `Session`. The editor's `Scene`, the timeline's track list and
  the wired-port set are cached by revision. `Registry::generation()` covers the
  node types. The track list is built in one pass over the clips instead of
  O(nodes × clips).
- **#237 The window sleeps.** `noodle_engine::wake` (a process-wide waker the app
  registers) is called when a `Job` ends and when the plugin scan ends.
  `Session::next_deadline` names what still has to be looked at, and `App::show`
  asks for one repaint at the earliest, replacing the fixed 16/100 ms timer and
  the separate MIDI timer. An idle project runs about one frame a second.
- **#239 One audio-file watcher.** `noodle_io::AudioFiles` and `FileStamp` (size
  and modification time) replace `FileWatch`, the timeline's mtime check and
  `ClipFeeds`' mtime check. No `stat` on the UI thread for a file already known;
  a file replaced with the same mtime and another length is now noticed
  everywhere. The hub and warp worker retry timers were left alone.
- **#243 Plugin housekeeping.** The plugin thread sleeps on a `Doorbell` and wakes
  for a queued command, a callback, a resize, a closed editor, a timer or an open
  editor; `pump()` returns `None` when none applies. The sandbox monitor ticks at
  250 ms (not 20 ms) while the host is healthy and idle, and wakes at once when the
  audio thread drops a block.
- **#244 Piano roll snap.** Notes snap to the absolute grid the roll draws
  (`grid::snap_to`), not to multiples of the step from the clip's start, so a
  clip placed off the beat or a 6/8 bar snaps to the lines on screen. Literal
  `960`s in the piano roll and `snap.rs` use `TICKS_PER_QUARTER`.

Left over: moving the arrangement's and the clip editor's axes onto a shared
`TimeAxis` type (the piano roll part of #244 is done), a zero-wake sandbox
monitor (it needs a way to wait on the child process that does not take the
`Child`), and a single retry policy for files that fail to open.
