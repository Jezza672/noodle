# Noodle roadmap

Open work only. Everything delivered (M0 to M11 and the batches after them)
is recorded in [features/](features/README.md); the design as it stands is in
[ARCHITECTURE.md](ARCHITECTURE.md). When a piece of work here is finished, move
its record to a file in `features/` and delete it from this page.

All milestones M0 to M11 are built in code. What is left is five feature items
aimed at making Noodle a daily replacement for other DAWs, checks that need a
person and hardware, and a few deliberately deferred items.

## Playback cache

**What it is.** Replaying a Pass with nothing changed does no DSP: playback streams cached audio and mixes it, but does not recompute it. Editing one part of the graph re-renders only that part. Every cacheable cluster becomes an automatic target. An entry is the audio of one cluster for one start, rendered from a fresh state at that start (ADR 0010). The existing per-user disk store, `CachedPlayer`, replacements and freeze chain carry it; the new work is the partition, the tier analysis, start-scoped keys, growing entries and the policy.

**The partition.** Flatten the graph, drop the live, nondeterministic and feedback region and everything downstream, then cut what remains into clusters. Cuts are chosen by benefit: group boundaries first (user intent), then joins (parallel branches) and fan-outs (shared upstream); expensive nodes and chain midpoints come later. A cut protects the upstream side from downstream edits, so its value is the upstream's render cost times the chance of a downstream edit, and its cost is the upstream's bytes times the number of hot starts it must exist for. The lower bound is render cost, not node count: storage is per audio-second, so a 20-node gain chain costs the same bytes as one reverb and is not worth caching, but we still recurse inside a cheap group to find the expensive clusters within it. The partition is carried across compiles and adjusted locally, so adding or moving a node does not reshuffle every cut.

**Tiers.** A node declares `Memory::None`, `Bounded(n)` or `Unbounded`; the compiler composes it along the longest path, and a feedback loop is unbounded. A `None` cluster is a pure function of the playhead, so one entry serves every start. A `Bounded(n)` cluster is warmed from `S−n` and one entry serves a range of starts. An `Unbounded` cluster is reset at S, so its entry is valid only for that start. The tier also decides the loop rule: bounded clusters warm across the wrap so their tails carry over, unbounded clusters reset at the wrap with a short fade.

**Entries.** Keyed by cluster, start, tier, config, sample rate and `max_frames`, with no rendered length for prefix-stable nodes. An entry grows as the pass plays; a lookup for a position takes the best entry whose validity range covers it. Live input branches stay live, and everything downstream of them is not cached. The store is the existing content-addressed disk cache, LRU-trimmed; the budget and priority policy are revisited when the first real projects are measured.

**Settled decisions.**

- The truth is the pass start, not sample 0 (ADR 0010).
- A cacheable pass begins with a seek; pause→play stays live and uncached.
- Automatic and on by default, with a settings toggle; explicit freeze keeps its current behaviour.
- Uncacheable branches are pruned rather than disabling the project.
- Clusters are top-level groups and tracks, cut before their gain, mute and solo stage; recurse inside a group when it is worth it.
- Cuts in v1: group, join and fan-out, with the cost threshold. Expensive-node and chain cuts after a measured cost model.
- Growing entries; no rendered length in the key for prefix-stable nodes.
- Hot starts: 0 and the loop start; section markers when they exist.

**Batches.**

1. **The invariant and the keys.** Built: see [features/playback-cache.md](features/playback-cache.md).
2. **Automatic group targets.** Built: see [features/playback-cache.md](features/playback-cache.md).
3. **Tiers and cross-start reuse.** Built: see [features/playback-cache.md](features/playback-cache.md).
4. **Recursive partition.** Built: see [features/playback-cache.md](features/playback-cache.md). Expensive-node and chain cuts, a measured cost model and section alignment are left, after the first real projects are measured.
5. **Visibility and docs.** Built: see [features/playback-cache.md](features/playback-cache.md).

