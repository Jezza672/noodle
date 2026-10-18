# Plugins

A plugin is a node. Its parameters become input ports, and its audio and note
ports become graph ports. Plugin state is saved in the Project.

- **CLAP** is hosted through `clack` (`noodle-plugins`).
- **VST3** is hosted through the Rust `vst3` bindings (raw COM, loaded with
  `libloading`): see "VST3 hosting" below.
- **AU** (macOS only) is hosted through AudioToolbox's component API: see
  "AU hosting" below.
- Plugin editors open as separate native windows: embedded in a window the host makes (macOS, Windows), or the plugin's own floating window (Linux, or when it can't embed). Both run on the plugin thread.
- Scanning runs in a child process, so a plugin that crashes during scanning
  can't take down the app.
- **Out-of-process hosting (M6)** runs each plugin in a host process of its
  own that exchanges audio through shared memory, so a crashing or hanging
  plugin can't take down the session; see [Sandboxed plugins](#sandboxed-plugins).
- **Latency:** plugins report their latency (CLAP's latency extension, VST3's
  `getLatencySamples`, `kAudioUnitProperty_Latency`, all read once after activation, on the plugin thread) and the engine
  compensates for it; see [Performance (M6)](performance.md).

## CLAP hosting

**Scanning.** `ScanCommand` runs a scanner in a child process for each
`.clap` file (`noodle-plugin-scan <file>`, or `noodle` / `noodle-app` started
with the hidden `--noodle-scan-plugin` argument, which their `main` hands to
`run_scan_child_if_requested`). The child loads the file, instantiates each
plugin to read its audio ports, note ports and parameters, and prints one JSON
line (`Descriptor`s). A crash, a hang (30 s timeout, then it is killed) or a
file that isn't a plugin comes back as an `error` on that file's `PluginFile`,
not as a failure of the scan. `ScanCache` keeps the results in the user's
cache folder, keyed by each file's size and modification time, so a file is
scanned again only when it changes (a failed one too). The standard folders
are `~/.clap` and `/usr/lib/clap` on Linux, `/Library/Audio/Plug-Ins/CLAP`
(system and user) on macOS and `%COMMONPROGRAMFILES%\CLAP` and
`%LOCALAPPDATA%\Programs\Common\CLAP` on Windows, plus `CLAP_PATH`.

**Node types.** A `Catalog` builds one `NodeType` per scanned plugin, with ID
`clap.<plugin id>` and category "Plugins". Ports come from the scan alone, so
a project's graph compiles without loading any plugin code. The layout
depends on one setting, `params`, which lists the parameters that get ports
(below); it never depends on the plugin's code:

| Port | Key |
|---|---|
| audio inputs, one per CLAP audio input port | `in`, `in2`, ... |
| a parameter input per exposed parameter (see below) | `p<param id>` |
| audio outputs | `out`, `out2`, ... |
| note input ports (all are read) | `notes`, `notes2`, ... |
| note output ports (all are written) | `notes_out`, ... |

**Exposed parameters.** A synth can have hundreds of parameters, which would
make its node unusably tall, so only the ones listed in the node's `params`
setting get ports (comma-separated parameter ids; `none` for no ports). That
is config, since it changes the ports. A node without the setting shows every
parameter, so projects saved before it existed keep their ports; the plugin
browser and the add-node search start a plugin with 5 or more parameters
with `none`, and fewer than 5 with all of them, so the user opts in to the
ones they want. They get that through `NodeType::initial_config`, which
any node type can use for a setting whose first value differs from leaving
it out. The
properties panel has a searchable picker, grouped by the plugin's modules. A
parameter with a wire or automation lane into it can't be hidden (they would
point at nothing). Changing the choice while the editor is open keeps the
editor's changes in the same undo step, and a parameter that is hidden forgets
its stored value. A hidden parameter is never sent to the plugin, so it keeps what
the plugin's saved state gave it. The editor shows a node's note inputs before
its audio and parameter inputs (the layout keeps them in their own list).

A parameter is `Offset`-modulated, with the plugin's range and default; a
stepped CLAP parameter is a stepped one (the node rounds the value before
sending). One instance has one voice: a polyphonic wire into a plugin is a
compile error, so put a Voice Mix before it. A mono wire into a stereo port
feeds both channels; any other channel mismatch is an error.

