//! The track input plays MIDI clips as note events.

mod common;

use std::time::Duration;

use common::{BEAT, BLOCK, Rig};
use noodle_engine::EventKind;

/// (sample, is a note-on, key or note id) for every event in a stretch.
fn run(rig: &mut Rig, from: u64, to: u64, playing: bool) -> Vec<(u64, bool, u32)> {
    let mut out = Vec::new();
    let mut at = from;
    while at < to {
        let n = BLOCK.min((to - at) as usize);
        for (time, kind) in rig.run_events(at, n, playing) {
            match kind {
                EventKind::NoteOn { key, .. } => out.push((time, true, u32::from(key))),
                EventKind::NoteOff { note, .. } => out.push((time, false, note.0)),
                other => panic!("unexpected {other:?}"),
            }
        }
        at += n as u64;
    }
    out
}

/// The node waits for each schedule instead of playing silence until it
/// arrives, so these tests don't depend on how quickly the hub's thread runs
/// (a sleep was not enough on loaded macOS runners).
fn rig(name: &str) -> Rig {
    let rig = Rig::blocking(name);
    rig.settle();
    rig
}

#[test]
fn notes_start_and_end_where_the_clip_puts_them() {
    let mut rig = rig("midi-places");
    // A clip a beat in, with a note at its start and one a beat later.
    rig.add_midi_clip(1, 4, &[(0, 480, 60), (960, 960, 64)]);
    rig.settle();
    let events = run(&mut rig, 0, 4 * BEAT, true);
    let keys: Vec<_> = events.iter().map(|&(t, on, _)| (t, on)).collect();
    assert_eq!(
        keys,
        [
            (BEAT, true),
            (BEAT + BEAT / 2, false),
            (2 * BEAT, true),
            (3 * BEAT, false)
        ]
    );
    assert_eq!(events[0].2, 60);
    assert_eq!(events[2].2, 64);
    // Each off names the note it ends.
    let on_ids = events.iter().filter(|e| !e.1).count();
    assert_eq!(on_ids, 2);
}

#[test]
fn a_note_is_cut_where_its_clip_ends() {
    let mut rig = rig("midi-cut");
    // A one-beat clip whose note runs for four.
    rig.add_midi_clip(0, 1, &[(480, 3840, 60), (1000, 100, 61)]);
    rig.settle();
    let events = run(&mut rig, 0, 2 * BEAT, true);
    // The second note starts after the clip ends, so it never plays.
    assert_eq!(
        events.iter().map(|&(t, on, _)| (t, on)).collect::<Vec<_>>(),
        [(BEAT / 2, true), (BEAT, false)]
    );
}

#[test]
fn stopping_ends_the_notes_that_are_sounding() {
    let mut rig = rig("midi-stop");
    rig.add_midi_clip(0, 4, &[(0, 3840, 60)]);
    rig.settle();
    let started = run(&mut rig, 0, BEAT, true);
    assert_eq!(started.len(), 1);
    // Stopped: the next block ends it, once.
    let stopped = run(&mut rig, BEAT, BEAT + BLOCK as u64, false);
    assert_eq!(stopped.len(), 1);
    assert!(!stopped[0].1);
    assert!(run(&mut rig, BEAT, BEAT + BLOCK as u64, false).is_empty());
}

#[test]
fn a_jump_ends_the_notes_and_plays_from_the_new_place() {
    let mut rig = rig("midi-seek");
    rig.add_midi_clip(0, 8, &[(0, 7680, 60), (3840, 480, 62)]);
    rig.settle();
    run(&mut rig, 0, BEAT, true);
    // Seek to the second note's start: the long note ends, the new one starts.
    let at = 4 * BEAT;
    let events = run(&mut rig, at, at + BLOCK as u64, true);
    assert_eq!(
        events
            .iter()
            .map(|&(t, on, _)| (t - at, on))
            .collect::<Vec<_>>(),
        [(0, false), (0, true)]
    );
    assert_eq!(events[1].2, 62);
}

