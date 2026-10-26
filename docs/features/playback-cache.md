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
  rule is the next batch). `Session::set_auto_cache` turns it off; the settings
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