**macOS and Linux keep plugins loaded.** A plugin's Objective-C objects (an
editor's views) can still be queued for release on the main thread when its
instance is dropped, and releasing them after the plugin is unloaded or
deinitialised crashes. On Linux, a mitigation for an abort seen once in CI and
not reproduced (`double free or corruption` as the real-plugin tests exited):
plugins' GUI toolkits can leave global state that unloading and process exit
both free. So on both, each plugin file, CLAP or VST3, stays loaded for the
life of the app: the entry's `deinit` and the bundle's exit never run, and a
plugin file replaced on disk takes effect after a restart. Windows lets them
go. (`tests/keep_loaded.rs` checks this through the test plugins' `deinit` and
module exit.)

**Instances and the plugin thread.** CLAP says a plugin instance belongs to
its main thread, and JUCE-based plugins expect that to be one thread for the
whole process. So one shared plugin thread (`thread.rs`, started on first
use) owns every live instance. `instantiate` sends it a load command (30 s
limit) and gets back the audio processor, which the node holds; on drop the
node sends the processor back and the thread deactivates and destroys the
instance. The thread also runs what CLAP gives the main thread: plugin timers
(`timer-support`), `request_callback`, and editor creation and destruction.
By default that thread is one of its own, but an editor wants the application's
main thread on macOS (Cocoa) and a thread that pumps messages on Windows. So an
application whose UI thread should be the plugin thread calls
`adopt_current_thread(wake)` once at startup and `pump()` every frame:
requests from the UI thread then run on the spot, and requests from other
threads (a freeze render loading the plugin, say) wait in a queue for `pump`,
which `wake` asks the UI to run. `pump` also returns when to be called next so
plugin timers run (nothing is asked of an app with no plugin loaded). The app
does this on macOS and Windows only; Linux plugins keep their own thread, since
an X11 editor needs no event loop of ours. The cost is that a slow plugin
load or editor call stalls the UI thread there, as it already did when the
compile was waiting on it. One plugin that hangs while loading blocks the
others, unless plugins are sandboxed (see [Sandboxed plugins](#sandboxed-plugins)).

**Processing** (`node.rs`). Each block is copied into preallocated plugin
buffers (in chunks of at most `max_frames`), parameters whose value changed
are sent as timed `ParamValue` events (at frame 0, and every `PARAM_STEP` = 32
frames while a wire drives them, so audio-rate modulation is stepped), notes
are converted to CLAP note events (a note-off is given the key and channel its
note-on had, since Noodle's note-off carries only the id), and the plugin's
output is copied out. A plugin's note output becomes the node's event output.
The transport (tempo, position, signature and where it began, playing) is passed on. Nothing on
this path allocates (`tests/realtime.rs` checks it), so events beyond 4096 per
block are dropped. A plugin that fails in `process` gives silence for that
block. A parameter is sent once its port value differs from the plugin's
reported default: the first block leaves an untouched port alone, so state the
plugin restored isn't overwritten (Surge XT reports a default of 0 for every
parameter, and sending those silenced it).

**State.** The node's `state` config setting holds the plugin's saved state as
base64 text, so it is written to the project file with the rest of the graph.
It is loaded into the plugin before activation. Bad base64, or state the
plugin refuses, is a node error rather than a crash. CLAP state is wrapped on
saving in `NCS1`, a length and a checksum, and checked before it reaches the
plugin: some plugins crash the host on state that is cut short, and plugins run
in our process. (State saved before the wrapper is passed on unchecked.)
`capture_state` loads a
plugin (optionally with earlier state) and returns what it saves.

**Editors and capture** (`editor.rs`). Running plugin nodes register by node
ID. `open_editor(node)` opens the plugin's editor (CLAP `gui`). If the
application has set a window factory (`set_window_factory`; on macOS and
Windows `adopt_current_thread` installs one that makes an `NSWindow` or a
top-level `HWND`, `src/window/`) and the plugin supports the platform's API
non-floating, the editor is *embedded*: the host makes a hidden window of the
plugin's `get_size`, gives the plugin its handle with `set_parent`, and shows
both. The host follows the window: a size the plugin asks for
(`request_resize`) is applied from the plugin thread's housekeeping, a size the
user drags to goes through the plugin's `adjust_size` and `set_size`, and the
close button closes the editor (polled: `EditorWindow::is_closed`). With no
factory, or if embedding fails, the plugin's own floating window is used (the
platform's API). Edits in the window go to
the running plugin, so they are heard at once. `capture(node)` reads the live
plugin's saved state and every parameter's value; `Captured::commands` turns
that into one `Command::Batch`: `SetConfig state` plus a `SetParam` per
parameter, so the ports agree with the plugin (the ports win on the first
block). The app opens editors from the properties panel or a double-click on the node (`Edit::PluginEditor`; a failure to open stays beside the button)
and captures when the window closes (`Session::poll_plugin_editors`), as one
undo step. That is a config change, so the node recompiles and its plugin is
reloaded from the stored state. State changed while an editor is open is
stored only when it closes. The app follows an editor by its instance (`EditorKey`), not the node ID, so a freeze render of the same node can't take its place; if the node is rebuilt (rewired, config changed) while the editor is open, the editor's instance is gone and its unsaved edits with it. Editor commands wait up to 10 s on the plugin thread, and the app calls them synchronously, so a busy plugin thread freezes the UI that long.

## AU hosting

An Audio Unit is the same kind of node as the others (`PluginNodeType`, the
same `Catalog`, scan cache, browser, sandbox and caching), with ID
`au.<component id>`. The code is in `noodle-plugins/src/au/`; it compiles on
macOS only (`mod.rs` holds what is portable: IDs, names, the MIDI mapping, so
other systems can read a scan cache saved on a Mac and say why it can't be
built).

- **No files.** The system keeps AUs in a registry, found by type, subtype and
  manufacturer code, so an AU is a *virtual file* whose path is
  `audiounit:<id>`, the three codes as 8 hex digits each, joined by `-`.
  `discover` adds `audio_unit_files()` (every installed effect `aufx`,
  instrument `aumu`, music effect `aumf` and generator `augn`, listed with
  `AudioComponentFindNext`, which loads no plugin code) to the folders'
  results; `discover_in` over given folders doesn't. The scan child
  instantiates the unit to read its buses and parameters. The scan cache's
  "modification time" for one is its component version, so an update
  rescans.
- **Audio.** Every bus is set to non-interleaved 32-bit float at the engine's
  rate, with `MaximumFramesPerSlice` set to the block size. The node pulls
  each output bus with `AudioUnitRender`; the unit's input buses pull from a
  render callback that copies the node's inputs (a `Pull` the node owns and
  frees after the unit). A block runs in chunks of at most `max_frames`.
- **Events.** Parameters go in with `AudioUnitSetParameter` and a frame
  offset, a wired one sampled every `PARAM_STEP` frames; notes as MIDI
  through `MusicDeviceMIDIEvent` (note-on, note-off remembering the key and
  channel, raw MIDI). Instruments and music effects get a note port. Note
  expressions and MIDI output aren't mapped. The transport reaches the unit
  through the host callbacks (`HostCallbackInfo`: tempo and beat, time
  signature and bar, playing and sample position).
- **State** is the unit's `kAudioUnitProperty_ClassInfo` property list,
  serialised as a binary plist. Latency is `kAudioUnitProperty_Latency`.
- **Parameters** in the global scope; meters, read-only and non-realtime
  ones are left out.
- **Not yet:** editors (a Cocoa view factory or `AUViewController`; the
  properties panel says so, and state changes by wiring the parameter
  ports), AUv3 units that need asynchronous instantiation, and AUs outside the
  system registry. `tests/au_realtime.rs` (macOS) counts allocations on the
  Rust side of the AU node (the delay with a wired parameter, the synth with
  notes, a seek); what Apple's units do inside their render calls is their
  code and not seen by Rust's allocator.

## VST3 hosting

A VST3 plugin is the same kind of node as a CLAP one (`PluginNodeType`, the
same `Catalog`, scan cache, browser and caching), with ID `vst3.<class id>`
(32 hex digits; `Descriptor::format` says which standard it is, and scan
results saved before VST3 load as CLAP). The code is in
`noodle-plugins/src/vst3/`.

- **Files.** A `.vst3` is a bundle on every system (a folder holding the
  binary at `Contents/x86_64-linux/<name>.so`, `Contents/MacOS/<name>` or
  `Contents/x86_64-win/<name>.vst3`) or, on Windows, a plain DLL. The scan
  returns it as one file and doesn't search inside. Folders: `~/.vst3`,
  `/usr/lib/vst3`, `/usr/local/lib/vst3` on Linux;
  `/Library/Audio/Plug-Ins/VST3` (system and user) on macOS;
  `%COMMONPROGRAMFILES%\VST3` and `%LOCALAPPDATA%\Programs\Common\VST3` on
  Windows; plus `VST3_PATH`.
- **Loading** (`module.rs`). The binary is opened, its entry point is run
  (`ModuleEntry` with the `dlopen` handle on Linux, `bundleEntry` with the
  `CFBundle` on macOS, `InitDll` on Windows) and `GetPluginFactory` gives the
  factory. A module is shared by everything loaded from it (the entry runs
  once while any plugin is alive, `ModuleExit` after the last), and each plugin
  holds an `Arc<Module>` so the code outlives it. The classes that count are
  those in the category "Audio Module Class".
- **Plugin** (`plugin.rs`). The component, its `IAudioProcessor` and a
  controller: the component itself if it is also an `IEditController`,
  otherwise the class it names, initialised on its own. Both get an
  `IHostApplication` (the host's name, and `IMessage`/`IAttributeList` for
  plugins that make them) and the controller an `IComponentHandler`; a separate
  component and controller are wired through `IConnectionPoint`, so their
  messages reach each other (on the sender's thread; disconnected before
  teardown). Set-up follows the
  standard's order: bus arrangements (the plugin's own), every bus activated,
  `setupProcessing`, `setActive`, and `setProcessing` on the first block, on the
  audio thread. Tear-down is the reverse, with the component released last.
  The component is made, set up and dropped on the shared plugin thread (via
  `thread::run_job`, so a macOS plugin that wants the main thread gets it where
  the app has adopted its UI thread), processed on the audio thread.
