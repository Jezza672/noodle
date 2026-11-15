# Noodle roadmap

Open work only. Everything delivered (M0 to M11 and the batches after them)
is recorded in [features/](features/README.md); the design as it stands is in
[ARCHITECTURE.md](ARCHITECTURE.md). When a piece of work here is finished, move
its record to a file in `features/` and delete it from this page.

All milestones M0 to M11 are built in code. What is left is the feature
items below that make Noodle usable in place of another DAW, planned
features with issues, checks that need a person and hardware, and a few
deliberately deferred items.

## Sample packs

**What it is.** A browsable, downloadable library of sounds, with sources the user adds (ADR 0012). Samples are stored by content hash as FLAC, one immutable object each. Pack definitions live in git and publish from main to R2. A source is a URL plus optional credentials, or a folder on disk. Chunks, in order:

1. **Design** (this ADR, the roadmap and the glossary).
2. **`noodle-network`:** the shared downloader and S3 client (renamed from `noodle-download`; consent, streaming, SHA-256, content-keyed cache), with the stem separator moved onto it.
3. **`noodle-packs`:** pack definitions, manifests, licence and credit fields, validation.
4. **`noodle-publish`:** built (docs/architecture/packs.md). Owed: add the repo secrets `R2_ACCOUNT_ID`, `R2_ACCESS_KEY_ID`, `R2_SECRET_ACCESS_KEY`, `R2_BUCKET` for the first real publish, and decide how contributors other than Jeremy get an upload credential.

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
- Hot starts: 0 and the loop start; project markers (they exist now).

**Batches.**

1. **The invariant and the keys.** Built: see [features/playback-cache.md](features/playback-cache.md).
2. **Automatic group targets.** Built: see [features/playback-cache.md](features/playback-cache.md).
3. **Tiers and cross-start reuse.** Built: see [features/playback-cache.md](features/playback-cache.md).
4. **Recursive partition.** Built: see [features/playback-cache.md](features/playback-cache.md). Expensive-node and chain cuts, a measured cost model and section alignment are left, after the first real projects are measured.
5. **Visibility and docs.** Built: see [features/playback-cache.md](features/playback-cache.md).

**Known limits.** Live branches and anything downstream stay live. Feedback loops are uncached. Pause→play is uncached. The first pass is always computed. A plugin that cannot return to a fresh state is unbounded or uncacheable. Cache entries are per machine, because transcendentals differ across platforms.

## Usability scorecard

**What it is.** Twenty-eight live user stories (thirty, two retired) for advanced DAW users (`docs/usability/stories.md`), each driven through the app by one or more scripted *routes* that use only real pointer and key events. Every route logs its actions; a scorer turns the log into a scorecard of effort, repetition, rework, the cost of late changes, findability, feedback and robustness. Routes are app tests, so CI keeps them current, and each PR shows how it moved the scorecard. The design is in `docs/architecture/usability.md`; the work is tracked in #306.

