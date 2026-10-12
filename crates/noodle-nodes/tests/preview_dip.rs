//! Switching the warp preview on or off while a warped clip plays dips the
//! sound to silence and back instead of cutting from one rendering of the
//! warp to the other.

mod common;

use common::{BLOCK, Rig};
use noodle_core::WarpMode;

/// Plays on from `at` for `blocks` and returns what came out (left channel)
/// and where it got. The rig is blocking, so the node waits for the hub and
/// the stretch worker itself and there is nothing to pace.
fn play(rig: &mut Rig, mut at: u64, blocks: usize) -> (Vec<f32>, u64) {
    let mut out = Vec::new();
    for _ in 0..blocks {
        let (left, _) = rig.run(at, BLOCK, true);
        out.extend(left);
        at += BLOCK as u64;
    }
    (out, at)
}

fn biggest_step(signal: &[f32]) -> f32 {
    signal
        .windows(2)
        .map(|w| (w[1] - w[0]).abs())
        .fold(0.0, f32::max)
}

#[test]
fn throwing_the_preview_switch_dips_instead_of_cutting() {
    let mut rig = Rig::blocking("preview-dip");
    // A tone riding on a steady 0.5, so any dip to silence stands out, and
    // it moves at most 0.02 a frame.
    rig.wav("tone.wav", 2, 400_000, |_, i| {
        0.5 + (i as f32 * 0.05).sin() * 0.3
    });
    let a = rig.add_clip(0, "tone.wav", 0, 300_000);
    // Faster than recorded, so Grain and Repitch are out of step.
    rig.warp_clip(a, |warp| {
        warp.mode = WarpMode::Grain;
        warp.tempo = noodle_core::TempoCurve::constant(90.0);
        warp.tempo_conversion = true;
    });
    rig.wait_ready(0, 1);
    let (before, mut at) = play(&mut rig, 0, 160);
    let mut tail = *before.last().unwrap();

    for on in [true, false] {
        rig.feeds.set_preview(on);
        let (mut switched, next) = play(&mut rig, at, 40);
        at = next;
        // Count the step from the last frame before the switch.
        switched.insert(0, tail);
        // The sound ran down to nothing and (once the other rendering was
        // ready) came back, without a jump.
        let quiet = switched.iter().position(|x| x.abs() < 0.05);
        assert!(quiet.is_some(), "preview {on}: it never dipped");
        assert!(
            biggest_step(&switched) < 0.05,
            "preview {on}: a step of {}",
            biggest_step(&switched)
        );
        let (settled, next) = play(&mut rig, at, 80);
        at = next;
        tail = *settled.last().unwrap();
        let peak = settled.iter().fold(0.0f32, |m, x| m.max(x.abs()));
        assert!(peak > 0.5, "preview {on}: it never came back ({peak})");
    }
}