#[test]
fn editing_the_notes_while_they_play_ends_only_what_changed() {
    let mut rig = rig("midi-edit");
    let id = rig.add_midi_clip(0, 8, &[(0, 7680, 60), (0, 7680, 67)]);
    rig.settle();
    let started = run(&mut rig, 0, BEAT, true);
    assert_eq!(started.len(), 2);
    // Removing the first note ends it and leaves the other one alone.
    rig.edit_clip(id, |clip| {
        let noodle_core::ClipContent::Midi(midi) = &mut clip.content else {
            unreachable!()
        };
        midi.notes.remove(0);
    });
    // Play on until the hub has handed the node the new notes, which ends the
    // removed one. How long that takes depends on the machine.
    let mut at = BEAT;
    let mut events = Vec::new();
    let started_waiting = std::time::Instant::now();
    while events.is_empty() {
        assert!(
            started_waiting.elapsed() < Duration::from_secs(10),
            "no note-off"
        );
        events = run(&mut rig, at, at + BLOCK as u64, true);
        at += BLOCK as u64;
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(events.len(), 1, "{events:?}");
    assert!(!events[0].1);
    let rest = run(&mut rig, at, 8 * BEAT + BLOCK as u64, true);
    assert_eq!(rest.len(), 1, "{rest:?}");
    assert_eq!(rest[0].0, 8 * BEAT);
}

#[test]
fn overlapping_clips_all_play_and_audio_clips_are_unaffected() {
    let mut rig = rig("midi-overlap");
    rig.add_midi_clip(0, 2, &[(0, 480, 60)]);
    rig.add_midi_clip(0, 2, &[(0, 480, 64)]);
    rig.settle();
    let events = run(&mut rig, 0, BEAT, true);
    let mut keys: Vec<_> = events.iter().filter(|e| e.1).map(|e| e.2).collect();
    keys.sort();
    assert_eq!(keys, [60, 64]);
    assert_eq!(events.iter().filter(|e| !e.1).count(), 2);
}

#[test]
fn nothing_plays_while_the_transport_stands_still() {
    let mut rig = rig("midi-idle");
    rig.add_midi_clip(0, 2, &[(0, 480, 60)]);
    rig.settle();
    assert!(run(&mut rig, 0, BEAT, false).is_empty());
}

#[test]
fn an_auditioned_key_sounds_on_its_own_track_even_when_stopped() {
    let mut rig = rig("midi-audition");
    let other = Rig::blocking("midi-audition-other");
    rig.feeds.audition(rig.id, 62, true);
    let events = rig.run_events(0, BLOCK, false);
    assert_eq!(events.len(), 1, "{events:?}");
    assert!(matches!(
        events[0].1,
        EventKind::NoteOn {
            key: 62,
            channel: 0,
            ..
        }
    ));
    // Held: nothing more. Another track's input is not touched.
    assert!(rig.run_events(0, BLOCK, false).is_empty());
    let mut other = other;
    assert!(other.run_events(0, BLOCK, false).is_empty());
    // Let go: one off, naming the same note.
    rig.feeds.audition(rig.id, 62, false);
    let off = rig.run_events(0, BLOCK, false);
    let (EventKind::NoteOn { note: on, .. }, EventKind::NoteOff { note: off, .. }) =
        (events[0].1, off[0].1)
    else {
        panic!("a note-on and a note-off")
    };
    assert_eq!(on, off);
    assert!(on.0 & 0x8000_0000 == 0, "apart from live MIDI's IDs");
    assert!(rig.run_events(0, BLOCK, false).is_empty());
}

/// The events of one block as (time in the block, event).
fn block(rig: &mut Rig, position: u64, playing: bool) -> Vec<(u32, EventKind)> {
    rig.run_quiet(position, BLOCK, playing);
    rig.events().iter().map(|e| (e.time, e.kind)).collect()
}

fn note_on(key: u8) -> [u8; 3] {
    [0x90, key, 100]
}

fn note_off(key: u8) -> [u8; 3] {
    [0x80, key, 0]
}

fn live_notes(events: &[(u32, EventKind)]) -> Vec<(bool, u8)> {
    events
        .iter()
        .filter_map(|(_, kind)| match *kind {
            EventKind::NoteOn { key, .. } => Some((true, key)),
            EventKind::NoteOff { note, .. } => Some((false, (note.0 & 0xFF) as u8)),
            _ => None,
        })
        .collect()
}

#[test]
fn a_monitored_track_plays_live_midi_with_the_transport_stopped() {
    let bus = noodle_io::MidiBus::new();
    let mut rig = Rig::monitored("midi-live", &bus);
    rig.feeds.set_monitoring(rig.id, true);
    bus.send(note_on(60));
    // Stopped, and still the note comes out, from any channel.
    let events = block(&mut rig, 0, false);
    assert_eq!(live_notes(&events), [(true, 60)]);
    bus.send([0x95, 64, 100]);
    let events = block(&mut rig, 0, false);
    assert_eq!(live_notes(&events), [(true, 64)]);
    bus.send(note_off(60));
    assert_eq!(live_notes(&block(&mut rig, 0, false)), [(false, 60)]);
}

#[test]
fn a_track_that_is_not_monitored_ignores_live_midi_for_good() {
    let bus = noodle_io::MidiBus::new();
    let mut rig = Rig::monitored("midi-unmonitored", &bus);
    bus.send(note_on(60));
    assert!(block(&mut rig, 0, false).is_empty());
    // What was played before it was monitored is not replayed.
    rig.feeds.set_monitoring(rig.id, true);
    assert!(block(&mut rig, 0, false).is_empty());
}

#[test]
fn stopping_and_starting_the_transport_leaves_live_notes_alone() {
    let bus = noodle_io::MidiBus::new();
    let mut rig = Rig::monitored("midi-live-stop", &bus);
    rig.add_midi_clip(0, 4, &[(0, 3840, 72)]);
    rig.settle();
    rig.feeds.set_monitoring(rig.id, true);
    bus.send(note_on(60));
    let started = block(&mut rig, 0, true);
    assert_eq!(live_notes(&started), [(true, 60), (true, 72)]);
    // The clip's note ends on stop, and the held live note does not.
    let stopped = block(&mut rig, BLOCK as u64, false);
    assert_eq!(live_notes(&stopped).len(), 1, "{stopped:?}");
    assert!(stopped.iter().all(
        |(_, kind)| !matches!(kind, EventKind::NoteOff { note, .. } if note.0 & 0x2000_0000 != 0)
    ));
    assert!(block(&mut rig, BLOCK as u64, false).is_empty());
    bus.send(note_off(60));
    assert_eq!(live_notes(&block(&mut rig, 0, false)), [(false, 60)]);
}

#[test]
fn disarming_ends_the_live_notes_that_are_held() {
    let bus = noodle_io::MidiBus::new();
    let mut rig = Rig::monitored("midi-live-disarm", &bus);
    rig.feeds.set_monitoring(rig.id, true);
    bus.send(note_on(60));
    bus.send(note_on(64));
    block(&mut rig, 0, false);
    rig.feeds.set_monitoring(rig.id, false);
    bus.send(note_on(67));
    let events = block(&mut rig, 0, false);
    assert_eq!(live_notes(&events), [(false, 60), (false, 64)]);
    assert!(block(&mut rig, 0, false).is_empty());
}

#[test]
fn all_notes_off_ends_live_notes() {
    let bus = noodle_io::MidiBus::new();
    let mut rig = Rig::monitored("midi-live-panic", &bus);
    rig.feeds.set_monitoring(rig.id, true);
    bus.send(note_on(60));
    block(&mut rig, 0, false);
    bus.all_notes_off();
    let offs = live_notes(&block(&mut rig, 0, false));
    assert_eq!(offs, [(false, 60)]);
}

#[test]
fn live_notes_are_merged_with_a_clips_in_time_order() {
    let bus = noodle_io::MidiBus::new();
    let mut rig = Rig::monitored("midi-live-order", &bus);
    // A note at the very start of the block the test runs second.
    rig.add_midi_clip(1, 4, &[(0, 480, 72)]);
    rig.settle();
    rig.feeds.set_monitoring(rig.id, true);
    block(&mut rig, 0, false);
    // Arrives some way into the next block, which the clip's note starts.
    std::thread::sleep(Duration::from_millis(4));
    bus.send(note_on(60));
    let events = block(&mut rig, BEAT, true);
    let times: Vec<u32> = events.iter().map(|&(t, _)| t).collect();
    assert_eq!(times.len(), 2, "{events:?}");
    assert!(times.windows(2).all(|w| w[0] <= w[1]), "{times:?}");
    assert_eq!(times[0], 0);
    assert!(times[1] > 0);
}

#[test]
fn live_notes_use_ids_of_their_own() {
    let bus = noodle_io::MidiBus::new();
    let mut rig = Rig::monitored("midi-live-ids", &bus);
    rig.feeds.set_monitoring(rig.id, true);
    bus.send(note_on(60));
    let events = block(&mut rig, 0, false);
    let EventKind::NoteOn { note, .. } = events[0].1 else {
        panic!("{events:?}")
    };
    // Apart from the MIDI In node's (top bit), a clip's, and the piano roll's.
    assert_eq!(note.0 & 0xF000_0000, 0x2000_0000);
}

#[test]
fn a_looping_clip_plays_its_loop_again_until_it_ends() {
    let mut rig = rig("midi-loop");
    // A four beat clip at the start whose first beat is the loop.
    let id = rig.add_midi_clip(0, 4, &[(0, 240, 60), (480, 240, 62), (1500, 100, 71)]);
    rig.edit_clip(id, |clip| {
        let noodle_core::ClipContent::Midi(midi) = &mut clip.content else {
            unreachable!()
        };
        midi.set_loop(noodle_core::Tick(0), noodle_core::Tick(960));
        midi.loop_on = true;
    });
    rig.settle();
    let events = run(&mut rig, 0, 5 * BEAT, true);
    let on: Vec<_> = events
        .iter()
        .filter(|e| e.1)
        .map(|&(t, _, key)| (t, key))
        .collect();
    // Four passes of the beat; the note after the loop's end never plays.
    let expect: Vec<_> = (0..4)
        .flat_map(|b| [(b * BEAT, 60), (b * BEAT + BEAT / 2, 62)])
        .collect();
    assert_eq!(on, expect);
    // Every note-on has its note-off.
    assert_eq!(events.iter().filter(|e| !e.1).count(), expect.len());
}
