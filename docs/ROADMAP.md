# Noodle roadmap

Open work only. Everything delivered (M0 to M11 and the batches after them)
is recorded in [features/](features/README.md); the design as it stands is in
[ARCHITECTURE.md](ARCHITECTURE.md). When a piece of work here is finished, move
its record to a file in `features/` and delete it from this page.

All milestones M0 to M11 are built in code. What is left is the feature
items below that make Noodle usable in place of another DAW, checks that need a
person and hardware, and a few
deliberately deferred items.

## Sample packs

**What it is.** A browsable, downloadable library of sounds, with sources the user adds (ADR 0012). Samples are stored by content hash as FLAC, one immutable object each. Pack definitions live in git and publish from main to R2. A source is a URL plus optional credentials, or a folder on disk. Chunks, in order:

1. **Design** (this ADR, the roadmap and the glossary).
2. **`noodle-download`:** the shared downloader (consent, streaming, SHA-256, content-keyed cache), with the stem separator moved onto it.
3. **`noodle-packs`:** pack definitions, manifests, licence and credit fields, validation.
4. **`noodle-publish`:** build the static site, `noodle-cli packs build`, and the CI workflow that publishes from main to R2. The first real publish needs R2 credentials as repo secrets.
6. **Sources:** the settings page (URL, credentials, remove, storage use, Clean up), self-hosted sources, and folders added in place from the browser.
7. **Make shareable pack:** the wizard and its usability stories and routes (find a sample, add a source, make a pack).
8. **Clips use the hash store**, so a dragged clip needs no copy.

Later: Nodes, Groups and Presets as tabs of the same browser.

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
- **Drum separation models on real weights** (`docs/features/drum-separation-models.md`):
  fill in each model's address, SHA-256 and size, host the LarsNet ONNX conversion,
  check the tensor layouts with `tests/drum_runtime.rs`, and listen on real kits.
  Until the addresses are in, the Models tab shows "not available yet" and the
  first-use popup does not appear (#523).
- **Drum separation analyser, listening on real kits** (#501, built;
  `docs/features/drum-separation-analyser.md`): with real weights, run Find overlapping
  drums on a dry kit, loud rock with ringing cymbals, an electronic kit and a
  room-miked live kit. Check that hits with two drums are marked and clean ones are
  not, and that Use part… sounds like the one drum. The 20 dB `ACTIVE_WITHIN_DB`
  margin was set on synthetic parts and may need tuning. Ask before making sound.
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
- **Model controls for Split to stems:** the same enable/disable, download and
  storage controls in the Models settings tab, and a model choice in the stems
  dialog, as the drum rack gets. Not part of the drum rack batch.
- **M11 follow-ups:** more spectral nodes (pitch shift and time stretch on
  frames, spectral compressor, convolution reverb from an impulse response,
  noise-profile denoiser) and a choice of frame size (a config on Analyze that
  every spectral node on the wire adopts).
- **Piano roll and recording:** keeping the raw take so a recorded MIDI clip
  can be re-quantised afterwards (quantise on record and several MIDI
  ports are built; overdub and count-in are in the items above). The 128-frame engine block is fixed;
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
- **Flaky test** #138 stays open (no recurrence).
