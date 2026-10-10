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
    let other = Rig::new("midi-audition-other");
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