**MVP:** six stories routed (S01, S07, S09, S12, S19, S20). Routes are written for S01, S09, S12, S19 and S20 (S12 is `full`, the rest `partial`); S07 is `full` (markers and sections, #329). Where a story is possible but awkward, the route takes the workaround so the scorecard shows its cost: S20 trims with a second gain stage (#333). S01 records its takes through the virtual input (#334). S19 models the drum-bus fader as an ordinary Gain, so a send is a fan-out wire taken before or after it; it stays `partial` for plugin delay compensation (the latency badges show it, #429; the route does not use a latent node) and adding a drum track. Built: the action log, the route framework and registry, the effort scorer (group A), the report in CI (#308, #309, #310, #314), and the repetition (per extra item), rework and late-change scorers (#311, #312, #313).

**Still owed from groups B to D:** the repetition ratio and per-route batch-path notes (#311), and the premature-commitment and destructive-step tags (#313).

**Built since (#317, #318, #320, #321):** entry-point surfaces, the vocabulary match, time to hear, live editing, state visibility, dropouts, and save and reopen.

**Still owed from groups E to G:** the Fitts, precision and typed-value parts of group A (#310); the naive-agent runs for stories other than S12, and a run with user docs available (#319; the driver and first S12 results are in `docs/usability/new-user-runs.md`).

**Built since (#322, #324):** the scorecard diff on PRs; the Cognitive Dimensions rubric and the human-session protocol (`docs/usability/rubric.md`); and a status for every story in the registry, assessed from the docs and code.

**Then:** routes for the 24 stories that have a status but no route (U10, #316, #368), more naive-agent runs (#319), grades and thresholds once there is data for every story (#323), and the cross-DAW baseline (#325, optional).


## Planned: new nodes and MIDI files

Seven features picked from a shortlist in November 2026. Each issue holds the design, the tests and the open questions; the chunks below are one PR each. Every instrument here ships as a group template built from the new primitive, never as a synth node. They are independent of each other, so the order is only a suggestion: quick wins first, then the two sound primitives, the looper and the morph.

**MIDI file import and export (#540).** Drop a .mid on the timeline or use File > Import MIDI; export a clip, a track or the arrangement.

1. Reader and writer in `noodle-io` (`smf.rs`, on `midly`), the mapping to `MidiClip` and `TempoMap`, timeline drop and menu import as one undo step, export, and `noodle midi-export`. Size L.

**Microtuning (#539).** A Tuning node on the events wire retunes notes from a Scala file by adding a per-note pitch expression, so Voices, Mono Note, the Sampler and plugins that honour tuning follow it.

1. The node, the `.scl` and `.kbm` parser, the loader in the properties panel, an example with a golden render. Size M.

**Harmony and chance nodes (#538).** Scale Quantise, Chord, Transpose, Euclidean and Chance on the events wire, and a Song Key source node (named apart from the existing Key node, which makes notes from a gate) that puts the timeline's key on a wire (the pattern of ADR 0008).

1. Scale Quantise, Chord, Transpose, the Song Key node and `Transport::key`. Size M.
2. Euclidean, Chance, the Harmoniser and Euclid Hat templates, an example with a golden render. Size M.

**Wavetable oscillator (#536).** `noodle.osc.wavetable`: a table is a sample in the store, `position` is a parameter, band-limited copies are built off the audio thread.

1. The oscillator, the table builder, an example with a golden render. Size M.
2. Generated CC0 tables committed as system samples with their generator, the Wavetable Synth template and presets, the cycle drawn on the node, drop a table on the node. Size M.

**Grains (#537).** `noodle.instrument.grains`: a granular sample player with position, spray, size, density, pitch and scatter as parameters, one cloud per voice.

1. The node, an example with a golden render. Size M to L.
2. The Grain Pad and Grain Freeze templates, the waveform and grains drawn on the node, the S17 route. Size M.

**Looper (#541).** `noodle.util.looper`: record, loop on the bar, overdub, undo the last layer and clear, driven by gate parameters. The recording is session state and survives a seek, so the node is exempt from the reset-equals-fresh tests.

1. ~~The node.~~ Done: see [features/looper.md](features/looper.md).
2. The Looper template (Buttons, Launch Quantise, Mono Note for the gate), the state display, the S27 route. Size M.

**Preset morph (#543).** "Morph between presets…" on a group adds one Morph node (`noodle.mod.morph`) with an output wired into every parameter that differs between two group presets, and exposes its `position` as a group input. Nothing is modulated without a wire; a parameter that already has a wire or a lane, and all config, stay out.

1. ~~The node, `Edit::MorphPresets`, the picker.~~ Done: see [features/preset-morph.md](features/preset-morph.md). Still open from this chunk: the node's panel listing the rows with A and B sliders, and the S13 route using the morph.
2. The XY form for four presets with a pad widget, Set from current, Randomise. Size M.

## Planned: Noodle as a plugin (#544)

Noodle loaded in another DAW: the node editor, properties, library and Sounds panel in the host's window, no arrangement, with Host In, Host Out and Host MIDI nodes already in the graph. CLAP first; VST3 and AU are made from the CLAP with `clap-wrapper`. The issue holds the design and the open questions (whether an instance is a project or a group is the main one).

1. Shell split and host backend: `noodle-app` as a library with a desktop shell, a host audio backend in `noodle-io`, Host devices and ports on the io nodes. Size M.
2. A headless CLAP (`noodle-plugin`): ports, process, state, latency, default projects, tested by Noodle hosting Noodle. Size L.
3. Transport and tempo from the host, and a 64-slot bank of host parameters surfaced as Host Parameter nodes. Size M.
4. The window: the editor-only layout drawn in the host's window, focus, text, drag and drop, scaling. Size L, the main risk.
5. More than one instance in a process: the UI waker and the other process-wide state. Size M.
6. VST3 and AU through the wrapper, packaging, and an owed pass in real hosts on the Mac. Size M.

## Planned: feedback regions (#547)

A wired feedback loop as tight as the patch asks for, with no setting. The compiler groups the nodes on a loop into a region and runs only those in chunks as short as the loop's shortest delay, so a long echo costs what it does today and a plucked string above 375 Hz becomes possible as a group. A one-sample Feedback node makes delay-free loops legal.

1. Regions and Delay: the region step in the compiler and the plan, `Node::loop_chunk`, Delay without its one-block floor inside a region, events, silence, meters and parallel processing through a region. Size L.
2. The Feedback node (`noodle.util.feedback`), and the Plucked String, Comb and Flanger templates with examples and golden renders. Size M.

## Planned: low-latency monitoring (#548)

The least delay the hardware allows while a track is armed or monitored, chosen automatically, and a larger device buffer when nobody is recording. One setting, Monitoring latency (Auto, Always low, Off). The engine block stays 128 so cache entries stay valid; the device buffer is what moves. (So the deferred idea below, of the engine block following the device period, is not part of this.)

Chunks 1 and 2 are built (`features/low-latency-monitoring.md`). Owed on the Mac: the gap when arming reopens the stream, and a power measurement of the relaxed buffer.

3. Only the live cone runs in the device callback. Today a cluster with no entry plays live until the background `Freezing` render finishes and swaps in; this adds a streaming render that keeps just ahead of the playhead and is played while it fills, with an ADR for the change to that rule. Size L, and it may want two PRs (the streaming entry, then the hand-over on edits and shortfalls).

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
- **Create drum rack from clip** (`docs/design/drum-rack-from-clip.md`): on a real
  drum recording, right-click the clip, Create drum rack from clip, and listen to
  the rack: the groups are sensible, the guessed pads are right, the picker
  changes the hit, and the rack sounds like the track's kit. Never produce real
  audio there without asking.
- **Drum rack from clip, follow-ups** (`docs/design/drum-rack-from-clip.md`): a small
  classifier of our own, (the model-free template matching and the separation analyser are built). Also listen to velocity layers and the groove clip on the
  Mac, and listen to the dialog's new audition (it plays straight to the
  monitor device) on the Mac: ask before making sound.
- **Listen to the drum separation models on real kits** (`docs/features/drum-separation-models.md`):
  dry, loud rock with ringing cymbals, electronic, room-miked live. The tensor
  layouts are checked against the real files; only the listening is owed. Also
  consider streaming overlap-add in `MaskSeparator` if long clips run out of memory
  (about 600 MB for 5 minutes at 4096/1024).
- **Drum separation analyser, listening on real kits** (#501, built;
  `docs/features/drum-separation-analyser.md`): run Find overlapping drums on the same
  kits. Check that hits with two drums are marked and clean ones are not, and that
  Use part… sounds like the one drum. The 20 dB `ACTIVE_WITHIN_DB` margin was set on
  synthetic parts and may need tuning. Ask before making sound.
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
- **Bring-your-own separation model:** let users load a model file of their own
  for the drum rack. Needs a stable, fairly universal interface definition (parts,
  STFT front end, chunking), so it waits until the three built-in models show what
  that has to cover. The MDX23C DrumSep is not supported (no licence, gone upstream).
- **Piano roll and recording:** the 128-frame engine block is fixed; it could
  follow the device period.
- **VST3:** an editor window on Linux (the app makes none).
- **Sandbox monitor with no tick** (see `features/code-cleanup.md` and
  `features/deferred-items-2.md`): it wakes every 250 ms while a host is healthy.
- **Flaky test** #138 stays open (no recurrence).