**Known limits.** Live branches and anything downstream stay live. Feedback loops are uncached. Pause→play is uncached. The first pass is always computed. A plugin that cannot return to a fresh state is unbounded or uncacheable. Cache entries are per machine, because transcendentals differ across platforms.

## Automation recording and curves

**The problem.** A lane is drawn by hand, a point at a time, with linear or hold segments (`noodle_core::automation::Curve`). Nothing records a move: dragging a parameter, a mixer fader or a knob in a plugin's editor while the transport plays changes the sound and leaves no lane. A plugin editor's edits reach the project only when its window closes (`Captured::commands`), and are lost if the node is rebuilt meanwhile. Only boundary gain and mute and Automation nodes get rows under a track (`timeline/automation.rs`), so a synth's cutoff or a plugin knob is automated by wiring a free Automation node by hand.

**What you get.** Right-click any parameter (node, properties panel, mixer strip) and choose Automate: its lane appears under its track. An automation arm in the transport and a mode per track (Read, Touch, Latch, Write) record moves of any parameter widget, mixer fader, plugin editor control or mapped controller into lanes while playing, one undo step per pass. Lanes gain curved segments and range editing.

**Scope.** In: Automate from any parameter, lanes on any parameter shown under the track that contains it; live plugin parameter sync (CLAP `params` output events and gestures, VST3 `IComponentHandler` `beginEdit`/`performEdit`/`endEdit`, AU parameter listeners) as `SetParam`s, one undo step per gesture, without rebuilding the node; recording from `ParamField` (`widgets/param.rs`), `mixer.rs` and plugin editors; thinning recorded points to a tolerance; exponential and S-curve segments with a tension, evaluated by the lane source (`noodle-engine/src/automation.rs`); selecting, moving, scaling and copying several points or a time range. Out: automation clips (lanes stay arrangement data), per-clip automation, automating solo (still read at compile time).

**Settled decisions.**

- Recording lives on the UI thread. The widget sends its value to the engine as it does now; the session buffers the points and commits them as one `Command::Batch` when the pass ends (stop, seek, loop wrap or disarm), as a take does. The audio thread gains nothing.
- While a parameter is being written, the hand wins: its lane source is bypassed for that parameter until the touch ends (Touch) or the pass ends (Latch).
- Track modes are saved with the project; the arm is session state, like track arming.

**Batches.**

1. **Automate any parameter.** The context-menu entry, lane rows for any target inside a track, curve shapes in `Curve`, multi-point selection, scaling and range copy.
2. **Live plugin parameter sync.** Editor edits become `SetParam`s while the window is open, for CLAP, VST3 and AU, with no reload.
3. **Recording from Noodle's widgets.** Arm, modes, Touch and Latch from parameter fields and the mixer, thinning, one undo step per pass.
4. **Recording from plugin editors.** Plugin gestures start and end a touch.

**Done when.** With the transport looping you can arm automation, ride a filter cutoff in a synth group and a knob in a CLAP plugin's editor, stop, hear both moves play back from new lanes, and undo the pass in one step. An app test records a drag through `testkit` and checks the lane and the rendered level.

## MIDI controllers and expressions in clips

**The problem.** A MIDI clip holds notes and nothing else (`MidiClip::notes` in `noodle-core/src/clip.rs`), and a take keeps only notes (`midi_take.rs`). Sustain pedal, mod wheel, pitch bend, aftertouch and MPE expressions are heard while monitoring and then lost, so a recorded piano part comes back without its pedal. The engine already carries all of them (`EventKind::Channel`, note expressions, `MIDI Control`); only the clip cannot store them.

**What you get.** Clips keep controllers, pitch bend, channel and polyphonic pressure, program changes and per-note expressions. Recording keeps them. The piano roll shows them in lanes under the notes, and per-note expressions as curves on the note.

