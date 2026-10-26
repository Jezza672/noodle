# Noodle roadmap

Open work only. Everything delivered (M0 to M11 and the batches after them)
is recorded in [features/](features/README.md); the design as it stands is in
[ARCHITECTURE.md](ARCHITECTURE.md). When a piece of work here is finished, move
its record to a file in `features/` and delete it from this page.

All milestones M0 to M11 are built in code. What is left is mostly checks that
need a person and hardware, plus a few deliberately deferred items.

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
3. **Tiers and cross-start reuse.** Pure entries valid for any start; bounded pre-roll from `S−n` with validity ranges; unbounded per-start entries; the tiered loop rule.
   *Done when:* a seek to a new start reuses the pure and bounded clusters and re-renders only the unbounded ones; a loop's reverb tail crosses the wrap while oscillator phase and voice state reset.
4. **Recursive partition.** Cut selection with group, join and fan-out cuts, the cost threshold, sink-first rendering and a sticky partition. Expensive-node and chain cuts, and section alignment, after the measured cost model.
   *Done when:* a drum rack or shared send is cached once and read twice; a cheap chain is not cached; moving or renaming a node does not invalidate the partition.
5. **Visibility and docs.** Settings toggle, size and eviction readout, a "from cache" indicator and an underrun counter. Add the playback section to `docs/architecture/caching.md` and a feature record when it lands.
   *Done when:* the user can see what is cached, turn it off, and tell when playback is live.

**Known limits.** Live branches and anything downstream stay live. Feedback loops are uncached. Pause→play is uncached. The first pass is always computed. A plugin that cannot return to a fresh state is unbounded or uncacheable. Cache entries are per machine, because transcendentals differ across platforms.

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
- **Piano roll and recording:** quantise, more
  than one MIDI port at a time, overdubbing into an existing clip, a count-in,
  keeping the raw take for re-quantising. The 128-frame engine block is fixed;
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
