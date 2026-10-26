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