**Scope.** In: `MidiClip::controls` (per channel and controller, points of tick and value, hold or linear) and expressions on `MidiNote`, at 32-bit resolution; scheduling in `ClipFeeds` (`noodle-nodes/src/clip/schedule.rs`) out of the track input's `midi` output; chase on a seek or loop wrap (the last value of each controller before the playhead is sent, so a held pedal stays held); loop, split and trim rules matching the notes'; cache keys (`noodle-core/src/cache_key.rs`); takes keeping them; piano roll lanes with draw, line, select and copy (their own clipboard MIME type). Out: SysEx, turning controller data into automation lanes and back, a separate MPE editor.

**Batches.**

1. **Data and playback.** The model, scheduling with chase, loop, split and trim, cache keys, and `examples/midi-cc.ron` (pedal and mod wheel into a synth group through `MIDI Control`) with its golden render.
2. **Recording.** Takes keep controllers and expressions; the take quantise moves expressions with their notes and leaves controllers alone.
3. **Controller lanes in the piano roll.**
4. **Per-note expressions in the piano roll.**

**Done when.** You record a piano part with sustain pedal and mod wheel from a keyboard; it plays back the same, including after a seek into the middle of a held pedal; and you edit the pedal in its lane.

## Takes, loop recording and comping

**The problem.** Record makes one clip per armed track per take, starting at the playhead. A second pass over the same bars is just another clip. There is no punch range, no loop recording that keeps every pass, and no way to build one performance from the best phrases of several. Vocal, guitar and keys sessions in Logic, Reaper and Live depend on take lanes and swipe comping. This extends, and replaces, the overdub and count-in parts of the deferred "Piano roll and recording" item.

**What you get.** Record with the loop on and every pass becomes a take, stacked in a take folder on the track. Drag across a take to make that phrase the one that plays, with short crossfades at the joins. Punch in and out at a range, a count-in and pre-roll, and for MIDI, overdub that merges into the clip under the playhead.

**Scope.** In: a take folder as audio clip data (takes share one recorded file, each a window into it; the comp is a list of spans naming a take); the comp scheduled as butted sections with the existing crossfade machinery (`schedule.rs`, `AudioClip::crossfade`); loop recording in `noodle-io/src/record.rs` and the session (one file, a take per wrap); punch range on the transport (`TransportLoop` style, saved, through a command); count-in and pre-roll from the `Metronome`; MIDI overdub and loop takes in `midi_take.rs`; the take lanes and swipe gesture in `timeline/take.rs` and `timeline/clips.rs`; flattening a comp to a plain clip. Every comp edit is one `Command`. Out: comping across warped clips with different warps (a folder shares one warp), cross-track comp groups (later).

**Batches.**

1. **Take folders in the model.** Data, scheduling of a comp, cache keys, flatten, golden render of a comped example.
2. **Loop recording and punch.** One file per recording, a take per pass, punch range, count-in and pre-roll.
3. **Comping in the arrangement.** Expand a folder into lanes, swipe to choose, audition a take, one undo step per swipe.
4. **MIDI takes.** Overdub into the clip under the playhead and loop takes for MIDI.

**Done when.** You loop four bars, record five vocal passes in one go, swipe the best phrase from each into one comp that plays without clicks at the joins, undo a swipe, and export it identical to playback. An app test records on the virtual output and checks the takes and the comp's samples.

## Multitrack export and render to clip

**The problem.** Export writes one mixdown, stereo or a surround layout, to WAV or FLAC (`noodle-nodes/src/export.rs`). Sharing a song with a mix engineer, a label or a collaborator means one file per track or bus. Inside a project, freeze plays a group from the cache but never becomes a clip, so you cannot render a synth or a plugin chain to audio and then cut, reverse or warp it. There is no lossy format for demos and no loudness readout.

**What you get.** File > Export offers Mixdown, Tracks or Groups (pick which, plus the mixdown), written in one pass with matching lengths and names. Right-click a track, group or time selection and Render to clip: a new audio track with the rendered clip, and the source muted if you choose. The export dialog reports integrated loudness and true peak, can normalise to a target, and dithers when writing 16 or 24 bits.

