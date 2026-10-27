# Playback cache

The playback cache replays a Pass without doing DSP in cached clusters (ADR
0010, [../ROADMAP.md](../ROADMAP.md) for what is left). This file records what
landed, batch by batch. The design as it stands is in
[../architecture/caching.md](../architecture/caching.md).

## Batch 1: the invariant and the keys (#268)

No sound change; the golden renders are untouched.

- **`reset()` equals a fresh instance.** White Noise reseeds from `Setup::seed`;
  Reverb clears its wobble phase; the Arpeggiator rebuilds itself from its seed
  (its step, note ids and clock too); Key and Mono Note forget their note ids and
  last note; the Sampler's steal clock restarts (its stretched copies and worker
  link are resources, not timeline state); the plugin bridge restarts its steady
  clock and clears its cursors (the plugin's own reset is called as before);
  the cached-audio player restarts its stream at the playhead; the track input
  drops its sounding notes and flags (streams and windows stay);
  `ValueInput::settle` ends a parameter ramp on a seek.
- **Tests.** `reset_matches_fresh.rs` (every registered node type against a new
  instance, 1.5 s, three parameter settings, with notes and noise);
  `seek_matches_fresh.rs` (every example through the engine, seek against start);
  a runtime test for the parameter ramp; sampler, cached-player and plan unit
  tests. Each was broken on purpose and caught (see the PR).
- **`Layout::memory`.** `Memory::{None, Bounded(ms), Unbounded}`, default
  `Unbounded`; the node types are marked (the track input, gain, mix, maths, pan,
  the channel nodes and the views `None`; SVF, EQ, delay, reverb, dynamics, lag
  and follower `Bounded`). `analyze` composes it into `NodeAnalysis::memory`:
  a chain sums, parallel branches take the longest, a loop or an offline node is
  unbounded.
- **Keys.** `max_frames` is in every key; the render length only in offline
  nodes' keys. The track input keys `AudioClip.extent`, `ClipWarp.loop_region`,
  the MIDI clip loop fields and the warp preview. Plugin latency is already a
  function of what is keyed. A track that is being monitored or auditioned is
  uncacheable through `CacheEnv::live` (`ClipFeeds::is_live`), without turning
  off every track's cache.

## Batch 2: automatic group targets (#268, #270)

No sound change; the golden renders are untouched.

- **Targets.** `TargetKind::Auto(group)`: every top-level group that is not
  frozen and is cacheable whole, cut before its outputs' gain, mute and solo
  (`flatten::group_sources`). A fader move or a solo renders nothing; a node
  inside one track re-renders only that track (`freeze.rs` tests).
- **Pass-start keys and renders.** `pass_key(output_key, start)`; `Pass { tick,
  frame, frames }`; `Processor::start_at` and `render_taps(.., start, ..)` render
  from a fresh state at the start; `freeze_passes` / `spawn_freeze_passes` render
  the missing entries per pass. `CachedPlayer::starting_at` plays an entry from
  the pass's first frame.
- **Growing entries.** An entry is ready when it is at least as long as needed,
  and a render's length is rounded up to 15 s steps, so a lengthened project no
  longer renders a frozen real-time target again over the same key.
- **Session.** The pass starts at a seek (and where the playhead was when the
  stream opened); pause then play is the same pass. While a stream is open the
  entries for the playing pass and the hot starts (0 and the loop start) are
  rendered in the background and swapped in with `Setup.position`. A seek while
  another start's entries play recompiles at once; otherwise the cache is looked
  at after the debounce. A loop is served only from its start (the tiered loop
  rule is batch 3). `Session::set_auto_cache` turns it off; the settings
  toggle and the indicator come with the visibility batch.
- **Tests.** `freeze.rs` (per-start entries match a seek there, and a wrong
  start's entry does not; fader and mute leave the key alone; one group
  re-renders; uncacheable and nested and frozen groups are not targets; entries
  grow), `cached.rs` (an entry plays from its start), the `stop_stop_play` session
  test (Stop, Stop, Play does no render and plays from the cache; a fader move
  does not render; a seek is a new pass). Each was broken on purpose and caught
  (see the PR).
- **#270.** `a_closed_task_is_dropped_before_close_returns` raced: a worker that
  found a cancelled task removed its entry under the pool lock and dropped the
  task after releasing it, so `Handle::close` could see the entry gone and
  return before the drop. The entry now stays (marked running) until the task is
  dropped. A regression test keeps the pool busy while closing.

## Batch 3: tiers and reuse across starts (#268)

No sound change; the golden renders are untouched.

- **Tier-chosen entries.** `Analysis::memory(target)` is the longest memory of
  the cluster's outputs; `Analysis::entries(target, pass)` lists the entries
  that can serve a pass, best first (`EntrySpec`: origin, frames, loop).
  Position-pure clusters use one entry from 0 for every start; bounded clusters
  an entry from a bar-grid origin at or before `S − n` (or from 0); unbounded
  clusters an entry from S. `Freezer::plan` takes the first ready one;
  `freeze_passes` renders the missing ones, merging passes that want the same
  entry into one render for the longest.
- **The tiered loop rule.** `Pass::looping`; `render_taps` takes a loop and the
  engine wraps with state carried. Bounded clusters get a steady lap (warm-up
  laps rendered and dropped), so a tail crosses the wrap; unbounded clusters get
  a lap from the loop's start with the last 4 ms faded out, reset at the wrap.
  Pure clusters are served in any loop pass. The session no longer decides
  eligibility per pass: every pass before the loop's end is looked up, and each
  cluster is served or left live by its tier.
- **Lap by lap.** A bounded cluster in a loop is played from one entry per lap
  until steady: lap 1 from an ordinary entry (no tail from the loop's end that
  the pass has not played), laps 2 to `ceil(n / lap)` from entries rendered by
  the engine looping (a tail longer than the loop differs in each), then the
  steady lap. `CachedPlayer::then_laps` and `Lapped` count the wraps.
- **Tests.** `freeze.rs`: a pure cluster is one entry for every start, a bounded
  cluster carries the clip from before a seek and serves a range of starts on
  the bar grid, an unbounded one only its own start, and the loop tests (a 0.1 s
  delay carries its tail across the wrap, every lap equal to a live loop, also
  when the tail is longer than the loop and each lap has its own entry; an oscillator resets and fades at the wrap); a session test for a loop.
  Each was broken on purpose and caught (pure origin, warm-up laps, fade, grid, wrap counting, the transient laps, the lap key); `realtime.rs` runs a lapped player with no allocation.

## Batch 4: the recursive partition (#268)

No sound change; the golden renders are untouched.

- **Cuts.** `TargetKind::Cut(node)`, picked in `cache/partition.rs`: each branch
  that feeds a join and each output that two nodes read, besides the top-level
  groups. A candidate is kept when its cost, the sum of `Layout::cost` over what
  it would render that no earlier cut stores, reaches `DEFAULT_MIN_COST`. A cheap
  chain is not cached, a cheap group is dropped and the worthwhile clusters in it
  are cut on their own, and a send or a rack that two nodes read is one entry
  read twice. A group's stage is not a join.
- **Costs.** `Layout::cost` (`#[ports(cost = n)]`, default 1): reverb 120,
  plugins 200, sampler, script 60, chorus, phaser 40, voices 30, clip player 24,
  ladder 12. A prior, to be replaced by measurement.
- **Sink-first rendering.** Explicit freezes and offline nodes render first;
  the automatic targets then render from the sink back, and a render that
  several targets want (the same origin and loop) writes all their entries in one
  run (`FreezeReport::renders`).
- **Sticky.** The partition depends on wiring, types, configs and ids only, so
  moving a node or reordering its ports changes no cut and no key. A measured
  cost model, expensive-node and chain cuts, and section alignment are left.
- **Tests.** `cache/tests.rs` (shared source, cheap chain, join branches, cost
  that leaves out stored cuts, move leaves the partition alone) and `freeze.rs`
  (a shared send is one entry and one player; a cheap chain and a cheap group;
  a group's render writes the cut inside it, then an edit after the cut renders
  only the group). Each was broken on purpose and caught (batching, order,
  threshold, stored cuts, fan-out, join).
- **Found on the way.** The reverb's wobble phase advances per block, so its
  output depends on block alignment and a cached reverb differs from live by
  about 0.5%. The tests use a chorus; see #273.