- **Ports.** As for CLAP. Every audio bus is a port (`in`, `in2`, ...; the
  plugin's default channel count) and every event input bus is a note input
  (`notes`, `notes2`, ...). Parameters come from the controller: hidden,
  read-only and program-change ones are left out. A continuous parameter's port
  holds its normalized value, 0 to 1 (VST3 has no host-side plain range
  without asking the controller per value); a stepped one holds 0 to its
  step count and is rounded.
- **Processing** (`node.rs`). Per chunk of at most `max_frames`: the inputs
  are copied into preallocated buses, a parameter whose value changed becomes a
  point in its `IParamValueQueue` (at the chunk's start, then every
  `PARAM_STEP` frames while a wire drives it), notes become `Event`s
  (`kNoteOnEvent`, `kNoteOffEvent` with the key the note-on had, note
  expressions, polyphonic pressure) merged in time order across the note inputs,
  and a note output bus is read back. The queues and event lists are COM
  objects of a fixed size made when the node is built (`classes.rs`), so
  nothing allocates in a block (`tests/realtime.rs` checks it; points past a
  queue's room are dropped). A port at its default is not sent, so state the
  plugin was restored from isn't overwritten by an untouched port (the CLAP
  node does the same: a plugin can report a default that isn't its live value). Raw MIDI controllers (modulation wheel,
  volume, pan, expression, sustain, channel pressure, pitch bend) go through
  the controller's `IMidiMapping` to parameters, which need no port; a
  controller with no mapping is sent as a legacy MIDI event.
- **State.** `Plugin::save_state` writes `NVS2`, the component's state length
  as a little-endian `u32`, a checksum of everything after it, the component's
  state, then the controller's (only
  for a separate controller), through memory `IBStream`s. A bad checksum is
  refused before the plugin sees the bytes (`NVS1`, saved without one, is still
  read, unchecked). Loading hands the
  component its bytes, then the controller `setComponentState` with the same
  bytes and its own `setState`. It lives in the `state` setting like CLAP's.
