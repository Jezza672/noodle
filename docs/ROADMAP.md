# Noodle roadmap

Open work only. Everything delivered (M0 to M11 and the batches after them)
is recorded in [features/](features/README.md); the design as it stands is in
[ARCHITECTURE.md](ARCHITECTURE.md). When a piece of work here is finished, move
its record to a file in `features/` and delete it from this page.

All milestones M0 to M11 are built in code. What is left is four feature items
that make Noodle usable in place of another DAW (in order: MIDI
controllers in clips, Takes and comping, Track export, MIDI
learn and output), checks that need a person and hardware, and a few
deliberately deferred items.

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

## MIDI controllers and expressions in clips

Batches 1 and 2 (the data, playback, chase and release, and recording) are built: see [features/midi-controllers-in-clips.md](features/midi-controllers-in-clips.md). What is left is the editing UI.

**What you get.** The piano roll shows a lane for each controller the clip uses, with "+" to add one, and a lane of expressions for the selected notes.

**Scope.** In: piano roll lanes with draw, line, select and copy. Out: SysEx, converting between controller data and automation lanes, curves drawn on the notes themselves (later).

**Batches.**

3. **Controller lanes in the piano roll.**
4. **Expression lane for the selected notes.**

**Done when.** You record a piano part with sustain pedal and mod wheel; it plays back the same, including after a seek into the middle of a held pedal; and you edit the pedal in its lane.

## Takes, loop recording and comping

**The problem.** Record makes one clip per armed track per take, and a seek or a loop wrap ends the take. There is no punch range, no loop recording that keeps every cycle, and no way to build one performance from the best phrases of several. This replaces the overdub and count-in parts of the deferred "Piano roll and recording" item.

**What you get.** Record with the loop on and every cycle becomes a take, stacked in a take folder on the track. Drag across a take to make that phrase the one that plays. Punch in and out at a range, with a count-in and pre-roll. For MIDI, choose whether cycles become takes or merge into one clip.

**Settled decisions.**

- A take folder is one clip holding its takes and a comp: a list of spans, each naming a take. Moving, splitting or copying it carries every take. Flattening gives a plain clip and drops the unused takes from it; the files stay on disk.
- One audio file per recording; each take is a window into it.
- MIDI cycles become takes or merge into one clip, set by one Takes or Merge record setting, an app preference. MIDI takes use the same take folders and swipe comping.
- The takes in a folder share one warp.
- The punch range is separate from the loop range: its own bar on the ruler, switched on and off. Count-in is 0, 1 or 2 bars of metronome (an app preference); pre-roll plays from a set number of bars before the punch-in point.
- Comp joins get a 10 ms crossfade by default, draggable per join, using the existing clip crossfade (`AudioClip::crossfade`).
- **Comping across tracks** uses Linked clips: a swipe on one linked take folder applies the same spans to all of them.

**Scope.** In: the take folder model and its scheduling (`schedule.rs`), loop recording in `noodle-io/src/record.rs` and the session, punch, count-in and pre-roll, MIDI takes and merge (`midi_take.rs`), take lanes and the swipe gesture (`timeline/take.rs`, `timeline/clips.rs`), flatten. Every comp edit is one `Command`. Cross-track comping builds on Linked clips ([features/linked-clips.md](features/linked-clips.md)).

**Batches.**

1. **Take folders in the model.** Data, comp scheduling, cache keys, flatten, golden render of a comped example.
2. **Loop recording and punch.** A take per cycle, punch range, count-in and pre-roll.
3. **Comping in the arrangement.** Take lanes, swipe, audition, one undo step per swipe, linked folders comped together.
4. **MIDI takes.** Takes or Merge for MIDI.

**Done when.** You loop four bars, record five vocal passes in one go, swipe the best phrase from each into one comp that plays without clicks at the joins, undo a swipe, and export it identical to playback. A comp across linked drum mics stays aligned. An app test records on the virtual output and checks the takes and the comp's samples.

## Track export and render to clip

**The problem.** Export writes one mixdown to WAV or FLAC (`noodle-nodes/src/export.rs`). Sharing a song with a mix engineer means one file per track or bus. Freeze plays a group from the cache but never becomes a clip, so you can't render a synth or plugin chain to audio and then cut or warp it. There is no loudness readout.

**What you get.** Export offers Mixdown, plus any tracks or groups you tick, written in one pass with matching lengths and names. Render to clip on a track, group or time range makes a new audio track with the rendered clip and mutes the original. The export dialog shows integrated loudness and true peak, can normalise to a target, and dithers at 16 or 24 bits.

**Settled decisions.**

