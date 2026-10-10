//! The arithmetic of editing notes: what each gesture does to a clip's notes.
//! Every function takes the notes as they were when the gesture began and
//! returns the notes it makes, so snapping can't accumulate error over a
//! drag. Positions are ticks from the clip's start.

use std::collections::BTreeSet;

use noodle_core::{MidiNote, Tick};

/// Rounds `tick` to the nearest multiple of `step` (1 or less is free).
pub fn snap(tick: i64, step: i64) -> i64 {
    if step <= 1 {
        tick
    } else {
        (tick + step / 2).div_euclid(step) * step
    }
}

/// The selected notes moved by `d_tick` ticks and `d_key` keys. The group
/// stops at the clip's start and end, and at the ends of the keyboard, so
/// it keeps its shape.
pub fn moved(
    original: &[MidiNote],
    selected: &BTreeSet<u32>,
    d_tick: i64,
    d_key: i32,
    clip_length: i64,
) -> Vec<MidiNote> {
    // A note that starts past the clip's end can't be moved or placed.
    let chosen = || {
        original
            .iter()
            .filter(|n| selected.contains(&n.id) && n.start.0 < clip_length)
    };
    let earliest = chosen().map(|n| n.start.0).min().unwrap_or(0);
    let latest = chosen().map(|n| n.start.0).max().unwrap_or(0);
    let d_tick = d_tick.clamp(-earliest, (clip_length - 1 - latest).max(-earliest));
    let low = chosen().map(|n| i32::from(n.key)).min().unwrap_or(0);
    let high = chosen().map(|n| i32::from(n.key)).max().unwrap_or(0);
    let d_key = d_key.clamp(-low, 127 - high);
    original
        .iter()
        .map(|note| {
            if !selected.contains(&note.id) || note.start.0 >= clip_length {
                return *note;
            }
            MidiNote {
                start: Tick(note.start.0 + d_tick),
                key: (i32::from(note.key) + d_key) as u8,
                ..*note
            }
        })
        .collect()
}

/// The selected notes with the end of `grabbed` dragged to `end`: each is
/// lengthened or shortened by the same amount, down to `min` ticks and up
/// to the end of the clip.
pub fn resized(
    original: &[MidiNote],
    selected: &BTreeSet<u32>,
    grabbed: usize,
    end: i64,
    min: i64,
    clip_length: i64,
) -> Vec<MidiNote> {
    let Some(grabbed) = original.get(grabbed) else {
        return original.to_vec();
    };
    let delta = end - grabbed.end().0;
    original
        .iter()
        .map(|note| {
            if !selected.contains(&note.id) || note.start.0 >= clip_length {
                return *note;
            }
            let length = (note.length.0 + delta)
                .min(clip_length - note.start.0)
                .max(min.min(clip_length - note.start.0));
            MidiNote {
                length: Tick(length),
                ..*note
            }
        })
        .collect()
}

/// `notes` with one more at `start` on `key`, `length` long but not past the
/// end of the clip, and its index. `None` if `start` is outside the clip.
pub fn added(
    notes: &[MidiNote],
    start: i64,
    length: i64,
    key: u8,
    velocity: f32,
    clip_length: i64,
) -> Option<(Vec<MidiNote>, u32)> {
    if !(0..clip_length).contains(&start) || key > 127 {
        return None;
    }
    let length = length.min(clip_length - start).max(1);
    let mut notes = notes.to_vec();
    let id = notes.iter().map(|n| n.id).max().map_or(0, |m| m + 1);
    notes.push(MidiNote {
        id,
        start: Tick(start),
        length: Tick(length),
        key,
        velocity: velocity.clamp(0.0, 1.0),
    });
    Some((notes, id))
}

/// `notes` without the selected ones.
pub fn removed(notes: &[MidiNote], selected: &BTreeSet<u32>) -> Vec<MidiNote> {
    notes
        .iter()
        .filter(|note| !selected.contains(&note.id))
        .copied()
        .collect()
}