**Scope.** In: track files from one render through `render_taps` (`noodle-engine/src/render.rs`), cut before or after each group's output stage as chosen; Render to clip writing to a `<project> renders` folder, stored and keyed like any sample (ADR 0004), added as one `Command::Batch`; BS.1770 loudness moved from `noodle-eval/src/level.rs` to where export can use it; TPDF dither in `noodle_io::ExportWriter`; MP3 through LAME (LGPL, GPL-compatible) if a maintained crate builds on all three platforms in CI. Out: OMF, AAF and DAWproject interchange; a render queue. "Render to clip" and "track export" are new terms (CONTEXT.md avoids "bounce", and "stem" already means an AI-separated part); add them to CONTEXT.md with the first batch that builds them.

**Batches.**

1. **Track export.** Tap selection in the dialog, one pass, one file per tap, a test that the track files sum to the mixdown.
2. **Render to clip.** Track, group and time range, offline and frozen nodes included, one undo step.
3. **Loudness and dither.** Readout, normalise and dither in the dialog.
4. **MP3.** Only if the encoder holds up on CI.

**Done when.** You export a project's tracks and its mixdown in one go, and the track files summed match the mixdown within the golden tolerance. You render a plugin synth track to a clip and warp it. The dialog's loudness agrees with `noodle-eval` on the example songs.

## Controller mapping and MIDI output

**The problem.** A hardware knob drives a parameter only if you add a `MIDI Control` node and wire it in yourself, and only from the one port chosen in the audio settings. There is no MIDI learn, no relative encoders, no pickup when a knob and its parameter disagree, and no MIDI output at all: external synths and drum machines cannot be sequenced, a plugin's MIDI output cannot reach hardware, and there is no clock out. This extends the deferred "more than one MIDI port at a time".

**What you get.** Right-click any parameter, choose Learn MIDI and move a control: it is mapped. The mapping is an ordinary `MIDI Control` node wired into the parameter, so it shows in the graph, undoes, and can be remapped or scaled like any wire. Several MIDI inputs are open at once, each `MIDI In` and `MIDI Control` picking its port. A `MIDI Out` node sends an events wire to a hardware port, ahead by the output latency, and can send clock.

**Scope.** In: several ports in `noodle-io/src/midi.rs` (one `MidiBus` subscription per port) and the audio settings; a `port` setting on `MIDI In` and `MIDI Control` (as Output nodes name their device; an absent port shows as a node problem, as Output nodes do); learn in the session, which listens for the next controller and adds the node through `spare::wire` as one undo step; `MIDI Control` parameters for encoder mode (absolute, the three relative codings), pickup and range, which change no ports and so are parameters; `noodle.event.midi_out`, whose `process` pushes timestamped messages into a preallocated ring and a sender thread calls `midir` (extend `tests/realtime.rs`); clock and start/stop out from the transport. Out: Mackie Control and HUI surfaces, feedback to motorised faders and LED rings, OSC, MIDI clock in and Ableton Link (later items).

**Batches.**

1. **Several MIDI inputs.** Ports per node, the settings list, hot-plug kept per port.
2. **MIDI learn.** The menu entry, the listener, encoder modes and pickup on `MIDI Control`.
3. **MIDI Out.** The node, the sender thread, latency offset, a loopback test on a virtual port where the platform has one.
4. **Clock out.** Transport start, stop and clock.

**Done when.** With two controllers plugged in, you learn a knob onto a synth group's cutoff and an encoder onto a plugin parameter in a few seconds each, see both as nodes in the graph, undo one, and sequence an external synth from a MIDI clip in time with the project.

## Owed: needs a person on real hardware

