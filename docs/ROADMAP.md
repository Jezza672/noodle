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

1. **The invariant and the keys.** Make `reset()` equal a fresh instance for every node (White Noise RNG, Reverb wobble, Arpeggiator RNG, Sampler, plugins' continuous time, `CachedPlayer`, `ValueInput` ramps). Add `Layout::memory()` and the three tiers, composed along the longest path. Close the key gaps (`max_frames`, `AudioClip.extent`, MIDI loop fields, `ClipWarp.loop_region`, monitoring, preview, plugin latency) and mark `TrackInput` nondeterministic under monitoring. Add a test that seeks to S on a running plan and compares the audio with a fresh engine started at S, plus mutation checks that it fails when a node lies about its state.
   *Done when:* every node's reset render matches a fresh instance's, the key changes when any of the above changes, and the golden renders are unchanged.
2. **Automatic group targets.** Select top-level cacheable groups as targets, before the boundary stage. Key by subtree, start, tier, sample rate and `max_frames`; grow the entries; render on Stop or ahead of the playhead and swap in with `Setup.position`. Prioritise 0 and the loop start.
   *Done when:* Stop-Stop-Play on an unchanged project does no DSP inside the cached clusters; a fader move does not invalidate them; changing a node inside one track re-renders only that track.
3. **Tiers and cross-start reuse.** Pure entries valid for any start; bounded pre-roll from `S−n` with validity ranges; unbounded per-start entries; the tiered loop rule.
   *Done when:* a seek to a new start reuses the pure and bounded clusters and re-renders only the unbounded ones; a loop's reverb tail crosses the wrap while oscillator phase and voice state reset.
4. **Recursive partition.** Cut selection with group, join and fan-out cuts, the cost threshold, sink-first rendering and a sticky partition. Expensive-node and chain cuts, and section alignment, after the measured cost model.
   *Done when:* a drum rack or shared send is cached once and read twice; a cheap chain is not cached; moving or renaming a node does not invalidate the partition.
5. **Visibility and docs.** Settings toggle, size and eviction readout, a "from cache" indicator and an underrun counter. Add the playback section to `docs/architecture/caching.md` and a feature record when it lands.
   *Done when:* the user can see what is cached, turn it off, and tell when playback is live.

**Known limits.** Live branches and anything downstream stay live. Feedback loops are uncached. Pause→play is uncached. The first pass is always computed. A plugin that cannot return to a fresh state is unbounded or uncacheable. Cache entries are per machine, because transcendentals differ across platforms.

## Owed: needs a person on real hardware

- **Mac listening checks** for M0 to M2: a short `noodle play`, plus the batched
  macOS tests on the latest main. Never produce real audio there without asking.
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
- **Recording with a real microphone**, to check the latency compensation.
- **Stem separation on the real HT-Demucs weights** (M10), and listening to the
  stems. There is no Intel Mac ONNX Runtime build.
- **Warp evaluation:** CC0 clips for `eval/clips.csv` (`eval/fetch.sh --pin`),
  the IEEE DataPort download for `noodle-eval validate` and `fit`, and a
  listening check of the warp modes on the Mac.

## Deferred on purpose

- **M10 batch 3, Stem warp** (a warp mode that separates, warps each stem with its
  own mode and adds them back). Research (`research/stem-warp/findings.md` in the
  project folder) finds no published gain over whole-mix warping. Prerequisite:
  real mixes with vocals in `eval/clips.csv`, and a run on real weights. It ships
  only if the evaluation shows a clear gain on most songs and clips. Not started.
  Out of scope: streaming separation, models other than HT-Demucs, note-level
  warping.
- **M11 follow-ups:** more spectral nodes (pitch shift and time stretch on
  frames, spectral compressor, convolution reverb from an impulse response,
  noise-profile denoiser) and a choice of frame size (a config on Analyze that
  every spectral node on the wire adopts).
- **Piano roll and recording:** quantise and copy/paste in the piano roll, more
  than one MIDI port at a time, overdubbing into an existing clip, a count-in,
  keeping the raw take for re-quantising. The 128-frame engine block is fixed;
  it could follow the device period.
- **Virtual output:** remember the live-monitoring toggle between launches and
  retry the saved device when it returns.
- **VST3:** an editor window on Linux (the app makes none).
- **Plugin latency changes:** a plugin's reported latency change (CLAP `changed`) is not followed until the node is rebuilt.
- **Beat view gain envelope.** The beat view draws no gain envelope (clip or
  sample).
- **Flaky test** #138 stays open (no recurrence).