- A track file is taken after the track's gain and mute, before the master group's processing. The mixdown is a separate file with the master processing on it.
- Any group can be exported, buses included.
- Render to clip makes a new audio track holding the clip; the original is muted and kept.
- Rendering a range continues past its end until the effects fall silent, capped at 10 s.
- Loudness and true peak are always shown; normalising is optional; dither is applied when writing 16 or 24 bits.
- No MP3 in this item.
- New terms: "track export" and "render to clip" (CONTEXT.md avoids "bounce", and "stem" is AI separation).

**Scope.** In: track files from one render through `render_taps` (`noodle-engine/src/render.rs`); Render to clip writing to a `<project> renders` folder, stored and keyed like any sample (ADR 0004), added as one `Command::Batch`; BS.1770 loudness moved from `noodle-eval/src/level.rs` to where export can use it; TPDF dither in `noodle_io::ExportWriter`. Out: MP3, OMF, AAF and DAWproject interchange, a render queue.

**Batches.**

1. **Track export.** Group selection in the dialog, one pass, one file per group, a test that the track files sum to the mixdown when the master group is empty.
2. **Render to clip.** Track, group and time range, tails, offline and frozen nodes included, one undo step.
3. **Loudness and dither.**

**Done when.** You export a project's tracks, buses and mixdown in one go; with no master processing, the track files sum to the mixdown within the golden tolerance. You render a plugin synth track to a clip and warp it. The dialog's loudness agrees with `noodle-eval` on the example songs.

## MIDI learn and MIDI output

**The problem.** A hardware knob drives a parameter only if you add a `MIDI Control` node and wire it in yourself, and only from the one MIDI port chosen in settings. There are no relative encoders, no pickup, and no MIDI output at all: external synths can't be sequenced and there is no clock out. This replaces the deferred "more than one MIDI port at a time".

**What you get.** Right-click a parameter, choose Learn MIDI and move a control: a `MIDI Control` node wired into it appears, so the mapping is in the graph and undoes like any edit. Several MIDI inputs are open at once. A `MIDI Out` node sends an events wire to hardware, in time, with clock if you want it.

**Settled decisions.**

- **Learn MIDI is a graph shortcut,** like Automate: it adds a `MIDI Control` node in the parameter's group, set to the controller just moved and the port it came from, and wires it into the parameter, as one undo step. Nothing else links them. It is disabled on a parameter that already has a wire. On an Automation node it wires into the `record` input, so the knob records into the lane.
- `MIDI Control` gets the same `min`, `max` and taper as the Automation node, copied once from the target.
- `MIDI In` and `MIDI Control` name their port as Output nodes name their device, defaulting to "All inputs".
- `MIDI Control` gains an encoder mode (absolute and the three common relative codings) and a pickup mode (jump, or wait until the knob passes the value). They change no ports, so they are parameters.
- `MIDI Out` takes an events wire and sends it to a named port, ahead by the output latency. Clock out is a toggle on the node, not a hidden setting.

**Scope.** In: several ports in `noodle-io/src/midi.rs` (one `MidiBus` subscription per port); the port setting, Learn in the session through `spare::wire`; encoder and pickup modes; `MIDI Out`, whose `process` pushes timestamped messages into a preallocated ring that a sender thread hands to `midir` (extend `tests/realtime.rs`); clock and start/stop out. Out: Mackie Control and HUI, motor fader and LED feedback, OSC, MIDI clock in and Ableton Link.

**Batches.**

1. **Several MIDI inputs.** Ports per node, the settings list, hot-plug per port.
2. **MIDI learn.** The menu entry, the listener, range, encoder and pickup modes.
3. **MIDI Out.** The node, the sender thread, latency offset, a loopback test on a virtual port where the platform has one.
4. **Clock out.**

**Done when.** With two controllers plugged in, you learn a knob onto a synth group's cutoff and an encoder onto an armed Automation node, see both as nodes in the graph, record the encoder into the lane, and sequence an external synth from a MIDI clip in time with the project.

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
- **AU editors** (`au/gui.rs`): open a third-party AU that ships its own
  Cocoa view (not just Apple's generic one), edit in it, close the window and
  see the edit kept and undoable; the same with "Run plugins in separate
  processes" on; resizing, and focus when the app is in the background.
- **AU leftovers** (needs a real Mac or a unit that exercises them): MIDI
  output from AU instruments, AUv3 async instantiation with `AUViewController`
  editors.
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
- **Piano roll and recording:** keeping the raw take so a recorded MIDI clip
  can be re-quantised afterwards (quantise on record is built; several MIDI
  ports, overdub and count-in are in the items above). The 128-frame engine block is fixed;
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
