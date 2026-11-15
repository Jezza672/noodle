# Low-latency monitoring, chunks 1 and 2 (#548)

Delivered. Open work (chunk 3, only the live cone running in the device callback) is in [ROADMAP.md](../ROADMAP.md), the design in [../architecture/transport-and-devices.md](../architecture/transport-and-devices.md).

**What it does.** While a track is armed or monitored, what the player hears of their own playing is not delayed by anything but its own path, and the device buffer is as small as the hardware allows. One setting, **Monitoring latency** (Auto, Always low, Off), on the Audio tab of Settings. The engine block stays 128, so cache entries stay valid; only the device buffer moves.

## 1. The live cone is left out of delay compensation

`Controller::set_live_nodes` names the nodes played live; the session passes the track inputs that are monitored (`ClipFeeds::monitored_nodes`: MIDI-armed, `monitor` In, or Auto and audio-armed; a held key of the keyboard is not counted, it comes and goes too fast to remake a plan for). `plan::compensate` follows what they feed (signal, spectral and event wires) as the **live cone** and does two things differently:

- A cone input is never given a delay line. Where the cone meets other paths (a mix), only the others are lined up with each other, and with the sink.
- Two lateness figures travel along a wire in the cone: the live one (the cone's own path) and the rest (what the other paths mixed in came to). The live one is `PlanInfo::monitored_latency` at the sinks, shown as `LatencyReport::monitored`; `output_latency`, which recording and offline renders use, is still the others'.

Limits: the cone's part of a mix is not delayed, so after a mix the live material is early against the others by design; a node in the cone with an input from outside it is still lined up for that input. Nothing is in the cone for offline renders, so they line up as before. Arming remakes the plan (the set changed), which fades like any other rewiring when delay lines come or go.

Tests: `crates/noodle-nodes/tests/pdc.rs` (`the_live_cone_is_not_held_back_to_match_other_tracks`, `what_follows_the_cone_stays_in_it_and_the_others_still_line_up`); without the skip for cone inputs both fail.

## 2. The device buffer follows arming

`AudioConfig::live_buffer` (never saved) is the size asked for in place of `LIVE_BUFFER` when `buffer_size` is `None`. Like the automatic size it is moved into the device's span, and a stream that refuses it is opened again leaving the size to the device. `Session` sets it when it opens a stream, from `latency::wanted_tier`:

| Tier | Frames | When |
|---|---|---|
| Low | 64 | Auto: a track is armed or monitored. Always low: always |
| Normal | the usual 256 | a stream opens with nothing live |
| Relaxed | 1024 | Auto: nothing armed, monitored or playing for 10 s |

The buffer follows arming one way: once low it stays low until it has idled long enough to relax, and a relaxed one goes back to low and never to normal, so a session reopens the stream rarely. `Session::maintain` reopens the stream only when the transport is stopped and no take, count-in or pre-roll is running. Arming while playing waits for the stop. A size chosen in the audio settings is never changed (the setting then has no say), and Off leaves everything alone. The setting is a preference (`Prefs::monitoring_latency`), saved with the others and exported with them.

The Audio tab shows the buffer's state and the monitored path's plugin latency (`latency::monitoring_readout`).

Tests: the session tests `arming_a_track_while_stopped_reopens_the_stream_with_a_small_buffer`, `the_buffer_relaxes_when_idle_and_wakes_when_armed`, `the_stream_is_not_reopened_while_the_transport_runs`, `a_chosen_buffer_size_and_the_off_setting_leave_the_buffer_alone`, `always_low_opens_small_from_the_start` (on the virtual output, whose buffer follows `live_buffer`), and the policy table in `latency.rs`.

**Owed on the Mac:** the gap when arming reopens the stream, and a power measurement of the relaxed buffer. Ask before making sound.
