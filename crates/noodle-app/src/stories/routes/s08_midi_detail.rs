//! S08: edit MIDI performance detail.
//!
//! Partial. The piano roll draws and moves notes, and Ctrl+A with the arrow
//! keys transposes a selection. There is no humanise (#350), so velocities
//! and timing are not randomised by an amount that could be halved later.
//! Recording the piano part and redrawing the sustain pedal's lane are not
//! routed.

use super::super::handle::{Handle, Probe};
use super::super::{LateChange, Route, Tag};
use super::roll::{BEAT, clip_of, hit, new_clip, one_track, select};
use egui::Key;

const KEYS: [u8; 4] = [60, 64, 67, 72];

fn steps(h: &mut Handle, _size: usize) {
    new_clip(h);
    h.step("play in four notes");
    for (i, key) in KEYS.iter().enumerate() {
        hit(h, i as i64 * BEAT, *key);
    }
}

fn keys(p: &mut Probe) -> Vec<u8> {
    let id = clip_of(p.project());
    let mut keys: Vec<u8> = p.roll_notes(id).iter().map(|n| n.2).collect();
    keys.sort();
    keys
}

fn done(p: &mut Probe, _size: usize) -> Result<(), String> {
    let got = keys(p);
    if got != KEYS {
        return Err(format!("the part is {got:?}, wanted {KEYS:?}"));
    }
    Ok(())
}

fn down_a_tone(h: &mut Handle, _size: usize) {
    h.step("select the part and move it down a tone");
    let part: Vec<(i64, u8)> = KEYS
        .iter()
        .enumerate()
        .map(|(i, k)| (i as i64 * BEAT, *k))
        .collect();
    select(h, &part);
    let at = h.probe().roll_grid().expect("the roll").center();
    h.press(at, egui::Modifiers::NONE, Key::ArrowDown);
    h.press(at, egui::Modifiers::NONE, Key::ArrowDown);
}

fn lowered(p: &mut Probe, _size: usize) -> Result<(), String> {
    let got = keys(p);
    let want: Vec<u8> = KEYS.iter().map(|k| k - 2).collect();
    if got != want {
        return Err(format!("the part is {got:?}, wanted {want:?}"));
    }
    Ok(())
}

pub fn route() -> Route {
    Route {
        story: "S08",
        tag: Tag::Default,
        name: "play in a part and transpose it down a tone",
        fixture: one_track,
        size: None,
        steps,
        done,
        late: &[LateChange {
            name: "transpose the part down a tone",
            steps: down_a_tone,
            done: lowered,
            keep: &[],
        }],
    }
}
