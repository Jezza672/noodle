# Low-latency monitoring (#548)

Delivered, in three chunks. What is owed on hardware is in [ROADMAP.md](../ROADMAP.md), the design in [../architecture/transport-and-devices.md](../architecture/transport-and-devices.md) and, for the third chunk, [../architecture/caching.md](../architecture/caching.md) ("Rendered ahead") and [ADR 0014](../adr/0014-a-render-is-played-while-it-fills.md).

**What it does.** While a track is armed or monitored, what the player hears of their own playing is not delayed by anything but its own path, and the device buffer is as small as the hardware allows. One setting, **Monitoring latency** (Auto, Always low, Off), on the Audio tab of Settings. The engine block stays 128, so cache entries stay valid; only the device buffer moves.

## 1. The live cone is left out of delay compensation

`Controller::set_live_nodes` names the nodes played live; the session passes the track inputs that are monitored (`ClipFeeds::monitored_nodes`: MIDI-armed, `monitor` In, or Auto and audio-armed; a held key of the keyboard is not counted, it comes and goes too fast to remake a plan for). `plan::compensate` follows what they feed (signal, spectral and event wires) as the **live cone** and does two things differently:

- A cone input is never given a delay line. Where the cone meets other paths (a mix), only the others are lined up with each other, and with the sink.
- Two lateness figures travel along a wire in the cone: the live one (the cone's own path) and the rest (what the other paths mixed in came to). The live one is `PlanInfo::monitored_latency` at the sinks, shown as `LatencyReport::monitored`; `output_latency`, which recording and offline renders use, is still the others'.

Limits: the cone's part of a mix is not delayed, so after a mix the live material is early against the others by design; a node in the cone with an input from outside it is still lined up for that input. Nothing is in the cone for offline renders, so they line up as before. The live set changes only while the transport is stopped and no take runs (like the buffer), so arming mid-playback never moves a track in time under a click; the monitored path stays aligned until the next stop. An armed track's own clips are in the cone too, so they skip compensation while it is monitored: during playback an armed track plays earlier than the others by their plugin latency. Arming remakes the plan (the set changed), which fades like any other rewiring when delay lines come or go.

Tests: `crates/noodle-nodes/tests/pdc.rs` (`the_live_cone_is_not_held_back_to_match_other_tracks`, `what_follows_the_cone_stays_in_it_and_the_others_still_line_up`); without the skip for cone inputs both fail.

## 2. The device buffer follows arming

`AudioConfig::live_buffer` (never saved) is the size asked for in place of `LIVE_BUFFER` when `buffer_size` is `None`. Like the automatic size it is moved into the device's span, and a stream that refuses it is opened again leaving the size to the device. `Session` sets it when it opens a stream, from `latency::wanted_tier`:

| Tier | Frames | When |
|---|---|---|
| Low | 64 | Auto: a track is armed or monitored. Always low: always |
| Normal | the usual 256 | a stream opens with nothing live |
| Relaxed | 1024 | Auto: nothing armed, monitored or playing for 10 s |

A held key of the keyboard counts as live for the buffer (it wakes a relaxed buffer and keeps the idle clock from running), though not for the live cone. The buffer follows arming one way: once low it stays low until it has idled long enough to relax, and a relaxed one goes back to low and never to normal, so a session reopens the stream rarely. `Session::maintain` reopens the stream only when the transport is stopped and no take, count-in or pre-roll is running. Arming while playing waits for the stop. A size chosen in the audio settings is never changed (the setting then has no say), and Off leaves everything alone. The setting is a preference (`Prefs::monitoring_latency`), saved with the others and exported with them.

The Audio tab shows the buffer's state and the monitored path's plugin latency (`latency::monitoring_readout`).

Tests: the session tests `arming_a_track_while_stopped_reopens_the_stream_with_a_small_buffer`, `the_buffer_relaxes_when_idle_and_wakes_when_armed`, `the_stream_is_not_reopened_while_the_transport_runs`, `a_chosen_buffer_size_and_the_off_setting_leave_the_buffer_alone`, `always_low_opens_small_from_the_start` (on the virtual output, whose buffer follows `live_buffer`), and the policy table in `latency.rs`.

**Owed on the Mac:** the gap when arming reopens the stream, and a power measurement of the relaxed buffer. Ask before making sound.

## 3. Only the live cone is left to the callback: rendering ahead

A cluster with no whole entry used to play live in the callback until the background render finished. The render runs many times faster than real time, so the callback now plays from the entry while it is still being written, as soon as the render is two seconds ahead of the playhead (ADR 0014).

- `noodle-io`: `CacheWriter::publish` (done every 4096 frames by `write`) makes what has been written readable; `CacheStore::get_growing` opens the entry as it fills, `CachedAudio::available` is what can be read now, and `Growth` says whether the render finished or was abandoned. The commit renames the file with readers holding it open, so a player started on a growing entry carries on into the whole one.
- `CachedPlayer::growing`: the worker makes a chunk only once the render has written past its end, so a chunk is never short or stale. Its config is the same as a whole entry's, so the render finishing swaps nothing.
- `Freezer::plan_at` takes an `Ahead` (the playhead, the entries played now and those given up) and plays a single non-looping entry as `TargetState::Streaming` when the render is `AHEAD_JOIN_SECONDS` (2 s) ahead, down to `AHEAD_KEEP_SECONDS` (0.4 s). `FreezePlan::watches` lists the renders it looked at.
- `Freezing::poll(playhead, playing)` watches those leads and has the session compile again when one is made or lost. A cluster given up stays live until its render ends or a new render starts, so a render slower than the playhead doesn't flap.
- An edit changes the key: the old entry comes off at once and the cluster plays live, so a tweak is heard immediately, and it goes back to the new entry once the new render is ahead.
- Monitoring latency Off keeps the old rule; Auto and Always low play ahead. The toolbar's cache status is **Ahead** (`CacheStatus::Ahead`) while a cluster is played this way.

Tests: the writer and reader (`noodle-io` `cache.rs`), the player on a growing entry (`cached.rs`), the plan's decisions and that a streamed entry sounds like the live one (`tests/freeze.rs`: `a_cluster_plays_from_an_entry_still_being_written_once_the_render_is_ahead`, `an_abandoned_render_is_not_played`), and the lead watching (`freezing.rs`).

Not done: loop laps are not played ahead (they wait for their renders), and the callback's per-block saving on a 60-track project has not been measured. Owed on the Mac: listen to the hand-over.