/// `notes` with note `index`'s velocity set.
pub fn with_velocity(notes: &[MidiNote], index: usize, velocity: f32) -> Vec<MidiNote> {
    let mut notes = notes.to_vec();
    if let Some(note) = notes.get_mut(index) {
        note.velocity = velocity.clamp(0.0, 1.0);
    }
    notes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(start: i64, length: i64, key: u8) -> MidiNote {
        MidiNote::new(Tick(start), Tick(length), key)
    }

    /// `notes` with ids 0, 1, 2...
    fn ided<const N: usize>(mut notes: [MidiNote; N]) -> [MidiNote; N] {
        for (i, note) in notes.iter_mut().enumerate() {
            note.id = i as u32;
        }
        notes
    }

    fn all(n: u32) -> BTreeSet<u32> {
        (0..n).collect()
    }

    #[test]
    fn snapping_rounds_to_the_nearest_step() {
        assert_eq!(snap(0, 240), 0);
        assert_eq!(snap(119, 240), 0);
        assert_eq!(snap(120, 240), 240);
        assert_eq!(snap(-130, 240), -240);
        assert_eq!(snap(133, 1), 133);
        assert_eq!(snap(133, 0), 133);
    }

    #[test]
    fn moving_keeps_the_group_inside_the_clip_and_the_keyboard() {
        let notes = ided([note(240, 240, 60), note(960, 480, 72), note(500, 100, 40)]);
        let two = BTreeSet::from([0, 1]);
        let moved_ = moved(&notes, &two, 480, 2, 3840);
        assert_eq!(moved_[0], note(720, 240, 62));
        assert_eq!(
            moved_[1],
            MidiNote {
                id: 1,
                ..note(1440, 480, 74)
            }
        );
        assert_eq!(moved_[2], notes[2], "unselected notes stay");
        // Stops at the clip's start, keeping the spacing.
        let moved_ = moved(&notes, &two, -5000, 0, 3840);
        assert_eq!((moved_[0].start.0, moved_[1].start.0), (0, 720));
        // And at its end, by where the latest note starts.
        let moved_ = moved(&notes, &two, 9999, 0, 3840);
        assert_eq!(moved_[1].start.0, 3839);
        assert_eq!(moved_[0].start.0, 3839 - 720);
        // The top of the keyboard.
        let moved_ = moved(&notes, &two, 0, 99, 3840);
        assert_eq!((moved_[0].key, moved_[1].key), (115, 127));
        let moved_ = moved(&notes, &two, 0, -99, 3840);
        assert_eq!((moved_[0].key, moved_[1].key), (0, 12));
        // An empty selection changes nothing.
        assert_eq!(moved(&notes, &BTreeSet::new(), 100, 1, 3840), notes);
    }

    #[test]
    fn resizing_changes_every_selected_note_by_the_same_amount() {
        let notes = ided([note(0, 240, 60), note(480, 480, 62), note(960, 240, 64)]);
        let first_two = BTreeSet::from([0, 1]);
        // Dragging the first note's end from 240 to 480 adds 240 to both.
        let out = resized(&notes, &first_two, 0, 480, 60, 3840);
        assert_eq!(out[0].length.0, 480);
        assert_eq!(out[1].length.0, 720);
        assert_eq!(out[2], notes[2]);
        // Can't go below the minimum, or past the clip's end.
        let out = resized(&notes, &first_two, 0, -900, 60, 3840);
        assert_eq!((out[0].length.0, out[1].length.0), (60, 60));
        let out = resized(&notes, &first_two, 1, 9999, 60, 3840);
        assert_eq!((out[0].length.0, out[1].length.0), (3840, 3360));
        // A grabbed note that isn't there changes nothing.
        assert_eq!(resized(&notes, &first_two, 9, 0, 60, 3840), notes);
    }

    #[test]
    fn notes_past_the_clips_end_are_left_alone() {
        // Left over from before the clip was trimmed.
        let notes = ided([note(100, 100, 60), note(5000, 240, 64)]);
        let both = all(2);
        let moved_ = moved(&notes, &both, 300, 1, 3840);
        assert_eq!(moved_[0], note(400, 100, 61));
        assert_eq!(moved_[1], notes[1]);
        let resized_ = resized(&notes, &both, 0, 500, 60, 3840);
        assert_eq!(resized_[0].length.0, 400);
        assert_eq!(resized_[1], notes[1]);
        // Nothing to move at all: the selection is only the stray note.
        assert_eq!(moved(&notes, &BTreeSet::from([1]), 300, 1, 3840), notes);
    }

    #[test]
    fn adding_and_removing_notes() {
        let notes = ided([note(0, 240, 60)]);
        let (out, index) = added(&notes, 3700, 480, 64, 0.5, 3840).unwrap();
        assert_eq!(index, 1, "the next id after the notes'");
        assert_eq!(out[1].length.0, 140, "cut at the clip's end");
        assert_eq!(out[1].velocity, 0.5);
        assert!(added(&notes, 3840, 480, 64, 0.5, 3840).is_none());
        assert!(added(&notes, -1, 480, 64, 0.5, 3840).is_none());
        assert!(added(&notes, 0, 480, 128, 0.5, 3840).is_none());
        assert_eq!(removed(&out, &BTreeSet::from([0])), [out[1]]);
        assert_eq!(removed(&out, &all(2)), []);
    }

    #[test]
    fn velocity_is_clamped() {
        let notes = ided([note(0, 240, 60)]);
        assert_eq!(with_velocity(&notes, 0, 1.7)[0].velocity, 1.0);
        assert_eq!(with_velocity(&notes, 0, -1.0)[0].velocity, 0.0);
        assert_eq!(with_velocity(&notes, 3, 0.1), notes);
    }
}