- **Editors** (`gui.rs`). VST3 has no floating windows, so an editor is
  always an `IPlugView` embedded in a window the app makes (the window
  factory of `window/`; macOS and Windows have one built in, Linux has none
  yet, so there opening an editor says so). The `Gui` for each plugin lives
  in a table on the plugin thread under a key (a VST3 key has the top bit
  set, which is how `editor.rs` tells it from a CLAP instance's); open, close
  and capture are jobs on that thread, and the node holds only the key. The
  view gets an `IPlugFrame` whose `resizeView` only sets a flag, which the
  thread's housekeeping carries out (resize the window, then `onSize`), as
  it does the user dragging the window's edge (`checkSizeConstraint`, then
  `onSize`) and a closed window (a resize is thus carried out up to 50 ms after
  the plugin asks, not inside `resizeView`). Capturing a running VST3 first
  tells the controller what the processor was given through ports and MIDI
  (`setParamNormalized`, from a mirror of the last values sent, so a stale
  controller value never overwrites a port), then reads the component's and
  controller's state and each listed parameter's value (a stepped one as its
  step number) from the plugin thread while the audio thread
  runs, as the standard's UI-thread calls allow.
- **Edits.** A controller reports what the user does in the editor through
  `IComponentHandler::performEdit`; the host must pass it to the processor, or
  a plugin with a separate controller would change its display and not its
  sound. The handler stores the value in an `Edits` table (one atomic per
  parameter that isn't read-only, hidden ones included, made when the plugin
  is) and the audio thread reads it at the start of each chunk and queues a
  parameter change, without a lock or an allocation. The port's last-sent
  value is left alone, so an untouched port doesn't undo the edit; capturing
  the editor then stores the edit in the port. Not mirrored the other way:
  a wire modulating a parameter doesn't move the editor's display.