- **AU on the Mac** after the shared plugin bridge (#242, #241): an Audio Unit
  instrument and an effect play, go quiet when idle (the unit sets
  `OutputIsSilence`) and wake on a note; `cargo test -p noodle-plugins --test
  au --test au_realtime` on macOS.

- **Mac listening checks** for M0 to M2: a short `noodle play`, plus the batched
  macOS tests on the latest main. Never produce real audio there without asking.
- **AirPods mic on the Mac** (live input resampling, #36): select the AirPods
  as input and the built-in speakers or wired headphones as output (the AirPods
  mic runs at 24 kHz): an Input node plays through with no clicks or dropouts
  over several minutes, at the right pitch, and Health shows no input glitches.
  Never produce real audio there without asking.
- **Real MIDI hardware** (M3): a keyboard playing a polyphonic synth; device input
  as UMP is blocked on midir, which delivers MIDI 1.0 bytes (MIDI-CI likewise).
- **Real plugins** (Surge XT, Vital, Dragonfly) on Mac and Windows: CLAP and
  VST3 in-process, editors embedded and focused, and the same with "Run plugins
  in separate processes" on. CI does not cover the sandbox embedding; focus from
  a background host may need `AllowSetForegroundWindow` (Windows) and
  `yieldActivation` (macOS 14+).
- **AU leftovers** (needs a real Mac or a unit that exercises them): editors,
  MIDI output from AU instruments, AUv3 async instantiation.
- **`noodle profile --threads`** showing real-time workers on Mac and Windows;
  the macOS audio workgroup join is not done.
- **Recording with a real microphone**, to check the latency compensation, and
  that the Audio Settings round-trip estimate matches what you hear.
- **Stem separation on the real HT-Demucs weights** (M10), and listening to the
  stems. There is no Intel Mac ONNX Runtime build.
- **Warp evaluation:** CC0 clips for `eval/clips.csv` (`eval/fetch.sh --pin`),
  the IEEE DataPort download for `noodle-eval validate` and `fit`, and a
  listening check of the warp modes on the Mac.

## Deferred on purpose

- **Live input latency, part two (#36):** let the input run at its own sample rate
  (the 24 kHz AirPods microphone is off today) and steer a resampler on the
  backlog so drift costs no clicks, instead of trimming and silence gaps.

- **M10 batch 3, Stem warp** (a warp mode that separates, warps each stem with its
  own mode and adds them back). Research (`research/stem-warp/findings.md` in the
  project folder) finds no published gain over whole-mix warping, and the perfect-stem
  ceiling experiment (`features/m10-stems.md`, `noodle-eval songs --ceiling`) found a
  trade, not a clear gain: better spectral fidelity, worse transients, level overall.
  Prerequisite:
  real mixes with vocals in `eval/clips.csv`, and a run on real weights. It ships
  only if the evaluation shows a clear gain on most songs and clips. Not started.
  Out of scope: streaming separation, models other than HT-Demucs, note-level
  warping.
- **M11 follow-ups:** more spectral nodes (pitch shift and time stretch on
  frames, spectral compressor, convolution reverb from an impulse response,
  noise-profile denoiser) and a choice of frame size (a config on Analyze that
  every spectral node on the wire adopts).
- **Piano roll and recording:** keeping the raw take so a recorded MIDI
  clip can be re-quantised after the fact (quantise on record is built). (Several MIDI ports, overdub and count-in moved to the items
  above.) The 128-frame engine block is fixed;
  it could follow the device period.
- **Virtual output:** remember the live-monitoring toggle between launches and
  retry the saved device when it returns.
- **VST3:** an editor window on Linux (the app makes none).
- **Plugin editor keeps its plugin running:** a plugin nothing listens to no
  longer runs, so its editor's meters freeze. `always_runs` is per node type;
  true-only-while-the-editor-is-open needs it to be per instance.
- **Plugin latency changes:** a plugin's reported latency change (CLAP `changed`) is not followed until the node is rebuilt.
- **Beat view gain envelope.** The beat view draws no gain envelope (clip or
  sample).
- **Cleanup leftovers** (see `features/code-cleanup.md`): one `TimeAxis` for the
  arrangement, piano roll and clip editor (only the piano roll's snap is done), a
  sandbox monitor that sleeps with no tick, and one retry policy for files that
  fail to open.
- **Random Select editor behaviour** (#211): resolve the node's `kind` (signal or notes) from the first wire and grow `inputs` as you wire, as the mixer does (`spare.rs`).
- **Flaky test** #138 stays open (no recurrence).
