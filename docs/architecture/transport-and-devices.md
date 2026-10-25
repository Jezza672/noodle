# The transport, devices, surround and monitoring

The **engine** is the audio stream that runs the graph; the **transport** is
the moving timeline (see CONTEXT.md and ADR 0001). They are separate. The app
starts the engine at launch (`Session::start_engine`, called by `main`; a
`Session` made for a test or a headless run starts with it off) and the
toolbar's Audio toggle turns it off (`Session::stop_engine`). Play, Stop and
rewind only touch the transport:

- **Play** (`Session::play`) starts the engine first if it's off, then the
  transport. **Stop** pauses the transport in place, and **Stop again**
  rewinds. Neither closes the stream, and notes held on the keyboard keep
  sounding through them. (A rewind is a seek, and a seek resets every node, so
  it ends a held note along with reverb tails; the key's note-off is harmless.)
- **An engine opened from the toggle starts with the transport stopped**
  (`noodle_io::play_with_outputs_stopped` stops it before the first block, so a
  clip at the start of the timeline doesn't blip). A change of device
  (`reopen_stream`) keeps the transport as it was. A new project seeks to the
  start and leaves the transport running or stopped as it was.
- **Idle cost.** A graph with nothing to do renders silence, which the engine
  already skips (a silent lane is a branch and a memset), so an idle engine is
  cheap. A graph that makes sound on its own, like a drone, sounds as soon as
  the engine is on; that's the price of ADR 0001. The node editor repaints
  (at most 30 times a second) only while something it shows is moving or
  falling: `Telemetry::activity` goes up when a meter, scope, parameter or
  position writer reports a change, and `Bodies::update` says `Moving`,
  `Decaying` (peaks, holds and spectra still falling) or `Still`. A silent
  graph with the transport stopped only looks again every 250 ms, to
  catch sound that starts without input, such as live MIDI. The playhead keeps its own
  repaint while it plays.
- **The window sleeps until there is news.** Background work wakes it:
  `noodle_engine::wake` (the app registers a repaint request) is called when a
  `Job` ends (freezes, exports, stem splits, renders), when the plugin scan
  finishes and when the audio file watcher sees a file change. For what has to
  be looked at, `Session::next_deadline` says when: the next frame while the
  transport plays, a recording is open or a plan waits for room in the queue; one
  second for the stream's health and for MIDI hot-plug; the freeze debounce;
  half a second for sandboxed plugin hosts. `App::show` asks for one repaint at
  the earliest, so an idle project with the transport stopped runs about one
  frame a second, plus the editor's own liveness repaints above.
- **The fallback.** `Session::start_engine` tries the saved output, then the
  system default (as `play` always did), then the **virtual output**. Either
  fallback is not saved, shows a message in the status bar, and sets
  `Session::engine_warning`, which turns the Audio toggle amber until the
  engine is closed. The saved device is tried again next time the engine
  opens, but not while it runs.
- **The virtual output** (`noodle-io/src/output.rs`, ID `virtual`, listed
  last in every device list as "Virtual (no sound)") runs the same
  `DeviceWriter` from a timer thread, once per buffer's duration (256 frames
  by default, 2 channels, 48 kHz unless the settings say). It discards what
  it renders. It keeps the real thing's behaviour, the fade-out on stopping,
  the `AudioClock` windows and anchors MIDI timing and recording read, and the
  pace: real time, not flat out, since MIDI stamps and the record anchor
  depend on the clock moving at audio speed. It is for machines with no
  device, CI, tests and `noodle play --output virtual`. Output nodes tied to
  other devices and audio input still open as usual around it.

## Live input latency

Input is its own cpal stream, so it crosses to the output callback through a
ring (`noodle-io/src/input.rs`): `Capture` fills it, `Feed` empties it one
engine block at a time. The time from a sound entering the input to it leaving
the output is the input's capture delay, the wait in that ring, and the
output's delay.

- **Buffer size.** `AudioConfig::buffer_size` applies to the input stream as
  well as the output (`buffer_request` moves it into the span the input takes;
  a device that doesn't say keeps its default). With no size chosen, both ask
  for `LIVE_BUFFER` (256 frames) when the device says it takes that, and
  otherwise leave it to the device: a size it never promised could fail the
  stream. If the device refuses the automatic size when the stream is built, the output
  and the input each retry once with the device's own choice; a size the user
  chose fails loudly instead.
- **Margin.** The feed's controller (see "Live input") holds the smallest
  backlog at what is needed: one input callback plus one engine block
  (`Feed::margin`), so a large input buffer isn't forced to sit a block behind
  and a small one sits closer. Until the input has called back, the fixed 64
  frames stand.
- **Readout.** `InputMeter` holds the input callback's size and the backlog
  after each read, as relaxed atomics, so the callbacks publish them without
  locking. `Playback::latency_report` combines them with the output period and
  the drivers' own latencies into a `LatencyReport`. Where a driver reports
  zero (ALSA does), the buffer's duration stands in. `round_trip` is
  `input + (backlog + block) + output`: an estimate, not a measurement. The
  Audio Settings dialog shows it live with its parts, and the Audio button's
  tooltip shows the figure.
- **Bluetooth.** The dialog warns when a chosen device's name looks like
  Bluetooth (`looks_like_bluetooth`) or the running output reports over
  `SLOW_OUTPUT` (80 ms). The delay is the link's, which Noodle cannot remove.
  A Bluetooth headset's microphone also runs at 16 or 24 kHz, and the input
  still has to match the output's rate until it can resample.

## Playing on several devices

Output nodes tied to other devices (see "Outputs view") make the engine render
one buffer for every device's channels: the main device's first, then each
extra's. The main device's callback plays its channels and hands the rest to
the extras through a queue each (`Tap` in `noodle-io/src/output.rs`), and each
extra's own callback (`ExtraOutput`) plays from its queue. The two clocks
drift, so an extra waits for a cushion (one main block and one of its own,
scaled to its rate) before it plays, plays silence if it runs dry and skips
ahead if it falls far behind, counting each as a glitch.

An extra device opens at the main device's rate if it runs at it, and at its own
default rate otherwise (`open_extra`). Then a `RateConverter` thread sits
between the tap's queue and the extra's: it reads the engine's audio, converts
it with a `StreamResampler` (rubato's sinc resampler, 128 input frames a time)
and queues the result for the device. The cushion grows by what the converter
holds back (its chunk and its timer), so a converted device is a few
milliseconds later than a direct one; the engine doesn't
compensate for either. Neither callback allocates or resamples; dropping the
extra's callback stops the thread.

## Surround and multichannel buses

The engine has always carried any number of channels per signal; surround adds
the conventions and nodes that make them usable.

- **Layouts** (`noodle-engine/src/channels.rs`, `ChannelLayout`): Mono, Stereo,
  3.0, Quad, 5.1 and 7.1, each a list of `Speaker`s in WAV channel order (5.1
  is L, R, C, LFE, Lb, Rb; 7.1 adds Ls, Rs). A signal's layout is implied by
  its channel count (1, 2, 3, 4, 6, 8); other counts are plain channels with no
  layout. A layout knows each speaker's azimuth (degrees from straight ahead,
  positive to the right; the LFE has none), its WAV channel mask, and two
  pure functions the nodes use: `pan_gains` (constant power between the two
  speakers either side of a direction, a hard pan at the edges of a layout
  with nothing behind, `spread` blending toward all speakers equally, never
  to the LFE) and `convert_matrix` (the usual downmix recipe: shared
  speakers pass, the centre of a layout without one goes to L and R at -3 dB,
  a missing rear pair folds onto the other rear pair or onto the front on its
  side at -3 dB, stereo into mono sums at -3 dB, the LFE dropped where there
  is none, speakers the source lacks stay silent; no normalising).
- **Nodes** (`noodle-nodes/src/surround.rs`). `Surround Pan`
  (`noodle.util.surround_pan`; config `layout`, default 5.1; parameters
  `azimuth` (wired signals move it along its range, so a full-scale LFO circles
  the listener), `spread` and `lfe` (a send to the LFE in dB, off at its
  minimum)) pans the average of its input channels per voice. `Convert Layout`
  (`noodle.util.convert_layout`; config `layout`, default Stereo) converts
  from the layout the input's channel count implies; a count with no layout is
  a compile error. `Split Channels` and `Merge Channels`
  (`noodle.util.split_channels`/`merge_channels`; config `channels`) take a
  multichannel wire into one-channel outputs and back (the config is the
  channel count, since it changes the ports). The properties panel shows the
  layout as a drop-down. `Mix` needs equal channel counts or one (the
  existing broadcast rule), so convert before mixing different layouts.
- **Outputs.** An Output node's channel n goes to device channel n. A mono
  signal goes to the first two channels only (not the centre, LFE and rears of
  a surround bus); a signal with fewer channels than the bus leaves the rest
  silent. A signal with *more* channels than the bus is downmixed through
  `convert_matrix` when both counts name layouts (a matrix made when the plan
  is built, so nothing allocates on the audio thread): 5.1 on a stereo
  device, or a stereo project exported as Mono, folds down instead of losing
  channels. Counts with no layout (say 5 into 2) still drop the extras. Plugin nodes keep their own
  rule (a mono wire into a stereo port, any other mismatch an error), so a
  surround mix goes through Convert Layout before a stereo plugin.
- **Devices.** `AudioConfig::channels` opens the main device with that many
  channels (the device's default, usually two, if `None`); `choose_config` takes
  it as the first preference and refuses a count no configuration has
  (`AudioError::UnsupportedChannels`) rather than guessing. The audio settings
  have an "Output channels" drop-down (1, 2, 4, 6, 8 and the device's maximum),
  `noodle play --channels N` the same, and the virtual output takes 1 to 8
  (`testkit::virtual_audio()` stays stereo unless told otherwise), so a surround
  render can be captured with no hardware. Extra devices still open at their
  default channels.
- **Export.** The export dialog has a Channels drop-down (a layout) and
  `ExportChoice::layout` sets the render's channel count; WAV and FLAC take up
  to 8. `noodle render --channels N` writes the same. Hound writes the
  `WAVE_FORMAT_EXTENSIBLE` mask as the first N speaker bits, so after
  finalising `set_wav_channel_mask` overwrites it (4 bytes at offset 40) with
  the layout's own, which matters for quad and 7.1 (WAV only; FLAC has no mask
  here).
- **Meters.** The Mix node's and the meters' readings average a bus's
  channels' power (peak is the loudest channel) and are not weighted by
  layout.
- **Not done:** per-speaker delay and level for room calibration, bass
  management (a crossover to the LFE), ambisonics, binaural rendering, a
  channel-mask field in exported WAVs matching the layout, and playing extra
  devices as part of one surround bus.

## Node monitoring

The speaker button in a node's header (any node with an audio output) listens
to its first audio output through the **monitor device**, chosen in the top
bar (default: the main output, saved in the preferences). It is session state
(`Session::node_monitor`), not project state: not saved, not undone.

- **How it taps.** `Controller::set_monitor` hands the compiler a `Monitor`
  (`noodle-engine/src/monitor.rs`). After flattening it adds a hidden Output
  node (marked with the `monitor` config key) per monitored node, wired to the
  node's flat output. For a group that is the output after its gain, mute and
  solo stage, so a node that is not sounding in the mix is silent in the
  monitor too. A node inside a group that is muted, or that solo leaves out, is not tapped at all.
  Hidden Outputs sum onto a device other Output nodes play on, instead of
  being refused as a second owner. Offline renders never include a monitor.
- **Clicks.** Plain: monitor that node alone, summed into the mix; again
  stops. Shift: add or remove a node. Ctrl/Cmd: alone, and the Output nodes
  on the monitor's device are silenced (when the monitor has the device to
  itself this changes nothing). Ctrl+Shift: alone, and every Output node is
  silenced. Right-clicking the button offers the same modes as menu items (monitor, add to monitor, silence the device's other outputs, silence all, stop). Escape or the top bar's Stop button ends it.
- **Devices.** A monitor on a named device opens a stream for it like an
  Output node would (`Session::wanted_devices`).

## Live input

The input device is a separate cpal stream (`noodle-io/src/input.rs`). Its
callback (`Capture`) queues samples in a lock-free ring at the device's own
rate; the output callback (`Feed::read`) takes them out one engine block at a
time. `open_input` asks for the engine's rate and falls back to the device's
default, so a 24 kHz Bluetooth mic works against a 48 kHz output.

`Feed` converts in the output callback, with a rubato `Async` sinc resampler:
a short kernel (32 taps, linear table interpolation, well under a millisecond
of delay) that makes 16 output frames a call, so a block of any size is served
from whole calls and a few frames held over. It has no thread of its own and
nothing in it allocates: the buffers are sized up front, and
`noodle-nodes/tests/realtime.rs` runs it under the counting allocator. It is
always on, even at equal rates, because two nominally equal clocks still drift.

The ratio is steered, not fixed. Every 50 ms the feed looks at the smallest
backlog left after a read and compares it with the margin (one input callback plus one block, above).
A PI controller moves the ratio by up to 5%: the proportional term pulls in an
offset in about half a second, the integral term learns the clocks' steady
difference (hundreds of ppm in practice), so the backlog holds at the margin
with no frame dropped or repeated. Backlog 50 ms or more over the margin (a
stream that opens early, or a stall) is cut back at once as a last resort. A
gap, where the input is late, plays silence. Both count as input glitches in
`Health`.

The recorder taps the device's samples before conversion and writes them at the
device's rate; import converts them like any other file.

## Live monitoring

A track **armed for MIDI** (RM on its header, `Session::arm_kind`) plays live MIDI
through its own synth, with no `MIDI In` node and without the transport:
`ClipFeeds::set_monitoring` switches a flag in the track input's shared state,
and a track input built with `TrackInput::with_midi` (the library's is)
subscribes to the `MidiBus` like a `MIDI In` node and merges what it hears
into its `midi` output. It takes every source (a port, the computer keyboard)
on every channel.

- The code that turns bus messages into events, with the timing, the held-note
  bookkeeping and all notes off, is `LiveMidi` in `midi_in.rs`, shared with
  the `MIDI In` node. A track that isn't monitored still empties its queue and
  drops what it finds, and lets go of live notes it holds when it's disarmed.
- Live note IDs have their own bit (`0x2000_0000`: clips use the low 29 bits,
  live monitoring the next, the piano roll's keyboard the next, `MIDI In` the
  top bit), so a clip's note-off never ends a live note and the reverse.
- The track input inserts events in time order (`EventsOut::insert`), because
  live events and a clip's notes in one block arrive from two sources.
- **Doubling.** A `MIDI In` node wired to the same synth as an armed track
  sends every note twice under two note IDs, so a polyphonic synth sounds it
  twice. That's allowed: the wiring is the user's. Disarm the track, or
  remove the node.
- The arming is session state and not saved; a new project starts with no
  track armed.