- **Transport.** `barPositionMusic` is the start of the current bar in
  quarter notes from the signature (6/8 is three quarters long), assuming the
  bars from the project's start are all the current signature's length.
- **Not yet.** A Linux window for editors, units and program lists, 64-bit
  audio, `IProcessContextRequirements`, `IRunLoop`, and note output other than
  note on and off. Latency is read once, when the plugin is created; a change
  the plugin announces later is not followed.

**Plugin browser** (`noodle-app/src/plugin_browser.rs`, View > Plugins). Lists
the catalog with a search over name, vendor, ID, format and kind; Add puts the node on
the canvas; Rescan runs the scan in the background (Shift scans every file
again) and the files that failed are listed with why.

**Caching.** Plugins are deterministic by default, so they can be frozen and
exported from the cache. The key covers the plugin's ID, its file's size and
modification time (not a content hash: hashing a plugin on every edit would be
slow) and the node's config (so its state). A plugin that uses randomness sets
the `nondeterministic` setting, and is never cached.

**Rendering.** Freeze, export and background renders build their own
registries, so `Catalog::extender()` is passed as
`RenderRequest::extend_registry`, and a render sees the same plugins as live
playback. The app scans in the background at startup (the cache is read first,
so plugins found last time are available at once) and compiles again when the
scan reports.

**Config exceptions.** The `state` and `nondeterministic` settings change
neither ports nor shapes, which the config-versus-parameter rule says makes
them parameters. They stay config because neither is a number a wire could
sensibly drive: state is an opaque blob and `nondeterministic` is a flag.

**Threads.** `instantiate` loads a plugin on its own thread (waiting up to 30
s, then failing the node), and `start_processing` runs on the first block, on
the audio thread. 

