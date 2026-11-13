# Tempo source and audio input monitoring

Delivered. Open work is in [ROADMAP.md](../ROADMAP.md), the design in [ARCHITECTURE.md](../ARCHITECTURE.md).

**Status:** tempo source done (#365, Link deferred). Input monitoring done except the cue mix send and the per-track recording channel (#345 stays open).

## Tempo source (#365)

The transport's tempo comes from the tempo map, from taps, or from an external MIDI clock. Tap and clock set a **live tempo** on the engine (`Controller::set_live_tempo`) that stands in for the map's tempos while it is set; the map's time signatures stay. It is session state, not a command: tapping adds nothing to the undo history and invalidates no cache keyed on the tempo map. The controller keeps the project's map apart from the live one: the timeline (playhead, seeks, loops, clip placement) always runs on the project's map (`timeline_map()`), so the playhead moves at the map's speed and what is under it is what plays, while `tempo_map()` is what nodes are told. The beat clock nodes see runs at the live tempo from where it was anchored, carries on unbroken when the tempo changes, and returns to the timeline's beat at a seek or loop. A change to the live tempo alone does not move the position. With a single tempo in the map, a tap sets that tempo in the map (one undo step) and no live tempo is made.

- **Transport bar:** a source menu (Map, Tap, MIDI clock), that also holds Tap tempo, and `T` (not while the computer keyboard is on, where `T` is a note). With more than one tempo in the map, the first tap switches the source to Tap and the third gives a tempo (the last eight taps, 20 to 400 bpm, forgotten after two seconds). While a live tempo is set the tempo field shows it, and "Write to map" turns it into a tempo change at the playhead (one `SetTempoMap`) and returns to Map.
- **MIDI clock in:** the MIDI input now keeps timing messages (`Ignore::SysexAndActiveSense`). They go to `MidiBus::clock_tap` and never to MIDI In nodes, which would drown in 24 a beat. `ClockFollower` locks to the first port that sends timing and keeps it until it is silent for a second. The tempo is the average over the last beat of pulses, published only when it moves by 0.1 bpm. Start seeks to the start and plays, Continue plays (from a Song Position Pointer if one came first), Stop stops.
- **Limits:** the beat phase is not locked, only the tempo, so the playhead can drift against a clock over a long time. The timeline and audio clips stay on the project's map and do not follow a live tempo; Tempo, Clock, the metronome and MIDI out do (so the metronome's beats can sit off the drawn grid while tapped). Tap is not yet a Learn MIDI target, because Learn MIDI only maps parameters. Ableton Link is not built.

Tests: `tempo_source.rs` (taps, clock estimator under jitter, start/stop/position, port lock), `midi.rs` (parsing, clock taps), the engine's live tempo, and an acceptance test that taps, follows a clock, and writes to the map.

## Input monitoring (#345)

The track input has a `monitor` parameter (Off, Auto, In; Auto by default) and an `input` config naming the device channels it listens to (`1`, or `3 4`; empty is 1 and 2, one channel is heard in both ears). Its `audio` output is the clips crossfaded (about 5 ms) to the live input when the input is heard: always for In, never for Off, and for Auto while the track is audio-armed and the transport is stopped, or recording inside the take's span (the old take plays in the pre-roll and after the punch-out). Everything after the track input, its effects and fader, applies to what is heard.

The audio thread writes each block of device input into the transport's `InputTap` (atomics, no lock) and the track input reads it; `ClipFeeds::set_input_tap` hands it the engine's tap when a stream opens. Offline renders have no tap, so they play clips only. A track that is monitoring is live (`ClipFeeds::is_live`), so the cache leaves it and what is downstream of it alone.

Not done: the cue mix (a send to a second Mix on the headphone channels) is not routed, and the recorder still writes the device input as one file for every armed track rather than each track's chosen channels.
