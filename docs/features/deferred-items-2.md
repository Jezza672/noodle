# Deferred items, second batch

- **Plugin editors keep their plugin running.** `NodeType::always_runs_for(node)`
  (default: `always_runs()`) is asked each time a plan is built, so it can be per
  instance. A plugin node answers `editor_is_open(node)`: while its editor is up
  the plugin runs even when nothing listens to it, and the editor's meters move.
  `Session::poll_plugin_editors` builds the plan again when the set of open
  editors changes. VST3 edits no longer wait for the plugin to be wired while the
  editor is open.
- **Plugin latency changes.** The CLAP host now offers the `latency` extension:
  `changed` flags the instance, the plugin thread asks the plugin again and stores
  the figure (`Flags::latency`, `noodle_plugins::node_latency`). The engine asks
  `NodeType::current_latency(node)` when a plan keeps an instance, and
  `Session::poll_plugin_latency` builds the plan again when a figure moves, so delay
  compensation follows. The plugin is asked again without being deactivated and
  reactivated, which the CLAP text allows a host to ask for; a plugin that only
  reports its new latency after reactivation is still followed at the next
  rebuild. VST3, AU and sandboxed plugins are not covered (they report nothing
  after loading).
- **One retry policy for files that fail to open** (`noodle-nodes/src/clip/retry.rs`).
  The stream hub and the stretch worker both used a fixed two second retry. They now
  share `Retry`: the first retry after 2 s, each failure in a row doubling the wait up to
  30 s, success or a new schedule starting over, and the task sleeps until the
  earliest retry instead of a fixed period.
- **Re-quantise a recorded MIDI clip.** A recorded note keeps its raw take in
  `MidiNote::played` (the start as played, and the start recording put it at).
  The piano roll's **Re-quantise** pill lists the grids and *As played*
  (`MidiClip::requantise`, one undo step through `SetClip`); the grid is the
  timeline's, not the clip's. A note moved by hand since (its start is no longer the
  place recording put it) and a drawn note are left alone; lengths are kept.

Not done: the sandbox monitor still wakes every 250 ms while a host is healthy.
Sleeping with no tick needs a wait on the child process that does not take the `Child`
(`waitid` with `WNOWAIT` on Unix, a process handle on Windows) and the editor flag
mirrored some other way than by polling; it stays on the roadmap.