**Limits.** The transport's bar fields assume quarter-note beats and a
meter that has not changed. A plugin that crashes while playing takes the process with it
(unless plugins are sandboxed, below); editors are separate windows (not drawn inside the app's window), Linux has no
embedding and no `posix-fd`, and the macOS and Windows editor windows are
checked only by CI compiling them and by one hand test on the Mac; CLAP extensions beyond audio
ports, note ports, parameters and state (e.g. tail, voice info, remote
controls) are not used.

## Sandboxed plugins

With `noodle_plugins::set_sandbox(Some(SandboxCommand))` (the app's View >
Plugins checkbox "Run plugins in separate processes", saved in the
preferences, or `noodle --sandbox-plugins`) each plugin node, CLAP or VST3,
is built in a **plugin host process** instead of in the app. The command is
this program started with the hidden `--noodle-plugin-host` argument (handled
by `run_scan_child_if_requested` next to the scanner's; the `noodle-plugin-scan`
program is a host too). It applies to nodes built afterwards; the app
compiles the graph again when the option changes. Off by default.

The code is `noodle-plugins/src/sandbox/`:

- **`host.rs`, the child.** It connects back to the app on a loopback socket
  (a token on the command line proves who started it), receives a `Hello` (the
  shared file, the plugin and its descriptor, the node's config and shapes) and builds
  exactly the node the app would have built in-process
  (`PluginNodeType::instantiate_local`, with the plugin thread, state restore
  and latency as before). It replies `Ready { latency }` or `Failed`, then runs
  blocks. A second thread serves the control socket and keeps the editor flag; on
  macOS and Windows the roles swap (see Editors and state).
- **`shm.rs`, the shared file.** One memory-mapped file (`/dev/shm` on Linux,
  else the temp folder; unlinked once the host has it, or delete-on-close on
  Windows) holds a header of atomics, the block's transport, each input's
  samples (or, for an input that is one value for the block, just the value;
  lane-packed by the block's frame count), silence flags, events in and out
  (`WireEvent`, up to 4096 per note input and 1024 per output port a block),
  and the outputs. Both sides compute the layout from the same `Spec`.
- **`proxy.rs`, the node.** `SandboxedNode::process` copies a block in,
  bumps `req_seq`, and waits for `done_seq`: it spins, then yields. If the host
  is asleep (it spins only 50 µs after a block, then blocks reading its
  stdin) one byte down that pipe wakes it. Live, it gives up when half the block's
  duration is up (so the rest of the graph can still finish), plays silence for that block and counts it
  (`SandboxStatus::dropped_blocks`); while the host is still on an earlier
  block, later blocks are skipped without touching the file. Offline renders (anything built through
  `Catalog::extender`, i.e. freeze and export) are *patient*: they wait for as
  long as the host lives. Nothing here allocates, locks or waits on a lock
  (`tests/realtime.rs` counts); the one syscall is the wake-up write.
- **Restarts.** A monitor thread per node polls the child (every 20 ms). If it
  exits, or a block has been pending for 2 s (60 s when patient; a patient node waits out a restart instead of rendering silence), it flips
  `state` away from RUNNING, waits for the audio thread to leave (`in_audio`),
  kills the child, and after a back-off (250 ms times the attempt) starts a new
  one from the node's original settings, so the plugin comes back as saved
  in the project and not as it was. After 4 failures (counted over the node's whole life, not a window) the node gives up and
  plays silence until the graph compiles it again. The shared file is zeroed when made so its pages are
  already in memory. `sandbox_status(node)`
  reports `running`, `failed`, `restarts`, `dropped_blocks`, `last_error` and
  the child's `pid`; the app polls it and says so in the status line.
- **Editors and state.** The editor functions in `editor.rs` find a node's
  newest instance, in-process or remote; for a remote one they send
  `OpenEditor`, `CloseEditor` and `Capture` over the control socket and the
  host runs them on its own plugin thread, so capturing an edited plugin back into
  the project works as before. On macOS and Windows the host's **main thread is
  its plugin thread** (`adopt_current_thread`, as in the app) and its
  control loop pumps `pump()` and the native window events
  (`window::pump_events`: an AppKit event drain with an Accessory activation
  policy on macOS, `PeekMessage` on Windows) every 8 ms, while the blocks run
  on a thread of their own; editors are then embedded in host-made windows
  exactly as in-process. On Linux the main thread runs blocks and plugins draw
  their own windows. `--noodle-plugin-host-pumped` runs the macOS/Windows
  arrangement on any system (the sandbox tests use it on Linux). Limits: the
  test plugin only embeds in the tests' fake parent, so CI on macOS and Windows
  covers the pump and threading but not embedding in a real NSView/HWND (that
  is the hand check with a real plugin); a nested modal loop inside
  `sendEvent`/`DispatchMessageW` (live resize, a menu, a window drag) stalls
  the control loop, so plugin timers and state captures wait until it ends; and
  a background host may not get focus (Windows refuses `SetForegroundWindow`
  without `AllowSetForegroundWindow` from the app; macOS 14+ activation is
  cooperative: the app should `yieldActivation(to:)` the host) which the hand
  check will show. `editor_open` reaches the app
  through the shared header, at most 100 ms stale.

Costs and limits: a block's hand-over is a copy in and out plus two wake-ups,
a few tens of microseconds, so a plugin in a host costs a little more CPU than
one in-process (and more so with tiny buffers); the host and the app each hold a
copy of the plugin's audio buffers; the host's priority is raised like a worker
(below) but its plugin threads' aren't; a hung plugin costs a 2 s outage before
it is replaced; plugin latency changes after load are still not followed; the
loopback socket is protected by a token only (another local process can connect first and make
a start fail, nothing more). The macOS and Windows
paths are checked by CI building and running the tests (the same tests as
Linux except the editor ones).
