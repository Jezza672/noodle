//! S10: multi-articulation parts.
//!
//! Workaround. Nothing maps named articulations to keyswitches (#351), so
//! the composer places the library's keyswitch note by hand, below the
//! playable range, ahead of the phrase it changes. Playback starting
//! mid-cue would not chase the right switch, and renaming an articulation
//! or changing the library's map means moving the notes by hand.

use super::super::handle::{Handle, Probe};
use super::super::{LateChange, Route, Tag};
use super::roll::{BEAT, clip_of, hit, new_clip, one_track};

/// The keyswitch for staccato, and the pitch of the phrase.
const STACCATO: u8 = 24;
const PLAYED: u8 = 60;

fn steps(h: &mut Handle, _size: usize) {
    new_clip(h);
    h.step("place the staccato keyswitch before the phrase");
    hit(h, 0, STACCATO);
    h.step("play the phrase");
    hit(h, BEAT, PLAYED);
}

fn done(p: &mut Probe, _size: usize) -> Result<(), String> {
    let id = clip_of(p.project());
    let notes = p.roll_notes(id);
    let switch = notes.iter().find(|n| n.2 == STACCATO);
    let played = notes.iter().find(|n| n.2 == PLAYED);
    match (switch, played) {
        (Some(s), Some(n)) if s.0 < n.0 => Ok(()),
        _ => Err(format!(
            "the keyswitch does not come before the phrase: {notes:?}"
        )),
    }
}

fn another_switch(h: &mut Handle, _size: usize) {
    h.step("add the pizzicato keyswitch before a second phrase");
    hit(h, 2 * BEAT, STACCATO + 2);
    hit(h, 3 * BEAT, PLAYED);
}

fn two_switches(p: &mut Probe, _size: usize) -> Result<(), String> {
    let id = clip_of(p.project());
    let notes = p.roll_notes(id);
    let switches = notes.iter().filter(|n| n.2 < 36).count();
    if switches != 2 {
        return Err(format!("{switches} keyswitch notes, wanted 2"));
    }
    Ok(())
}

pub fn route() -> Route {
    Route {
        story: "S10",
        tag: Tag::Default,
        name: "a keyswitch note placed by hand",
        fixture: one_track,
        size: None,
        steps,
        done,
        late: &[LateChange {
            name: "switch articulation for a second phrase",
            steps: another_switch,
            done: two_switches,
            keep: &[],
        }],
    }
}
